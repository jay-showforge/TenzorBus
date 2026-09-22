"""Phase 3 acceptance: the same transport invariants, through the Rust ring.

These mirror the v0.1 reference tests in `test_ring.py` one for one, against the
Rust production ring via its Python bindings, plus the lifetime rule the
reference could only document. Build the extension with `scripts/build_rust.sh`;
without it the whole module skips rather than failing.
"""

from __future__ import annotations

import multiprocessing as mp
import os
import shutil
import subprocess
import sys
import time
import unittest

import numpy as np

try:
    import tenzorbus_rs as tzb
except ImportError:  # pragma: no cover - extension not built
    tzb = None

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LAB = os.path.join(REPO, "rust", "target", "release", "tenzorbus-lab")


def ring_name(tag: str) -> str:
    return f"pyb_{tag}_{os.getpid()}_{time.monotonic_ns() % 1_000_000}"


@unittest.skipIf(tzb is None, "tenzorbus_rs extension not built; run scripts/build_rust.sh")
class RustRingTests(unittest.TestCase):
    def setUp(self):
        self.rings = []

    def tearDown(self):
        for name in self.rings:
            try:
                tzb.unlink(name)
            except Exception:
                pass

    def make_ring(self, tag, slots=8, slot_bytes=1 << 20):
        name = ring_name(tag)
        self.rings.append(name)
        return tzb.create(name, slots=slots, slot_bytes=slot_bytes)

    def test_zero_copy_numpy_view_aliases_the_slot(self):
        bus = self.make_ring("numpy")
        producer = bus.producer()
        consumer = bus.consumer()
        source = np.arange(24, dtype=np.float32).reshape(2, 3, 4)
        producer.publish(source)

        with consumer.next(timeout=1.0) as lease:
            view = lease.numpy()
            self.assertEqual(view.shape, source.shape)
            self.assertEqual(view.dtype, source.dtype)
            np.testing.assert_array_equal(view, source)
            # The decisive check: the array's data pointer is the slot itself.
            self.assertEqual(view.__array_interface__["data"][0], lease.data_address)
            del view

    def test_zero_copy_torch_view_aliases_the_slot(self):
        try:
            import torch
        except ImportError:
            self.skipTest("torch not installed")
        bus = self.make_ring("torch")
        producer = bus.producer()
        consumer = bus.consumer()
        source = np.arange(16, dtype=np.float32).reshape(4, 4)
        producer.publish(source)

        with consumer.next(timeout=1.0) as lease:
            tensor = lease.torch()
            self.assertEqual(tuple(tensor.shape), source.shape)
            self.assertTrue(torch.equal(tensor, torch.from_numpy(source)))
            self.assertEqual(tensor.data_ptr(), lease.data_address)
            del tensor

    def test_bounded_ring_does_not_overwrite_an_active_reader(self):
        bus = self.make_ring("backpressure", slots=2, slot_bytes=4096)
        producer = bus.producer()
        consumer = bus.consumer()
        first = np.full(16, 1, dtype=np.int32)
        producer.publish(first)
        producer.publish(np.full(16, 2, dtype=np.int32))

        lease = consumer.next(timeout=1.0)
        held = lease.copy()
        np.testing.assert_array_equal(held, first)

        # Both slots carry this consumer's claim, so the ring must refuse.
        with self.assertRaises(RuntimeError):
            producer.publish(np.full(16, 3, dtype=np.int32), timeout=0.2)
        np.testing.assert_array_equal(lease.numpy(), first)
        lease.release()

        consumer.next(timeout=1.0).release()
        self.assertIsNotNone(producer.publish(np.full(16, 3, dtype=np.int32), timeout=1.0))

    def test_view_cannot_outlive_its_lease(self):
        """The v0.1 lifetime bug, now structurally impossible.

        In the reference, a NumPy view could outlive its lease and then alias a
        recycled slot. Here the view holds a buffer export and release refuses
        until it is dropped.
        """
        bus = self.make_ring("lifetime", slots=2, slot_bytes=4096)
        producer = bus.producer()
        consumer = bus.consumer()
        producer.publish(np.arange(8, dtype=np.int32))

        lease = consumer.next(timeout=1.0)
        view = lease.numpy()
        with self.assertRaises(BufferError):
            lease.release()
        del view
        lease.release()  # now allowed

    def test_copy_outlives_the_lease(self):
        bus = self.make_ring("copy", slots=2, slot_bytes=4096)
        producer = bus.producer()
        consumer = bus.consumer()
        source = np.arange(8, dtype=np.int32)
        producer.publish(source)
        with consumer.next(timeout=1.0) as lease:
            kept = lease.copy()
        np.testing.assert_array_equal(kept, source)

    def test_multi_consumer_same_publication_across_processes(self):
        bus = self.make_ring("fanout", slots=8, slot_bytes=1 << 16)
        producer = bus.producer()
        ctx = mp.get_context("spawn")
        results = ctx.Queue()
        procs = [
            ctx.Process(target=_consumer_child, args=(bus.name, results))
            for _ in range(3)
        ]
        for p in procs:
            p.start()
        deadline = time.monotonic() + 20
        while bus.stats()["consumers"] < 3:
            self.assertLess(time.monotonic(), deadline, "consumers never registered")
            time.sleep(0.02)

        source = np.arange(64, dtype=np.int32)
        published = producer.publish(source, timeout=5.0)
        self.assertEqual(published["readers"], 3)

        seen = [results.get(timeout=20) for _ in procs]
        for p in procs:
            p.join(timeout=20)
        for entry in seen:
            self.assertEqual(entry["sequence"], published["sequence"])
            self.assertEqual(entry["slot_index"], published["slot_index"])
            self.assertEqual(entry["checksum"], int(source.sum()))

    def test_drop_newest_reports_the_drop(self):
        bus = self.make_ring("drop", slots=2, slot_bytes=4096)
        producer = bus.producer()
        consumer = bus.consumer()
        producer.publish(np.arange(8, dtype=np.int32))
        producer.publish(np.arange(8, dtype=np.int32))
        held = [consumer.next(timeout=1.0), consumer.next(timeout=1.0)]
        self.assertIsNone(
            producer.publish(np.arange(8, dtype=np.int32), policy="drop_newest")
        )
        self.assertEqual(bus.stats()["dropped"], 1)
        for lease in held:
            lease.release()

    def test_every_supported_dtype_round_trips(self):
        bus = self.make_ring("dtypes", slots=8, slot_bytes=1 << 16)
        producer = bus.producer()
        consumer = bus.consumer()
        for dtype in ["float32", "float16", "uint8", "int8", "int16",
                      "int32", "int64", "float64", "bool"]:
            with self.subTest(dtype=dtype):
                source = np.ones((3, 5), dtype=np.dtype(dtype))
                producer.publish(source)
                with consumer.next(timeout=1.0) as lease:
                    self.assertEqual(lease.dtype, dtype)
                    np.testing.assert_array_equal(lease.numpy(), source)

    def test_direct_write_uses_one_buffer_end_to_end(self):
        """Phase 4B: the producer writes into the slot the consumer reads."""
        bus = self.make_ring("direct")
        producer = bus.producer()
        consumer = bus.consumer()

        with producer.reserve("float32", (3, 16, 16)) as writer:
            slot_address = writer.data_address
            writer.set_timestamp_ns(1_234_000_000)
            frame = writer.numpy()
            self.assertEqual(frame.shape, (3, 16, 16))
            self.assertEqual(frame.__array_interface__["data"][0], slot_address)
            frame[:] = np.arange(frame.size, dtype=np.float32).reshape(frame.shape)
            expected = frame.copy()
            del frame

        with consumer.next(timeout=1.0) as lease:
            view = lease.numpy()
            self.assertEqual(view.__array_interface__["data"][0], slot_address)
            self.assertEqual(lease.timestamp_ns, 1_234_000_000)
            np.testing.assert_array_equal(view, expected)
            del view

    def test_direct_write_refuses_to_commit_while_a_view_is_alive(self):
        bus = self.make_ring("direct_lifetime")
        producer = bus.producer()
        writer = producer.reserve("float32", (8,))
        array = writer.numpy()
        with self.assertRaises(BufferError):
            writer.commit()
        del array
        writer.commit()

    def test_direct_writer_cleanup_keeps_its_producer_alive(self):
        """A rejected commit must remain safe through final object cleanup."""
        script = r"""
import gc
import os
import time

import tenzorbus_rs as tzb

name = f"pyb_cleanup_{os.getpid()}_{time.time_ns()}"
bus = tzb.create(name, slots=2, slot_bytes=4096)
producer = bus.producer()
writer = producer.reserve("float32", (8,))
view = writer.numpy()
try:
    writer.commit()
except BufferError:
    pass
else:
    raise AssertionError("commit unexpectedly accepted a live view")

del view
del producer
del bus
del writer
gc.collect()
"""
        proc = subprocess.run(
            [sys.executable, "-c", script],
            capture_output=True,
            text=True,
            timeout=20,
        )
        self.assertEqual(
            proc.returncode,
            0,
            f"cleanup subprocess exited {proc.returncode}\n"
            f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}",
        )

    def test_create_existing_name_requires_explicit_force(self):
        name = ring_name("double_create")
        self.rings.append(name)
        original = tzb.create(name, slots=2, slot_bytes=4096)
        with self.assertRaises(RuntimeError):
            tzb.create(name, slots=2, slot_bytes=4096)

        # A live old creator must not unlink the replacement when it eventually
        # drops. Replacement is a coordinated maintenance operation.
        original.keep_on_close()
        replacement = tzb.create(name, slots=3, slot_bytes=8192, force=True)
        attached = tzb.attach(name)
        self.assertEqual(original.stats()["slot_count"], 2)
        self.assertEqual(replacement.stats()["slot_count"], 3)
        self.assertEqual(attached.stats()["slot_count"], 3)
        del original
        attached_after_old_drop = tzb.attach(name)
        self.assertEqual(attached_after_old_drop.stats()["slot_count"], 3)

    def test_unlink_removes_the_name_without_invalidating_live_mappings(self):
        name = ring_name("unlink")
        self.rings.append(name)
        creator = tzb.create(name, slots=2, slot_bytes=4096)
        attached = tzb.attach(name)
        producer = creator.producer()
        consumer = attached.consumer()

        tzb.unlink(name)
        with self.assertRaises(RuntimeError):
            tzb.attach(name)

        source = np.arange(16, dtype=np.int32)
        producer.publish(source)
        with consumer.next(timeout=1.0) as lease:
            view = lease.numpy()
            np.testing.assert_array_equal(view, source)
            del view

    def test_direct_write_abort_restores_the_sequence(self):
        bus = self.make_ring("direct_abort", slots=2, slot_bytes=4096)
        producer = bus.producer()
        before = bus.stats()["next_sequence"]
        writer = producer.reserve("float32", (8,))
        writer.abort()
        self.assertEqual(bus.stats()["next_sequence"], before)
        self.assertEqual(bus.stats()["free_slots"], 2)

    def test_media_timestamp_zero_survives_the_rust_ring(self):
        """The first epoch of every clip carries timestamp 0."""
        bus = self.make_ring("ts_zero")
        producer = bus.producer()
        consumer = bus.consumer()
        for requested in (0, 1, 500_000_000):
            producer.publish(np.arange(8, dtype=np.int32), timestamp_ns=requested)
            with consumer.next(timeout=1.0) as lease:
                self.assertEqual(lease.timestamp_ns, requested)

    @unittest.skipUnless(
        os.path.exists(LAB), "build the Rust workspace first (scripts/build_rust.sh)"
    )
    def test_rust_producer_feeds_a_python_consumer(self):
        """Cross-language: a Rust process publishes, Python consumes in place."""
        name = ring_name("xlang")
        self.rings.append(name)
        subprocess.run(
            [LAB, "create", "--ring", name, "--slots", "8", "--capacity", "65536"],
            check=True,
            capture_output=True,
        )
        bus = tzb.attach(name)
        consumer = bus.consumer()
        child = subprocess.Popen(
            [LAB, "produce", "--ring", name, "--count", "100", "--elements", "256"],
            stdout=subprocess.PIPE,
        )
        received = 0
        while received < 100:
            with consumer.next(timeout=10.0) as lease:
                seq = lease.sequence
                values = lease.numpy()
                # The Rust lab's payload contract: element i == seq*1_000_003 + i.
                base = np.uint32((seq * 1_000_003) % (1 << 32))
                expected = (base + np.arange(values.size, dtype=np.uint32)).astype(np.int32)
                np.testing.assert_array_equal(values, expected)
                del values
            received += 1
        child.wait(timeout=20)
        self.assertEqual(child.returncode, 0)
        self.assertEqual(received, 100)


def _consumer_child(name, results):  # pragma: no cover - runs in a child process
    import numpy as np
    import tenzorbus_rs as tzb

    bus = tzb.attach(name)
    consumer = bus.consumer()
    with consumer.next(timeout=20.0) as lease:
        values = lease.numpy()
        results.put(
            {
                "sequence": lease.sequence,
                "slot_index": lease.slot_index,
                "checksum": int(values.sum()),
            }
        )
        del values
    consumer.close()


if __name__ == "__main__":
    unittest.main()
