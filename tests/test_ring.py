import multiprocessing as mp
import os
import time
import unittest

import numpy as np

from tenzorbus import RingFull, SharedTensorRing


def _consumer_worker(name, ready, outq):
    ring = SharedTensorRing.attach(name)
    consumer = ring.consumer()
    ready.set()
    try:
        with consumer.next(timeout=3.0) as lease:
            arr = lease.numpy()
            outq.put({
                "sequence": lease.meta.sequence,
                "slot": lease.meta.slot_index,
                "shape": arr.shape,
                "sum": float(arr.sum()),
                "first": float(arr.flat[0]),
            })
            # Zero-copy views must not outlive the lease.
            del arr
    finally:
        consumer.close()
        ring.close(unlink=False)


class RingTests(unittest.TestCase):
    def setUp(self):
        self.name = f"test_{os.getpid()}_{time.time_ns()}"
        self.ring = SharedTensorRing.create(self.name, slot_count=4, slot_capacity=1 << 20, force=True)

    def tearDown(self):
        self.ring.close(unlink=True)

    def test_zero_copy_numpy_view(self):
        consumer = self.ring.consumer()
        src = np.arange(3 * 8 * 8, dtype=np.float32).reshape(3, 8, 8)
        pub = self.ring.publish(src)
        with consumer.next() as lease:
            got = lease.numpy()
            self.assertEqual(got.shape, src.shape)
            self.assertEqual(pub.sequence, lease.meta.sequence)
            self.assertTrue(np.shares_memory(got, np.frombuffer(lease.view, dtype=np.uint8)))
            np.testing.assert_array_equal(got, src)
            # The view borrows the slot, so drop it before the lease releases.
            # Every assertion above still runs against the aliased memory.
            del got
        consumer.close()


    def test_zero_copy_torch_view(self):
        try:
            import torch
        except Exception:
            self.skipTest("torch not installed")
        consumer = self.ring.consumer()
        src = np.arange(3 * 4 * 4, dtype=np.float32).reshape(3, 4, 4)
        self.ring.publish(src)
        with consumer.next() as lease:
            tensor = lease.torch()
            self.assertEqual(tuple(tensor.shape), src.shape)
            self.assertAlmostEqual(float(tensor[0, 0, 1]), float(src[0, 0, 1]))
            # Prove the Torch tensor aliases the shared slot: mutating the slot view
            # is immediately visible through Torch while the lease is alive.
            arr = lease.numpy()
            arr[0, 0, 1] = 123.5
            self.assertAlmostEqual(float(tensor[0, 0, 1]), 123.5)
            del arr
            del tensor
        consumer.close()

    def test_multi_consumer_same_publication(self):
        q = mp.Queue()
        r1, r2 = mp.Event(), mp.Event()
        p1 = mp.Process(target=_consumer_worker, args=(self.name, r1, q))
        p2 = mp.Process(target=_consumer_worker, args=(self.name, r2, q))
        p1.start(); p2.start()
        self.assertTrue(r1.wait(3.0)); self.assertTrue(r2.wait(3.0))
        src = np.linspace(-1, 1, 3 * 32 * 32, dtype=np.float32).reshape(3, 32, 32)
        pub = self.ring.publish(src)
        a, b = q.get(timeout=3), q.get(timeout=3)
        p1.join(3); p2.join(3)
        self.assertEqual(p1.exitcode, 0); self.assertEqual(p2.exitcode, 0)
        self.assertEqual(a["sequence"], pub.sequence)
        self.assertEqual(b["sequence"], pub.sequence)
        self.assertEqual(a["slot"], b["slot"])
        self.assertAlmostEqual(a["sum"], float(src.sum()), places=4)
        self.assertAlmostEqual(b["sum"], float(src.sum()), places=4)

    def test_bounded_ring_does_not_overwrite_active_reader(self):
        # One consumer reserves responsibility for every publication and intentionally
        # holds all slots, so the producer must report backpressure instead of overwrite.
        consumer = self.ring.consumer()
        leases = []
        try:
            for i in range(4):
                self.ring.publish(np.full((16,), i, dtype=np.float32))
                leases.append(consumer.next())
            with self.assertRaises(RingFull):
                self.ring.publish(np.zeros((16,), dtype=np.float32), timeout=0.01)
        finally:
            for lease in leases:
                lease.release()
            consumer.close()

    def test_timestamp_zero_is_recorded_faithfully(self):
        """A media timestamp of 0 must survive.

        The first epoch of every clip carries timestamp 0, so treating it as
        "unset" and substituting wall-clock time silently desynchronises the
        first frame of every integration.
        """
        ring = SharedTensorRing.create(
            f"tszero_{os.getpid()}_{time.time_ns()}",
            slot_count=4, slot_capacity=4096, force=True,
        )
        self.addCleanup(ring.close, unlink=True)
        consumer = ring.consumer()
        self.addCleanup(consumer.close)
        for requested in (0, 1, 500_000_000):
            ring.publish(np.arange(8, dtype=np.int32), timestamp_ns=requested)
            with consumer.next() as lease:
                self.assertEqual(lease.meta.timestamp_ns, requested)

    def test_timestamp_defaults_to_wall_clock_when_unset(self):
        ring = SharedTensorRing.create(
            f"tsdefault_{os.getpid()}_{time.time_ns()}",
            slot_count=4, slot_capacity=4096, force=True,
        )
        self.addCleanup(ring.close, unlink=True)
        consumer = ring.consumer()
        self.addCleanup(consumer.close)
        ring.publish(np.arange(8, dtype=np.int32))
        with consumer.next() as lease:
            self.assertGreater(lease.meta.timestamp_ns, 1_600_000_000_000_000_000)


if __name__ == "__main__":
    unittest.main()
