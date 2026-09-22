//! TenzorBus — single-producer / multi-consumer shared-memory tensor transport.
//!
//! This is the Linux production ring. It keeps the byte layout and the
//! ownership semantics of the executed v0.1 Python reference (`src/tenzorbus/`)
//! and replaces its filesystem lock + polling loops with:
//!
//! * named `shm_open`/`mmap` shared mappings,
//! * atomic slot-state transitions with release/acquire publication,
//! * a consumer registration table carrying pid, generation and heartbeat,
//! * crash recovery, so a consumer killed while holding a lease cannot pin a
//!   slot forever and a producer killed mid-write cannot wedge the ring,
//! * futex wait/wake, so neither side spins.
//!
//! Ownership contract (unchanged from v0.1):
//!
//! * The producer copies a payload into a slot exactly once.
//! * A committed slot is reclaimable only when every consumer that was
//!   registered at publish time has released it.
//! * A consumer's view aliases the shared slab; it is valid only for the
//!   lifetime of its lease.

// The shared-memory protocol stores little-endian 64-bit words and relies on
// lock-free 16/32/64-bit atomics shared between processes. Keep unsupported
// targets from compiling a binary that could silently misinterpret the frozen
// v1 layout. This release intentionally covers Linux x86-64 and Linux ARM64.
#[cfg(not(target_os = "linux"))]
compile_error!("the TenzorBus production ring currently supports Linux only");
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("the TenzorBus production ring supports x86-64 and ARM64 only");
#[cfg(not(target_endian = "little"))]
compile_error!("the frozen TenzorBus v1 protocol requires a little-endian target");
#[cfg(not(all(
    target_has_atomic = "16",
    target_has_atomic = "32",
    target_has_atomic = "64"
)))]
compile_error!("TenzorBus requires native 16/32/64-bit atomic operations");

pub mod error;
pub mod futex;
pub mod ring;
pub mod shm;

pub use error::{Error, Result};
pub use ring::{
    Backpressure, Consumer, Lease, Producer, PublishResult, Ring, RingLayout, RingOptions,
    RingStats, SlotWriter, TensorMeta, TensorView,
};
pub use tenzor_core::{
    DType, GLOBAL_HEADER_SIZE, MAX_CONSUMERS, MAX_NDIM, PROTOCOL_MAGIC, PROTOCOL_VERSION,
    SLOT_HEADER_SIZE, TensorSlotHeader, slot_stride, total_bytes,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_layout_size() {
        let layout = RingLayout::new(8, 1 << 20).unwrap();
        assert!(layout.total_bytes() < 9 * (1 << 20));
    }

    #[test]
    fn layout_rejects_degenerate_configs() {
        assert!(RingLayout::new(1, 1 << 20).is_err());
        assert!(RingLayout::new(8, 8).is_err());
    }

    #[test]
    fn native_platform_contract_matches_the_frozen_layout() {
        assert_eq!(std::mem::size_of::<usize>(), 8);
        assert_eq!(std::mem::size_of::<std::sync::atomic::AtomicU64>(), 8);
        assert_eq!(std::mem::align_of::<std::sync::atomic::AtomicU64>(), 8);
    }
}
