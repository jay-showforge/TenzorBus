from __future__ import annotations

import contextlib
import json
import os
import re
import time
from dataclasses import dataclass
from multiprocessing import shared_memory
from pathlib import Path
from typing import Optional

import numpy as np

from . import protocol as p
import weakref

try:
    import fcntl
except ImportError:  # pragma: no cover - Windows handoff target
    fcntl = None


def _safe_name(name: str) -> str:
    cleaned = re.sub(r"[^A-Za-z0-9_.-]+", "_", name.strip())
    if not cleaned:
        raise ValueError("ring name must contain at least one safe character")
    return cleaned[:64]

class RingError(RuntimeError):
    pass


class LeaseStillBorrowed(RingError, BufferError):
    """Raised when a lease is released while a derived view still borrows its slot.

    Inherits from BufferError as well as RingError so that one handler works across
    both transports: the Rust extension raises BufferError for the same condition, and
    callers should not need to know which backend they are on to catch it.

    `TensorLease.numpy()` and `TensorLease.torch()` hand back views over the
    slot itself -- that is the point of the design. Releasing the lease while
    such a view is alive would let the ring recycle memory the caller is still
    reading, and it also leaves the mapping un-closable, which used to surface
    much later as an unexplained BufferError from `SharedTensorRing.close()`.

    Drop the view first, or copy it if the data must outlive the lease.
    """


class RingFull(RingError):
    pass


class RingTimeout(RingError):
    pass


class _FileLock:
    def __init__(self, path: Path):
        if fcntl is None:
            raise RuntimeError("Python reference transport is tested on POSIX only; Rust handoff adds Windows named synchronization")
        self.path = path
        self.fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o600)

    def __enter__(self):
        fcntl.flock(self.fd, fcntl.LOCK_EX)
        return self

    def __exit__(self, *_):
        fcntl.flock(self.fd, fcntl.LOCK_UN)

    def close(self):
        with contextlib.suppress(OSError):
            os.close(self.fd)


@dataclass
class PublishResult:
    sequence: int
    slot_index: int
    nbytes: int
    readers: int


class TensorLease:
    def __init__(self, consumer: "TensorConsumer", meta: p.TensorMeta, view: memoryview):
        self.consumer = consumer
        self.meta = meta
        self.view = view
        self._released = False
        # Views handed out by numpy()/torch() borrow the slot. Track them weakly
        # so release() can refuse while one is still alive, without keeping it
        # alive itself.
        self._borrowed: "list[weakref.ref]" = []

    def numpy(self) -> np.ndarray:
        dtype = np.dtype(self.meta.dtype)
        arr = np.frombuffer(self.view, dtype=dtype, count=self.meta.nbytes // dtype.itemsize)
        out = arr.reshape(self.meta.shape)
        self._track(out)
        return out

    def torch(self):
        import torch

        dtype_map = {
            "float32": torch.float32,
            "float16": torch.float16,
            "uint8": torch.uint8,
            "int8": torch.int8,
            "int16": torch.int16,
            "int32": torch.int32,
            "int64": torch.int64,
            "float64": torch.float64,
            "bool": torch.bool,
        }
        t = torch.frombuffer(self.view, dtype=dtype_map[self.meta.dtype], count=self.meta.nbytes // np.dtype(self.meta.dtype).itemsize)
        out = t.view(*self.meta.shape)
        self._track(out)
        return out

    def _track(self, view) -> None:
        """Remember a view weakly, so release() can tell whether it is still alive."""
        try:
            self._borrowed.append(weakref.ref(view))
        except TypeError:
            # An object that cannot be weak-referenced cannot be tracked; treat it
            # as borrowed for the lifetime of the lease rather than ignoring it.
            self._borrowed.append(lambda _v=view: _v)

    def borrowed_views(self) -> int:
        """How many views handed out by this lease are still alive."""
        return sum(1 for ref in self._borrowed if ref() is not None)

    def release(self) -> None:
        if self._released:
            return
        # A view returned by numpy()/torch() points at the slot itself. Releasing
        # while one is alive would let the ring recycle memory the caller is still
        # reading, and would leave the mapping un-closable -- which previously only
        # showed up later as a BufferError from SharedTensorRing.close().
        live = self.borrowed_views()
        if live:
            raise LeaseStillBorrowed(
                f"cannot release sequence {self.meta.sequence}: {live} view(s) returned by "
                f"numpy()/torch() are still alive and borrow this slot. "
                f"Delete them first, or take a copy if the data must outlive the lease "
                f"(for example `arr = lease.numpy().copy()`)."
            )
        # Drop derived view reference held by this lease before reclamation.
        self.view.release()
        self._released = True
        self.consumer._release(self.meta)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.release()


class SharedTensorRing:
    """Single-producer, multi-consumer shared-memory tensor ring.

    Reference implementation for TenzorBus v0.1. Payloads are copied exactly once
    by the producer into the shared slab. Consumers obtain zero-copy NumPy/PyTorch
    views over that slab. Coordination uses a tiny filesystem lock; the Rust target
    replaces this with atomics + OS-native wait/wake primitives.
    """

    def __init__(self, logical_name: str, shm: shared_memory.SharedMemory, owner: bool):
        self.logical_name = _safe_name(logical_name)
        self.shm = shm
        self.owner = owner
        self.buf = shm.buf
        self.lock_path = Path("/tmp") / f"tenzorbus_{self.logical_name}.lock"
        self.lock = _FileLock(self.lock_path)
        self._validate_header()
        self.slot_count = p.read_u32(self.buf, p.G_SLOT_COUNT)
        self.slot_capacity = p.read_u64(self.buf, p.G_SLOT_CAPACITY)

    @classmethod
    def create(cls, name: str, *, slot_count: int = 8, slot_capacity: int = 1 << 20, force: bool = False) -> "SharedTensorRing":
        name = _safe_name(name)
        if slot_count < 2:
            raise ValueError("slot_count must be >= 2")
        if slot_capacity < 64:
            raise ValueError("slot_capacity must be >= 64 bytes")
        shm_name = f"tzbus_{name}"
        if force:
            with contextlib.suppress(FileNotFoundError):
                stale = shared_memory.SharedMemory(name=shm_name, create=False)
                stale.close()
                stale.unlink()
        shm = shared_memory.SharedMemory(name=shm_name, create=True, size=p.total_bytes(slot_count, slot_capacity))
        buf = shm.buf
        buf[:] = b"\x00" * len(buf)
        buf[p.G_MAGIC:p.G_MAGIC + len(p.MAGIC)] = p.MAGIC
        p.write_u32(buf, p.G_VERSION, p.VERSION)
        p.write_u32(buf, p.G_SLOT_COUNT, slot_count)
        p.write_u64(buf, p.G_SLOT_CAPACITY, slot_capacity)
        p.write_u64(buf, p.G_NEXT_SEQ, 1)
        p.write_u32(buf, p.G_CONSUMERS, 0)
        p.write_u32(buf, p.G_MAX_CONSUMERS, 64)
        for i in range(slot_count):
            base = p.slot_base(i, slot_capacity)
            buf[base + p.S_MAGIC:base + p.S_MAGIC + 4] = p.SLOT_MAGIC
            p.write_u32(buf, base + p.S_STATE, p.STATE_FREE)
        return cls(name, shm, owner=True)

    @classmethod
    def attach(cls, name: str) -> "SharedTensorRing":
        name = _safe_name(name)
        shm = shared_memory.SharedMemory(name=f"tzbus_{name}", create=False)
        return cls(name, shm, owner=False)

    def _validate_header(self) -> None:
        if bytes(self.buf[p.G_MAGIC:p.G_MAGIC + len(p.MAGIC)]) != p.MAGIC:
            raise RingError("invalid TenzorBus shared-memory magic")
        if p.read_u32(self.buf, p.G_VERSION) != p.VERSION:
            raise RingError("unsupported TenzorBus protocol version")

    def stats(self) -> dict:
        with self.lock:
            return {
                "name": self.logical_name,
                "slot_count": p.read_u32(self.buf, p.G_SLOT_COUNT),
                "slot_capacity": p.read_u64(self.buf, p.G_SLOT_CAPACITY),
                "next_sequence": p.read_u64(self.buf, p.G_NEXT_SEQ),
                "consumers": p.read_u32(self.buf, p.G_CONSUMERS),
                "published": p.read_u64(self.buf, p.G_PUBLISH_COUNT),
                "dropped": p.read_u64(self.buf, p.G_DROP_COUNT),
            }

    def consumer(self) -> "TensorConsumer":
        return TensorConsumer(self)

    def _select_slot(self) -> Optional[int]:
        free = []
        reclaimable = []
        for i in range(self.slot_count):
            base = p.slot_base(i, self.slot_capacity)
            state = p.read_u32(self.buf, base + p.S_STATE)
            if state == p.STATE_FREE:
                free.append(i)
            elif state == p.STATE_COMMITTED and p.read_u16(self.buf, base + p.S_READERS) == 0:
                reclaimable.append((p.read_u64(self.buf, base + p.S_SEQUENCE), i))
        if free:
            return free[0]
        if reclaimable:
            reclaimable.sort()
            return reclaimable[0][1]
        return None

    def publish(self, array, *, timestamp_ns: Optional[int] = None, timeout: float = 1.0, policy: str = "block") -> Optional[PublishResult]:
        arr = np.asarray(array)
        if arr.ndim > p.MAX_NDIM:
            raise ValueError(f"ndim {arr.ndim} exceeds protocol max {p.MAX_NDIM}")
        if str(arr.dtype) not in p.DTYPE_TO_CODE:
            raise ValueError(f"unsupported dtype {arr.dtype}")
        arr = np.ascontiguousarray(arr)
        if arr.nbytes > self.slot_capacity:
            raise ValueError(f"tensor requires {arr.nbytes} bytes; slot capacity is {self.slot_capacity}")
        deadline = time.monotonic() + timeout
        while True:
            with self.lock:
                slot = self._select_slot()
                if slot is not None:
                    base = p.slot_base(slot, self.slot_capacity)
                    sequence = p.read_u64(self.buf, p.G_NEXT_SEQ)
                    consumers = p.read_u32(self.buf, p.G_CONSUMERS)
                    p.write_u32(self.buf, base + p.S_STATE, p.STATE_WRITING)
                    p.write_u64(self.buf, base + p.S_SEQUENCE, sequence)
                    # `or` would treat a legitimate timestamp of 0 as "unset", and
                    # 0 is exactly what the first frame of every clip carries.
                    stamp = time.time_ns() if timestamp_ns is None else timestamp_ns
                    p.write_u64(self.buf, base + p.S_TIMESTAMP_NS, stamp)
                    p.write_u64(self.buf, base + p.S_NBYTES, arr.nbytes)
                    self.buf[base + p.S_DTYPE] = p.DTYPE_TO_CODE[str(arr.dtype)]
                    self.buf[base + p.S_NDIM] = arr.ndim
                    p.write_u16(self.buf, base + p.S_READERS, consumers)
                    p.write_u32(self.buf, base + p.S_FLAGS, 0)
                    p.write_shape(self.buf, base + p.S_SHAPE, arr.shape)
                    # NumPy strides are byte strides.
                    p.write_shape(self.buf, base + p.S_STRIDES, arr.strides)
                    p.write_u32(self.buf, base + p.S_PRODUCER_PID, os.getpid())
                    # Single-producer contract lets us copy after reserving the slot.
                    payload = p.payload_base(slot, self.slot_capacity)
                    self.buf[payload:payload + arr.nbytes] = memoryview(arr).cast("B")
                    p.write_u32(self.buf, base + p.S_STATE, p.STATE_COMMITTED)
                    p.write_u64(self.buf, p.G_NEXT_SEQ, sequence + 1)
                    p.write_u64(self.buf, p.G_PUBLISH_COUNT, p.read_u64(self.buf, p.G_PUBLISH_COUNT) + 1)
                    return PublishResult(sequence, slot, arr.nbytes, consumers)
                if policy == "drop_newest":
                    p.write_u64(self.buf, p.G_DROP_COUNT, p.read_u64(self.buf, p.G_DROP_COUNT) + 1)
                    return None
                if policy != "block":
                    raise ValueError("policy must be 'block' or 'drop_newest'")
            if time.monotonic() >= deadline:
                raise RingFull("no reclaimable slot before timeout")
            time.sleep(0.0005)

    def close(self, *, unlink: Optional[bool] = None) -> None:
        if unlink is None:
            unlink = self.owner
        self.lock.close()
        self.buf.release()
        self.shm.close()
        if unlink:
            with contextlib.suppress(FileNotFoundError):
                self.shm.unlink()
            with contextlib.suppress(FileNotFoundError):
                self.lock_path.unlink()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


class TensorConsumer:
    def __init__(self, ring: SharedTensorRing):
        self.ring = ring
        self._closed = False
        with ring.lock:
            count = p.read_u32(ring.buf, p.G_CONSUMERS)
            max_count = p.read_u32(ring.buf, p.G_MAX_CONSUMERS)
            if count >= max_count:
                raise RingError("maximum consumer count reached")
            p.write_u32(ring.buf, p.G_CONSUMERS, count + 1)
            # Subscribe to future publications only. This guarantees each committed
            # slot's readers_remaining was calculated with this consumer included.
            self.last_sequence = p.read_u64(ring.buf, p.G_NEXT_SEQ) - 1

    def next(self, *, timeout: float = 1.0) -> TensorLease:
        deadline = time.monotonic() + timeout
        while True:
            with self.ring.lock:
                candidates = []
                for i in range(self.ring.slot_count):
                    base = p.slot_base(i, self.ring.slot_capacity)
                    if p.read_u32(self.ring.buf, base + p.S_STATE) != p.STATE_COMMITTED:
                        continue
                    seq = p.read_u64(self.ring.buf, base + p.S_SEQUENCE)
                    if seq > self.last_sequence:
                        candidates.append((seq, i))
                if candidates:
                    seq, i = min(candidates)
                    base = p.slot_base(i, self.ring.slot_capacity)
                    ndim = int(self.ring.buf[base + p.S_NDIM])
                    dtype_code = int(self.ring.buf[base + p.S_DTYPE])
                    nbytes = p.read_u64(self.ring.buf, base + p.S_NBYTES)
                    readers = p.read_u16(self.ring.buf, base + p.S_READERS)
                    if readers == 0:
                        # Published before this consumer counted; skip safely.
                        self.last_sequence = seq
                        continue
                    meta = p.TensorMeta(
                        sequence=seq,
                        timestamp_ns=p.read_u64(self.ring.buf, base + p.S_TIMESTAMP_NS),
                        nbytes=nbytes,
                        dtype=p.CODE_TO_DTYPE[dtype_code],
                        shape=p.read_shape(self.ring.buf, base + p.S_SHAPE, ndim),
                        strides=p.read_shape(self.ring.buf, base + p.S_STRIDES, ndim),
                        slot_index=i,
                        readers_remaining=readers,
                    )
                    payload = p.payload_base(i, self.ring.slot_capacity)
                    view = self.ring.buf[payload:payload + nbytes]
                    self.last_sequence = seq
                    return TensorLease(self, meta, view)
            if time.monotonic() >= deadline:
                raise RingTimeout("no tensor published before timeout")
            time.sleep(0.00025)

    def _release(self, meta: p.TensorMeta) -> None:
        with self.ring.lock:
            base = p.slot_base(meta.slot_index, self.ring.slot_capacity)
            if p.read_u64(self.ring.buf, base + p.S_SEQUENCE) != meta.sequence:
                raise RingError("slot was overwritten before consumer released it")
            readers = p.read_u16(self.ring.buf, base + p.S_READERS)
            if readers == 0:
                raise RingError("lease released more than once")
            p.write_u16(self.ring.buf, base + p.S_READERS, readers - 1)

    def close(self) -> None:
        if self._closed:
            return
        with self.ring.lock:
            count = p.read_u32(self.ring.buf, p.G_CONSUMERS)
            p.write_u32(self.ring.buf, p.G_CONSUMERS, max(0, count - 1))
        self._closed = True

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()