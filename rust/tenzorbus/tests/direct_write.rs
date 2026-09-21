//! Phase 4B acceptance: the direct-write path, and the ownership rules it must
//! not bend.
//!
//! `publish` copies a tensor the caller already holds. `reserve` hands the caller
//! the slot itself so a producer that generates its data can write it straight
//! into the destination — no producer copy at all. Everything TenzorBus
//! guarantees about slot ownership has to survive that, which is what most of
//! these tests are about.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tenzorbus::{Backpressure, DType, Error, Ring, RingOptions, TensorView};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_ring(tag: &str, slots: usize, capacity: usize) -> Ring {
    let name = format!(
        "dw_{tag}_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    Ring::create(
        &name,
        RingOptions {
            slot_count: slots,
            slot_capacity: capacity,
            force: true,
        },
    )
    .expect("create ring")
}

#[test]
fn direct_write_delivers_exactly_what_the_producer_wrote() {
    let ring = make_ring("roundtrip", 4, 4096);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    for seq in 1..=5u64 {
        let mut writer = producer
            .reserve(
                DType::F32,
                &[4, 8],
                Backpressure::Block,
                Duration::from_millis(500),
            )
            .expect("reserve")
            .expect("slot");
        writer.set_timestamp_ns(seq * 1_000_000);
        // Generate straight into the slot.
        let values = unsafe { writer.payload_as::<f32>() };
        assert_eq!(values.len(), 32);
        for (i, v) in values.iter_mut().enumerate() {
            *v = seq as f32 * 100.0 + i as f32;
        }
        let published = writer.commit();
        assert_eq!(published.sequence, seq);
        assert_eq!(published.nbytes, 32 * 4);

        let lease = consumer.next(Duration::from_millis(500)).unwrap();
        assert_eq!(lease.sequence(), seq);
        assert_eq!(lease.meta().dtype, DType::F32);
        assert_eq!(lease.meta().shape(), &[4, 8]);
        assert_eq!(lease.meta().strides(), &[32, 4]);
        assert_eq!(lease.meta().timestamp_ns, seq * 1_000_000);
        let got: &[f32] = unsafe { lease.as_slice::<f32>() };
        for (i, v) in got.iter().enumerate() {
            assert_eq!(
                *v,
                seq as f32 * 100.0 + i as f32,
                "element {i} of seq {seq}"
            );
        }
    }
}

#[test]
fn the_producer_writes_into_the_same_bytes_the_consumer_reads() {
    // The whole point of Phase 4B: one buffer, written once, read in place.
    let ring = make_ring("onebuffer", 4, 4096);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    let mut writer = producer
        .reserve(
            DType::U8,
            &[64],
            Backpressure::Block,
            Duration::from_millis(500),
        )
        .expect("reserve")
        .expect("slot");
    let slot = writer.slot_index();
    let write_addr = writer.payload().as_ptr() as usize;
    writer.payload().copy_from_slice(&[7u8; 64]);
    writer.commit();

    let lease = consumer.next(Duration::from_millis(500)).unwrap();
    let read_addr = lease.data().as_ptr() as usize;
    let expected = ring.base_addr() + tenzor_core::payload_base(slot, ring.slot_capacity());

    assert_eq!(write_addr, expected, "producer did not write into the slot");
    assert_eq!(read_addr, expected, "consumer did not read from the slot");
    assert_eq!(write_addr, read_addr, "a copy happened somewhere");
    assert_eq!(write_addr % 64, 0, "payload lost its cache-line alignment");
    assert_eq!(lease.data(), &[7u8; 64]);
}

#[test]
fn a_reserved_slot_is_invisible_until_commit() {
    let ring = make_ring("invisible", 4, 4096);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    let mut writer = producer
        .reserve(
            DType::I32,
            &[16],
            Backpressure::Block,
            Duration::from_millis(500),
        )
        .expect("reserve")
        .expect("slot");
    unsafe { writer.payload_as::<i32>() }.fill(42);

    // Nothing is committed, so a consumer must find nothing at all.
    assert!(matches!(
        consumer.next(Duration::from_millis(80)),
        Err(Error::Timeout)
    ));
    let stats = ring.stats();
    assert_eq!(stats.committed_slots, 0);
    assert_eq!(stats.published, 0);

    writer.commit();
    let lease = consumer.next(Duration::from_millis(500)).unwrap();
    assert_eq!(unsafe { lease.as_slice::<i32>() }, &[42i32; 16]);
}

#[test]
fn abort_returns_the_slot_and_the_sequence_number() {
    let ring = make_ring("abort", 2, 4096);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    let before = ring.stats().next_sequence;
    let writer = producer
        .reserve(
            DType::I32,
            &[16],
            Backpressure::Block,
            Duration::from_millis(500),
        )
        .expect("reserve")
        .expect("slot");
    assert_eq!(writer.sequence(), before);
    writer.abort();

    // Delivered sequences must stay contiguous: an aborted reservation cannot
    // leave a hole that a consumer would report as a gap.
    assert_eq!(
        ring.stats().next_sequence,
        before,
        "abort consumed a sequence"
    );
    assert_eq!(ring.stats().free_slots, 2, "abort leaked a slot");
    assert_eq!(ring.stats().published, 0);

    let mut writer = producer
        .reserve(
            DType::I32,
            &[16],
            Backpressure::Block,
            Duration::from_millis(500),
        )
        .expect("reserve")
        .expect("slot");
    assert_eq!(writer.sequence(), before, "the sequence was not reusable");
    unsafe { writer.payload_as::<i32>() }.fill(9);
    writer.commit();
    let lease = consumer.next(Duration::from_millis(500)).unwrap();
    assert_eq!(lease.sequence(), before);
}

#[test]
fn dropping_a_writer_without_committing_does_not_leak_the_slot() {
    let ring = make_ring("dropleak", 2, 4096);
    let producer = ring.producer().unwrap();

    for _ in 0..10 {
        let mut writer = producer
            .reserve(
                DType::I32,
                &[16],
                Backpressure::Block,
                Duration::from_millis(500),
            )
            .expect("reserve")
            .expect("slot");
        unsafe { writer.payload_as::<i32>() }.fill(1);
        // No commit, no abort: the producer simply gives up.
        drop(writer);
    }
    let stats = ring.stats();
    assert_eq!(
        stats.free_slots, 2,
        "a slot was leaked by an abandoned writer"
    );
    assert_eq!(stats.published, 0);
    assert_eq!(stats.next_sequence, 1);
}

#[test]
fn direct_write_never_takes_a_slot_with_an_active_reader() {
    let ring = make_ring("ownership", 2, 4096);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    // Fill both slots through the direct-write path.
    for seq in 1..=2u64 {
        let mut writer = producer
            .reserve(
                DType::I32,
                &[16],
                Backpressure::Block,
                Duration::from_millis(500),
            )
            .expect("reserve")
            .expect("slot");
        unsafe { writer.payload_as::<i32>() }.fill(seq as i32);
        writer.commit();
    }

    let lease = consumer.next(Duration::from_millis(500)).unwrap();
    let held: Vec<i32> = unsafe { lease.as_slice::<i32>() }.to_vec();

    // Both slots carry this consumer's claim, so reserving must fail rather than
    // hand the producer a buffer someone is still reading.
    let refused = producer.reserve(
        DType::I32,
        &[16],
        Backpressure::Block,
        Duration::from_millis(150),
    );
    assert!(
        matches!(refused, Err(Error::RingFull)),
        "{:?}",
        refused.err()
    );
    assert_eq!(
        unsafe { lease.as_slice::<i32>() },
        &held[..],
        "the leased payload changed underneath the reader"
    );

    lease.release().unwrap();
    consumer
        .next(Duration::from_millis(500))
        .unwrap()
        .release()
        .unwrap();
    assert!(
        producer
            .reserve(
                DType::I32,
                &[16],
                Backpressure::Block,
                Duration::from_millis(500)
            )
            .unwrap()
            .is_some()
    );
}

#[test]
fn direct_write_honours_drop_newest() {
    let ring = make_ring("dropnewest", 2, 4096);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    for _ in 0..2 {
        let mut writer = producer
            .reserve(
                DType::I32,
                &[16],
                Backpressure::Block,
                Duration::from_millis(500),
            )
            .unwrap()
            .unwrap();
        unsafe { writer.payload_as::<i32>() }.fill(1);
        writer.commit();
    }
    let _a = consumer.next(Duration::from_millis(500)).unwrap();
    let _b = consumer.next(Duration::from_millis(500)).unwrap();

    let dropped = producer
        .reserve(
            DType::I32,
            &[16],
            Backpressure::DropNewest,
            Duration::from_millis(0),
        )
        .unwrap();
    assert!(dropped.is_none());
    assert_eq!(ring.stats().dropped, 1);
}

#[test]
fn oversized_reservation_is_refused() {
    let ring = make_ring("oversize", 2, 256);
    let producer = ring.producer().unwrap();
    let result = producer.reserve(
        DType::F32,
        &[1024],
        Backpressure::Block,
        Duration::from_millis(50),
    );
    assert!(matches!(result, Err(Error::Invalid(_))));
}

#[test]
fn copy_path_and_direct_path_interleave_on_one_ring() {
    // A producer may use whichever path suits each tensor; the stream a consumer
    // sees must be indistinguishable. The ring is sized to hold every
    // publication undrained, so this is about ordering and content, not
    // backpressure (which the tests above cover).
    let ring = make_ring("mixed", 16, 4096);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    for seq in 1..=10u64 {
        if seq % 2 == 0 {
            let source: Vec<i32> = (0..16).map(|i| seq as i32 * 1000 + i).collect();
            let bytes = unsafe {
                std::slice::from_raw_parts(source.as_ptr() as *const u8, source.len() * 4)
            };
            let view = TensorView::contiguous(DType::I32, &[16], bytes)
                .unwrap()
                .with_timestamp_ns(seq);
            producer
                .publish(&view, Backpressure::Block, Duration::from_millis(500))
                .unwrap()
                .unwrap();
        } else {
            let mut writer = producer
                .reserve(
                    DType::I32,
                    &[16],
                    Backpressure::Block,
                    Duration::from_millis(500),
                )
                .unwrap()
                .unwrap();
            writer.set_timestamp_ns(seq);
            for (i, v) in unsafe { writer.payload_as::<i32>() }.iter_mut().enumerate() {
                *v = seq as i32 * 1000 + i as i32;
            }
            writer.commit();
        }
    }

    for seq in 1..=10u64 {
        let lease = consumer.next(Duration::from_millis(500)).unwrap();
        assert_eq!(lease.sequence(), seq);
        assert_eq!(lease.meta().timestamp_ns, seq);
        let got: &[i32] = unsafe { lease.as_slice::<i32>() };
        for (i, v) in got.iter().enumerate() {
            assert_eq!(*v, seq as i32 * 1000 + i as i32);
        }
    }
}

/// Debug builds poison a freshly reserved payload, so a producer that commits
/// without filling it is caught by a test rather than shipping the previous
/// occupant's bytes. Release builds skip the poison: writing it would be exactly
/// the cost this path exists to avoid.
#[test]
#[cfg(debug_assertions)]
fn an_unfilled_payload_is_poisoned_in_debug_builds() {
    let ring = make_ring("poison", 2, 4096);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    let mut first = producer
        .reserve(
            DType::U8,
            &[32],
            Backpressure::Block,
            Duration::from_millis(500),
        )
        .unwrap()
        .unwrap();
    first.payload().fill(0x11);
    first.commit();
    consumer
        .next(Duration::from_millis(500))
        .unwrap()
        .release()
        .unwrap();

    // Reserve again (reusing that slot) and commit without writing anything.
    let writer = producer
        .reserve(
            DType::U8,
            &[32],
            Backpressure::Block,
            Duration::from_millis(500),
        )
        .unwrap()
        .unwrap();
    writer.commit();
    let lease = consumer.next(Duration::from_millis(500)).unwrap();
    assert_eq!(
        lease.data(),
        &[0xA5u8; 32],
        "an unfilled payload should read as poison, not as stale data"
    );
}
