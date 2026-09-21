"""Regression tests for the lease/view lifetime contract, on both transports.

A view returned by `numpy()` or `torch()` aliases the slot itself. Releasing the
lease while such a view is alive would let the ring recycle memory the caller is
still reading. Both backends must refuse, and both must refuse for Torch as well
as NumPy -- the Torch case is the one that regressed, because
`torch.frombuffer()` drops the Py_buffer as soon as it returns, which left the
export count at zero while the tensor still pointed at the slot.

These tests exist so that hole cannot reopen silently.
"""
from __future__ import annotations

import unittest

import numpy as np

from tenzorbus import (MAX_CONSUMERS, LeaseStillBorrowed, RingError,
                       SharedTensorRing, TensorConsumer)

try:
    import torch
    HAVE_TORCH = True
except Exception:
    HAVE_TORCH = False

try:
    import tenzorbus_rs
    HAVE_RUST = True
except Exception:
    HAVE_RUST = False

SRC = np.arange(3 * 8 * 8, dtype=np.float32).reshape(3, 8, 8)


class PythonLeaseLifetimeTests(unittest.TestCase):
    """The pure-Python reference transport."""

    def setUp(self):
        self.ring = SharedTensorRing.create(
            f"lifetime_py_{self.id().rsplit('.', 1)[-1]}",
            slot_count=4, slot_capacity=1 << 16, force=True)
        self.consumer = TensorConsumer(self.ring)
        self.addCleanup(self.ring.close, unlink=True)
        self.addCleanup(self.consumer.close)

    def test_release_refuses_while_a_numpy_view_is_alive(self):
        self.ring.publish(SRC)
        lease = self.consumer.next(timeout=2.0)
        view = lease.numpy()
        with self.assertRaises(LeaseStillBorrowed):
            lease.release()
        del view
        lease.release()          # the documented remedy

    @unittest.skipUnless(HAVE_TORCH, "torch not installed")
    def test_release_refuses_while_a_torch_view_is_alive(self):
        self.ring.publish(SRC)
        lease = self.consumer.next(timeout=2.0)
        tensor = lease.torch()
        with self.assertRaises(LeaseStillBorrowed):
            lease.release()
        del tensor
        lease.release()

    def test_a_copy_does_not_borrow_the_slot(self):
        self.ring.publish(SRC)
        lease = self.consumer.next(timeout=2.0)
        kept = lease.numpy().copy()
        lease.release()                       # must not raise
        np.testing.assert_array_equal(kept, SRC)

    def test_the_error_is_catchable_as_a_buffer_error(self):
        # The Rust transport raises BufferError for the same condition, so one
        # handler has to work across both.
        self.assertTrue(issubclass(LeaseStillBorrowed, BufferError))
        self.ring.publish(SRC)
        lease = self.consumer.next(timeout=2.0)
        view = lease.numpy()
        with self.assertRaises(BufferError):
            lease.release()
        del view
        lease.release()

    def test_release_stays_idempotent(self):
        self.ring.publish(SRC)
        lease = self.consumer.next(timeout=2.0)
        lease.release()
        lease.release()                       # a safe no-op, not an error

    def test_max_consumers_is_documented_and_enforced(self):
        self.assertEqual(MAX_CONSUMERS, 64)
        extra = []
        self.addCleanup(lambda: [c.close() for c in extra])
        with self.assertRaises(RingError):
            # one already exists from setUp
            for _ in range(MAX_CONSUMERS + 1):
                extra.append(TensorConsumer(self.ring))


@unittest.skipUnless(HAVE_RUST, "build the Rust extension: scripts/build_rust.sh")
class RustLeaseLifetimeTests(unittest.TestCase):
    """The Rust/PyO3 transport, which tracks buffer-protocol exports."""

    def setUp(self):
        self.ring = tenzorbus_rs.create(
            f"lifetime_rs_{self.id().rsplit('.', 1)[-1]}", slots=4, slot_bytes=1 << 16)
        self.producer = self.ring.producer()
        self.consumer = self.ring.consumer()

    def test_release_refuses_while_a_numpy_view_is_alive(self):
        self.producer.publish(SRC)
        lease = self.consumer.next(timeout=2.0)
        view = lease.numpy()
        with self.assertRaises(BufferError):
            lease.release()
        del view
        lease.release()

    @unittest.skipUnless(HAVE_TORCH, "torch not installed")
    def test_release_refuses_while_a_torch_view_is_alive(self):
        # The regression: torch.frombuffer() released the Py_buffer immediately,
        # so the lease could be handed back while the tensor still aliased the slot.
        self.producer.publish(SRC)
        lease = self.consumer.next(timeout=2.0)
        tensor = lease.torch()
        with self.assertRaises(BufferError):
            lease.release()
        del tensor
        lease.release()

    @unittest.skipUnless(HAVE_TORCH, "torch not installed")
    def test_the_torch_view_actually_aliases_the_slot(self):
        # Guard against "fixing" the lifetime issue by quietly returning a copy.
        self.producer.publish(SRC)
        lease = self.consumer.next(timeout=2.0)
        tensor = lease.torch()
        view = lease.numpy()
        view[0, 0, 0] = 1234.5
        self.assertAlmostEqual(float(tensor[0, 0, 0]), 1234.5)
        del tensor, view
        lease.release()

    def test_copy_outlives_the_lease(self):
        self.producer.publish(SRC)
        lease = self.consumer.next(timeout=2.0)
        kept = lease.copy()
        lease.release()                       # must not raise
        np.testing.assert_array_equal(kept, SRC)

    def test_context_manager_surfaces_a_held_view(self):
        self.producer.publish(SRC)
        held = None
        with self.assertRaises(BufferError):
            with self.consumer.next(timeout=2.0) as lease:
                held = lease.numpy()
        del held


if __name__ == "__main__":
    unittest.main()
