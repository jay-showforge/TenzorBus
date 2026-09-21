//! Shared wire-layout primitives for the Tenzor ecosystem.
//!
//! This crate is the frozen byte-layout authority for TenzorBus v0.1. Every
//! constant here has a parity test against the executed Python reference in
//! `src/tenzorbus/protocol.py`; see `tests/layout_parity.rs`.
//!
//! Layout rules (frozen at the Phase 1 gate):
//!
//! * Global header is 4096 bytes. Offsets 0..56 are the v0.1 reference fields
//!   written by the Python implementation. Offsets 56..4096 were zero-filled
//!   and unused by the reference, so the Rust transport claims them for
//!   coordination state (futex words, registry lock, consumer table). A ring
//!   created by the Python reference therefore presents an empty registry
//!   rather than garbage.
//! * Slot header is 128 bytes. Offsets 0..108 are the v0.1 reference fields.
//!   Offsets 108..128 were the reserved tail, so the Rust transport claims
//!   112..120 for the reader bitmask. `readers_remaining` is maintained as the
//!   popcount of that mask so the Python reference's view stays truthful.

#![forbid(unsafe_op_in_unsafe_fn)]

use core::mem::{offset_of, size_of};

pub const PROTOCOL_MAGIC: [u8; 8] = *b"TZBUS001";
pub const SLOT_MAGIC: [u8; 4] = *b"SLOT";
pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_NDIM: usize = 8;
pub const GLOBAL_HEADER_SIZE: usize = 4096;
pub const SLOT_HEADER_SIZE: usize = 128;

/// Hard ceiling on simultaneously registered consumers. Bounded by the width of
/// the per-slot reader bitmask.
pub const MAX_CONSUMERS: usize = 64;

// ---------------------------------------------------------------------------
// Slot states (values are frozen; the Python reference uses the same numbers)
// ---------------------------------------------------------------------------

pub const STATE_FREE: u32 = 0;
pub const STATE_COMMITTED: u32 = 1;
pub const STATE_WRITING: u32 = 2;

// ---------------------------------------------------------------------------
// Global header offsets
// ---------------------------------------------------------------------------

pub const G_MAGIC: usize = 0;
pub const G_VERSION: usize = 8;
pub const G_SLOT_COUNT: usize = 12;
pub const G_SLOT_CAPACITY: usize = 16;
pub const G_NEXT_SEQ: usize = 24;
pub const G_CONSUMERS: usize = 32;
pub const G_MAX_CONSUMERS: usize = 36;
pub const G_PUBLISH_COUNT: usize = 40;
pub const G_DROP_COUNT: usize = 48;

// Rust transport coordination state, inside the reserved region.
/// Monotonic counter bumped on every commit; consumers futex-wait on it.
pub const G_PUBLISH_FUTEX: usize = 56;
/// Monotonic counter bumped whenever a slot becomes reclaimable; the producer
/// futex-waits on it under the `block` policy.
pub const G_RECLAIM_FUTEX: usize = 60;
/// Futex mutex guarding registration and slot reservation.
pub const G_REGISTRY_LOCK: usize = 64;
/// Producer pid, for reclaiming slots abandoned mid-write.
pub const G_PRODUCER_PID: usize = 68;
/// Bumped whenever the consumer registry changes (register/unregister/reap).
pub const G_REGISTRY_EPOCH: usize = 72;
/// Pid currently holding `G_REGISTRY_LOCK`, so a dead holder's lock can be stolen.
pub const G_REGISTRY_LOCK_OWNER: usize = 76;
/// Count of consumers reaped after dying while holding a lease.
pub const G_REAPED_COUNT: usize = 80;
/// Start-time token of the producer pid, so a recycled pid is not mistaken for
/// the original producer.
pub const G_PRODUCER_START_TOKEN: usize = 88;
/// Start-time token of the registry-lock owner pid.
pub const G_REGISTRY_LOCK_OWNER_TOKEN: usize = 92;
/// Start of the consumer registration table.
pub const G_REGISTRY_OFF: usize = 128;
pub const REGISTRY_ENTRY_SIZE: usize = 32;

// Registry entry field offsets, relative to the entry base.
pub const R_STATE: usize = 0;
pub const R_PID: usize = 4;
pub const R_GENERATION: usize = 8;
pub const R_FLAGS: usize = 12;
pub const R_HEARTBEAT_NS: usize = 16;
pub const R_LAST_SEQUENCE: usize = 24;

pub const REGISTRY_FREE: u32 = 0;
pub const REGISTRY_ACTIVE: u32 = 1;

// ---------------------------------------------------------------------------
// Slot header offsets
// ---------------------------------------------------------------------------

pub const S_MAGIC: usize = 0;
pub const S_STATE: usize = 4;
pub const S_SEQUENCE: usize = 8;
pub const S_TIMESTAMP_NS: usize = 16;
pub const S_NBYTES: usize = 24;
pub const S_DTYPE: usize = 32;
pub const S_NDIM: usize = 33;
pub const S_READERS: usize = 34;
pub const S_FLAGS: usize = 36;
pub const S_SHAPE: usize = 40;
pub const S_STRIDES: usize = 72;
pub const S_PRODUCER_PID: usize = 104;
/// Rust transport extension inside the reserved tail: one bit per registered
/// consumer that still holds a lease on this slot.
pub const S_READERS_MASK: usize = 112;
/// Start-time token of the pid in `S_PRODUCER_PID`, for abandoned-write recovery.
pub const S_PRODUCER_START_TOKEN: usize = 120;

// ---------------------------------------------------------------------------
// Dtypes
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    F32 = 1,
    F16 = 2,
    U8 = 3,
    I8 = 4,
    I16 = 5,
    I32 = 6,
    I64 = 7,
    F64 = 8,
    Bool = 9,
}

impl DType {
    pub const fn code(self) -> u8 {
        self as u8
    }

    pub const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => DType::F32,
            2 => DType::F16,
            3 => DType::U8,
            4 => DType::I8,
            5 => DType::I16,
            6 => DType::I32,
            7 => DType::I64,
            8 => DType::F64,
            9 => DType::Bool,
            _ => return None,
        })
    }

    /// Element size in bytes. Matches NumPy itemsize for every supported dtype.
    pub const fn itemsize(self) -> usize {
        match self {
            DType::F32 => 4,
            DType::F16 => 2,
            DType::U8 => 1,
            DType::I8 => 1,
            DType::I16 => 2,
            DType::I32 => 4,
            DType::I64 => 8,
            DType::F64 => 8,
            DType::Bool => 1,
        }
    }

    /// NumPy dtype spelling, as the Python reference writes it.
    pub const fn numpy_name(self) -> &'static str {
        match self {
            DType::F32 => "float32",
            DType::F16 => "float16",
            DType::U8 => "uint8",
            DType::I8 => "int8",
            DType::I16 => "int16",
            DType::I32 => "int32",
            DType::I64 => "int64",
            DType::F64 => "float64",
            DType::Bool => "bool",
        }
    }

    pub fn from_numpy_name(name: &str) -> Option<Self> {
        Some(match name {
            "float32" => DType::F32,
            "float16" => DType::F16,
            "uint8" => DType::U8,
            "int8" => DType::I8,
            "int16" => DType::I16,
            "int32" => DType::I32,
            "int64" => DType::I64,
            "float64" => DType::F64,
            "bool" => DType::Bool,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Header structs. These exist so the C ABI and the offset constants above are
// checked against each other at compile time.
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TensorSlotHeader {
    pub magic: [u8; 4],
    pub state: u32,
    pub sequence: u64,
    pub timestamp_ns: u64,
    pub nbytes: u64,
    pub dtype: u8,
    pub ndim: u8,
    pub readers_remaining: u16,
    pub flags: u32,
    pub shape: [u32; MAX_NDIM],
    pub strides: [u32; MAX_NDIM],
    pub producer_pid: u32,
    pub _pad0: u32,
    pub readers_mask: u64,
    pub producer_start_token: u32,
    pub _reserved: [u8; 4],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ConsumerEntry {
    pub state: u32,
    pub pid: u32,
    pub generation: u32,
    pub flags: u32,
    pub heartbeat_ns: u64,
    pub last_sequence: u64,
}

impl TensorSlotHeader {
    pub fn validate(&self, slot_capacity: usize) -> Result<(), &'static str> {
        if self.magic != SLOT_MAGIC {
            return Err("bad slot magic");
        }
        if self.ndim as usize > MAX_NDIM {
            return Err("ndim exceeds MAX_NDIM");
        }
        if self.nbytes as usize > slot_capacity {
            return Err("payload exceeds slot capacity");
        }
        if DType::from_code(self.dtype).is_none() {
            return Err("unknown dtype code");
        }
        Ok(())
    }
}

// Compile-time proof that the C ABI structs agree with the offset constants.
const _: () = {
    assert!(size_of::<TensorSlotHeader>() == SLOT_HEADER_SIZE);
    assert!(offset_of!(TensorSlotHeader, magic) == S_MAGIC);
    assert!(offset_of!(TensorSlotHeader, state) == S_STATE);
    assert!(offset_of!(TensorSlotHeader, sequence) == S_SEQUENCE);
    assert!(offset_of!(TensorSlotHeader, timestamp_ns) == S_TIMESTAMP_NS);
    assert!(offset_of!(TensorSlotHeader, nbytes) == S_NBYTES);
    assert!(offset_of!(TensorSlotHeader, dtype) == S_DTYPE);
    assert!(offset_of!(TensorSlotHeader, ndim) == S_NDIM);
    assert!(offset_of!(TensorSlotHeader, readers_remaining) == S_READERS);
    assert!(offset_of!(TensorSlotHeader, flags) == S_FLAGS);
    assert!(offset_of!(TensorSlotHeader, shape) == S_SHAPE);
    assert!(offset_of!(TensorSlotHeader, strides) == S_STRIDES);
    assert!(offset_of!(TensorSlotHeader, producer_pid) == S_PRODUCER_PID);
    assert!(offset_of!(TensorSlotHeader, readers_mask) == S_READERS_MASK);
    assert!(offset_of!(TensorSlotHeader, producer_start_token) == S_PRODUCER_START_TOKEN);

    assert!(size_of::<ConsumerEntry>() == REGISTRY_ENTRY_SIZE);
    assert!(offset_of!(ConsumerEntry, state) == R_STATE);
    assert!(offset_of!(ConsumerEntry, pid) == R_PID);
    assert!(offset_of!(ConsumerEntry, generation) == R_GENERATION);
    assert!(offset_of!(ConsumerEntry, heartbeat_ns) == R_HEARTBEAT_NS);
    assert!(offset_of!(ConsumerEntry, last_sequence) == R_LAST_SEQUENCE);

    // The registry must fit inside the reserved region of the global header.
    assert!(G_REGISTRY_OFF + MAX_CONSUMERS * REGISTRY_ENTRY_SIZE <= GLOBAL_HEADER_SIZE);
    // Coordination words must not collide with the v0.1 reference fields.
    assert!(G_PUBLISH_FUTEX >= G_DROP_COUNT + 8);
};

// ---------------------------------------------------------------------------
// Layout arithmetic (mirrors protocol.py)
// ---------------------------------------------------------------------------

pub const fn align_up(value: usize, alignment: usize) -> usize {
    value.div_ceil(alignment) * alignment
}

pub const fn slot_stride(slot_capacity: usize) -> usize {
    align_up(SLOT_HEADER_SIZE + slot_capacity, 64)
}

pub const fn total_bytes(slot_count: usize, slot_capacity: usize) -> usize {
    GLOBAL_HEADER_SIZE + slot_count * slot_stride(slot_capacity)
}

pub const fn slot_base(slot_index: usize, slot_capacity: usize) -> usize {
    GLOBAL_HEADER_SIZE + slot_index * slot_stride(slot_capacity)
}

pub const fn payload_base(slot_index: usize, slot_capacity: usize) -> usize {
    slot_base(slot_index, slot_capacity) + SLOT_HEADER_SIZE
}

pub const fn registry_entry_offset(index: usize) -> usize {
    G_REGISTRY_OFF + index * REGISTRY_ENTRY_SIZE
}

/// Sequence comparison that stays correct across a u64 wraparound.
/// Returns true when `a` is newer than `b`.
pub fn seq_newer(a: u64, b: u64) -> bool {
    (a.wrapping_sub(b) as i64) > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_128_bytes() {
        assert_eq!(size_of::<TensorSlotHeader>(), 128);
    }

    #[test]
    fn stride_is_cacheline_aligned() {
        assert_eq!(slot_stride(602_112) % 64, 0);
    }

    #[test]
    fn dtype_codes_round_trip() {
        for code in 1..=9u8 {
            let d = DType::from_code(code).expect("known code");
            assert_eq!(d.code(), code);
            assert_eq!(DType::from_numpy_name(d.numpy_name()), Some(d));
        }
        assert!(DType::from_code(0).is_none());
        assert!(DType::from_code(10).is_none());
    }

    #[test]
    fn sequence_comparison_survives_wraparound() {
        assert!(seq_newer(2, 1));
        assert!(!seq_newer(1, 2));
        assert!(!seq_newer(1, 1));
        // Straddling the u64 boundary.
        assert!(seq_newer(0, u64::MAX));
        assert!(seq_newer(3, u64::MAX - 2));
        assert!(!seq_newer(u64::MAX - 2, 3));
    }

    #[test]
    fn registry_fits_in_global_header() {
        assert!(registry_entry_offset(MAX_CONSUMERS) <= GLOBAL_HEADER_SIZE);
    }
}
