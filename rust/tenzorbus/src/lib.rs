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
}
