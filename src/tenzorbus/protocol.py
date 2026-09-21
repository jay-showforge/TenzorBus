from __future__ import annotations

import struct
from dataclasses import dataclass
from typing import Sequence

MAGIC = b"TZBUS001"
SLOT_MAGIC = b"SLOT"
VERSION = 1
GLOBAL_HEADER_SIZE = 4096
SLOT_HEADER_SIZE = 128
MAX_NDIM = 8

STATE_FREE = 0
STATE_COMMITTED = 1
STATE_WRITING = 2

# Global header offsets.
G_MAGIC = 0
G_VERSION = 8
G_SLOT_COUNT = 12
G_SLOT_CAPACITY = 16
G_NEXT_SEQ = 24
G_CONSUMERS = 32
G_MAX_CONSUMERS = 36
G_PUBLISH_COUNT = 40
G_DROP_COUNT = 48

# Slot header offsets.
S_MAGIC = 0
S_STATE = 4
S_SEQUENCE = 8
S_TIMESTAMP_NS = 16
S_NBYTES = 24
S_DTYPE = 32
S_NDIM = 33
S_READERS = 34
S_FLAGS = 36
S_SHAPE = 40
S_STRIDES = 72
S_PRODUCER_PID = 104

DTYPE_TO_CODE = {
    "float32": 1,
    "float16": 2,
    "uint8": 3,
    "int8": 4,
    "int16": 5,
    "int32": 6,
    "int64": 7,
    "float64": 8,
    "bool": 9,
}
CODE_TO_DTYPE = {v: k for k, v in DTYPE_TO_CODE.items()}


def align_up(value: int, alignment: int = 64) -> int:
    return (value + alignment - 1) // alignment * alignment


def slot_stride(slot_capacity: int) -> int:
    return align_up(SLOT_HEADER_SIZE + slot_capacity, 64)


def total_bytes(slot_count: int, slot_capacity: int) -> int:
    return GLOBAL_HEADER_SIZE + slot_count * slot_stride(slot_capacity)


def slot_base(slot_index: int, slot_capacity: int) -> int:
    return GLOBAL_HEADER_SIZE + slot_index * slot_stride(slot_capacity)


def payload_base(slot_index: int, slot_capacity: int) -> int:
    return slot_base(slot_index, slot_capacity) + SLOT_HEADER_SIZE


def read_u32(buf, off: int) -> int:
    return struct.unpack_from("<I", buf, off)[0]


def write_u32(buf, off: int, value: int) -> None:
    struct.pack_into("<I", buf, off, int(value))


def read_u64(buf, off: int) -> int:
    return struct.unpack_from("<Q", buf, off)[0]


def write_u64(buf, off: int, value: int) -> None:
    struct.pack_into("<Q", buf, off, int(value))


def read_u16(buf, off: int) -> int:
    return struct.unpack_from("<H", buf, off)[0]


def write_u16(buf, off: int, value: int) -> None:
    struct.pack_into("<H", buf, off, int(value))


def write_shape(buf, off: int, values: Sequence[int]) -> None:
    padded = list(values[:MAX_NDIM]) + [0] * (MAX_NDIM - len(values))
    struct.pack_into("<" + "I" * MAX_NDIM, buf, off, *padded)


def read_shape(buf, off: int, ndim: int) -> tuple[int, ...]:
    vals = struct.unpack_from("<" + "I" * MAX_NDIM, buf, off)
    return tuple(int(x) for x in vals[:ndim])


@dataclass(frozen=True)
class TensorMeta:
    sequence: int
    timestamp_ns: int
    nbytes: int
    dtype: str
    shape: tuple[int, ...]
    strides: tuple[int, ...]
    slot_index: int
    readers_remaining: int
