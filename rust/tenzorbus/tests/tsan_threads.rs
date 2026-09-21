//! ThreadSanitizer target: the ring exercised by threads inside one process.
//!
//! Why this shape. TSan instruments memory accesses within a single process and
//! knows nothing about another process touching the same `MAP_SHARED` pages, so
//! it cannot see TenzorBus's cross-process traffic at all. What it *can* see is
//! the synchronisation itself: the atomic slot state, the reader bitmask, the
//! sequence word and the non-atomic payload `memcpy` they guard. Those are the
//! same code paths a separate process runs, and the two bugs Phase 2 found were
//! both in exactly that logic. So driving the producer and the consumers as
//! threads over one mapping puts the interesting part under the sanitizer, and
//! the cross-process behaviour stays covered by the real-process tests in
//! `production_ring.rs`.
//!
//! Run:
//!
//! ```text
//! scripts/run_tsan.sh
//! ```
//!
//! which is `RUSTC_BOOTSTRAP=1 RUSTFLAGS=-Zsanitizer=thread cargo test
//! --target x86_64-unknown-linux-gnu --test tsan_threads`. A clean run prints no
//! `WARNING: ThreadSanitizer` lines; the script fails the build if any appear.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use tenzorbus::{Backpressure, DType, Ring, RingOptions, TensorView};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_ring(tag: &str, slots: usize, capacity: usize) -> Ring {
    let name = format!(
        "tsan_{tag}_{}_{}",
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

fn payload(seq: u64, elements: usize) -> Vec<u32> {
    let base = seq.wrapping_mul(1_000_003) as u32;
    (0..elements).map(|i| base.wrapping_add(i as u32)).collect()
}

fn as_bytes(v: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 4) }
}

/// One producer thread and `consumers` consumer threads over one ring, with the
/// slot count deliberately smaller than the publication count so every slot is
/// recycled many times and the reclaim path is hammered.
fn race_the_ring(tag: &str, consumers: usize, slots: usize, publications: u64, elements: usize) {
    let ring = make_ring(tag, slots, elements * 4);
    let barrier = Arc::new(Barrier::new(consumers + 1));

    let mut handles = Vec::new();
    for _ in 0..consumers {
        let consumer = ring.consumer().expect("consumer");
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let mut received = 0u64;
            let mut bad = 0u64;
            let mut last: Option<u64> = None;
            while received < publications {
                let lease = match consumer.next(Duration::from_secs(30)) {
                    Ok(l) => l,
                    Err(e) => panic!("consumer starved after {received}: {e}"),
                };
                let seq = lease.sequence();
                // Reading the payload under the lease is the access TSan must
                // see as ordered against the producer's copy.
                let data: &[u32] = unsafe { lease.as_slice::<u32>() };
                let base = seq.wrapping_mul(1_000_003) as u32;
                for (i, v) in data.iter().enumerate() {
                    if *v != base.wrapping_add(i as u32) {
                        bad += 1;
                        break;
                    }
                }
                if let Some(previous) = last {
                    assert_eq!(seq, previous + 1, "out-of-order or duplicate delivery");
                }
                last = Some(seq);
                received += 1;
                lease.release().expect("release");
            }
            (received, bad)
        }));
    }

    let producer = ring.producer().expect("producer");
    barrier.wait();
    for seq in 1..=publications {
        let data = payload(seq, elements);
        let view = TensorView::contiguous(DType::I32, &[elements as u32], as_bytes(&data))
            .expect("view")
            .with_timestamp_ns(seq * 1_000_000);
        producer
            .publish(&view, Backpressure::Block, Duration::from_secs(30))
            .expect("publish")
            .expect("slot");
    }

    for handle in handles {
        let (received, bad) = handle.join().expect("consumer thread");
        assert_eq!(received, publications);
        assert_eq!(bad, 0, "corrupt payload observed under TSan");
    }
    assert_eq!(ring.stats().published, publications);
}

#[test]
fn one_producer_one_consumer_thread() {
    race_the_ring("1c", 1, 2, 4_000, 64);
}

#[test]
fn one_producer_four_consumer_threads() {
    race_the_ring("4c", 4, 3, 2_000, 64);
}

#[test]
fn heavy_slot_recycling_with_two_slots() {
    // Two slots and three readers: the reclaim path runs on nearly every publish.
    race_the_ring("recycle", 3, 2, 2_000, 256);
}

/// The direct-write path under the sanitizer.
///
/// `publish` copies into the slot and then commits; `reserve` hands the producer
/// the slot and the producer writes there before committing. The second shape is
/// the one a decoder uses, and it has a longer window between reservation and
/// commit, so it deserves its own TSan coverage rather than being assumed safe
/// because the copy path is.
fn race_the_ring_direct(
    tag: &str,
    consumers: usize,
    slots: usize,
    publications: u64,
    elements: usize,
) {
    let ring = make_ring(tag, slots, elements * 4);
    let barrier = Arc::new(Barrier::new(consumers + 1));

    let mut handles = Vec::new();
    for _ in 0..consumers {
        let consumer = ring.consumer().expect("consumer");
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let mut received = 0u64;
            let mut bad = 0u64;
            let mut last: Option<u64> = None;
            while received < publications {
                let lease = match consumer.next(Duration::from_secs(30)) {
                    Ok(l) => l,
                    Err(e) => panic!("consumer starved after {received}: {e}"),
                };
                let seq = lease.sequence();
                let data: &[u32] = unsafe { lease.as_slice::<u32>() };
                let base = seq.wrapping_mul(1_000_003) as u32;
                for (i, v) in data.iter().enumerate() {
                    if *v != base.wrapping_add(i as u32) {
                        bad += 1;
                        break;
                    }
                }
                if let Some(previous) = last {
                    assert_eq!(seq, previous + 1, "out-of-order or duplicate delivery");
                }
                last = Some(seq);
                received += 1;
                lease.release().expect("release");
            }
            (received, bad)
        }));
    }

    let producer = ring.producer().expect("producer");
    barrier.wait();
    for _ in 1..=publications {
        let mut writer = producer
            .reserve(
                DType::I32,
                &[elements as u32],
                Backpressure::Block,
                Duration::from_secs(30),
            )
            .expect("reserve")
            .expect("slot");
        let seq = writer.sequence();
        writer.set_timestamp_ns(seq * 1_000_000);
        // Generated in place: this write is the access TSan must see as ordered
        // against every consumer's read of the same slot.
        let base = seq.wrapping_mul(1_000_003) as u32;
        for (i, v) in unsafe { writer.payload_as::<u32>() }.iter_mut().enumerate() {
            *v = base.wrapping_add(i as u32);
        }
        writer.commit();
    }

    for handle in handles {
        let (received, bad) = handle.join().expect("consumer thread");
        assert_eq!(received, publications);
        assert_eq!(
            bad, 0,
            "corrupt payload observed under TSan on the direct path"
        );
    }
    assert_eq!(ring.stats().published, publications);
}

#[test]
fn direct_write_one_consumer_thread() {
    race_the_ring_direct("d1c", 1, 2, 4_000, 64);
}

#[test]
fn direct_write_four_consumer_threads() {
    race_the_ring_direct("d4c", 4, 3, 2_000, 64);
}

#[test]
fn direct_write_heavy_slot_recycling() {
    race_the_ring_direct("drecycle", 3, 2, 2_000, 256);
}

#[test]
fn direct_write_aborts_race_with_consumers() {
    // Abort returns the slot and the sequence number while readers are active on
    // other slots, so the abort path touches the registry lock and the reclaim
    // futex under contention.
    let ring = make_ring("dabort", 4, 4096);
    let producer = ring.producer().expect("producer");
    let consumer = ring.consumer().expect("consumer");
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let reader = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(lease) = consumer.next(Duration::from_millis(20)) {
                    let _: &[u32] = unsafe { lease.as_slice::<u32>() };
                    let _ = lease.release();
                }
            }
        })
    };

    for seq in 1..=2_000u64 {
        let mut writer = producer
            .reserve(
                DType::I32,
                &[16],
                Backpressure::Block,
                Duration::from_secs(30),
            )
            .expect("reserve")
            .expect("slot");
        unsafe { writer.payload_as::<u32>() }.fill(seq as u32);
        if seq % 3 == 0 {
            writer.abort();
        } else {
            writer.commit();
        }
    }
    stop.store(true, Ordering::Relaxed);
    reader.join().expect("reader thread");
}

#[test]
fn registration_races_with_publication() {
    // Consumers joining and leaving while the producer is mid-stream exercises
    // the registry lock, the reader-mask update and the liveness sweep together.
    let ring = make_ring("churn", 8, 4096);
    let producer = ring.producer().expect("producer");
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let churn = {
        let ring = ring.clone();
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let consumer = ring.consumer().expect("consumer");
                // Take whatever is available, then leave again.
                if let Ok(lease) = consumer.next(Duration::from_millis(50)) {
                    let _ = lease.release();
                }
                drop(consumer);
            }
        })
    };

    let sweeper = {
        let ring = ring.clone();
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                ring.sweep();
                std::thread::sleep(Duration::from_micros(200));
            }
        })
    };

    for seq in 1..=1_000u64 {
        let data = payload(seq, 64);
        let view = TensorView::contiguous(DType::I32, &[64], as_bytes(&data)).expect("view");
        producer
            .publish(&view, Backpressure::DropNewest, Duration::from_millis(0))
            .expect("publish");
    }
    stop.store(true, Ordering::Relaxed);
    churn.join().expect("churn thread");
    sweeper.join().expect("sweeper thread");
}
