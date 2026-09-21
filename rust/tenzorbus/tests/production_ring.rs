//! Phase 2 acceptance: transport invariants and failure modes.
//!
//! Every fault that involves a process dying is driven through real OS
//! processes (`tenzorbus-lab`) and a real `SIGKILL`. Nothing here simulates a
//! crash by calling a cleanup function.

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tenzorbus::{Backpressure, DType, Error, Ring, RingOptions, TensorView};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn ring_name(tag: &str) -> String {
    format!(
        "t_{tag}_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn make_ring(tag: &str, slots: usize, capacity: usize) -> Ring {
    Ring::create(
        &ring_name(tag),
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

fn lab() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tenzorbus-lab"))
}

fn wait_json(child: Child) -> (bool, String) {
    let out = child.wait_with_output().expect("child output");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

fn field(json: &str, key: &str) -> i64 {
    let needle = format!("\"{key}\":");
    let start = json
        .find(&needle)
        .unwrap_or_else(|| panic!("no {key} in {json}"))
        + needle.len();
    let rest = &json[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(rest.len());
    rest[..end]
        .parse()
        .unwrap_or_else(|_| panic!("bad {key} in {json}"))
}

// ---------------------------------------------------------------------------
// Core transport invariants (the v0.1 acceptance floor, re-proved in Rust)
// ---------------------------------------------------------------------------

#[test]
fn bounded_ring_never_overwrites_an_active_reader() {
    let ring = make_ring("backpressure", 2, 4096);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    for seq in 1..=2u64 {
        let data = payload(seq, 16);
        let view = TensorView::contiguous(DType::I32, &[16], as_bytes(&data)).unwrap();
        producer
            .publish(&view, Backpressure::Block, Duration::from_millis(500))
            .unwrap()
            .unwrap();
    }

    // Hold the oldest publication. Both slots are now committed and one is
    // pinned, so the ring must refuse to recycle it.
    let lease = consumer.next(Duration::from_millis(500)).unwrap();
    assert_eq!(lease.sequence(), 1);
    let pinned_slot = lease.meta().slot_index;
    let before: Vec<u32> = unsafe { lease.as_slice::<u32>() }.to_vec();

    let data = payload(99, 16);
    let view = TensorView::contiguous(DType::I32, &[16], as_bytes(&data)).unwrap();
    // Both slots carry this consumer's reader bit — one leased, one still
    // undelivered — so neither is reclaimable and the publish must fail rather
    // than recycle a slot the consumer has a claim on.
    let blocked = producer.publish(&view, Backpressure::Block, Duration::from_millis(200));
    assert!(
        matches!(blocked, Err(Error::RingFull)),
        "producer recycled a pinned slot: {blocked:?}"
    );

    let after: Vec<u32> = unsafe { lease.as_slice::<u32>() }.to_vec();
    assert_eq!(
        before, after,
        "leased payload changed underneath the reader"
    );
    assert_eq!(lease.meta().slot_index, pinned_slot);
    lease.release().unwrap();

    // Once the reader lets go, the producer makes progress again.
    let second = consumer.next(Duration::from_millis(500)).unwrap();
    second.release().unwrap();
    producer
        .publish(&view, Backpressure::Block, Duration::from_millis(500))
        .unwrap()
        .unwrap();
}

#[test]
fn every_registered_consumer_sees_the_same_publication() {
    let ring = make_ring("fanout", 4, 4096);
    let producer = ring.producer().unwrap();
    let a = ring.consumer().unwrap();
    let b = ring.consumer().unwrap();
    let c = ring.consumer().unwrap();

    let data = payload(1, 32);
    let view = TensorView::contiguous(DType::I32, &[32], as_bytes(&data)).unwrap();
    let result = producer
        .publish(&view, Backpressure::Block, Duration::from_millis(500))
        .unwrap()
        .unwrap();
    assert_eq!(result.readers, 3);

    for consumer in [&a, &b, &c] {
        let lease = consumer.next(Duration::from_millis(500)).unwrap();
        assert_eq!(lease.sequence(), result.sequence);
        assert_eq!(lease.meta().slot_index, result.slot_index);
        assert_eq!(unsafe { lease.as_slice::<u32>() }, &data[..]);
    }
}

#[test]
fn lease_view_aliases_the_shared_slab() {
    let ring = make_ring("alias", 4, 4096);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    let data = payload(7, 64);
    let view = TensorView::contiguous(DType::I32, &[8, 8], as_bytes(&data)).unwrap();
    producer
        .publish(&view, Backpressure::Block, Duration::from_millis(500))
        .unwrap()
        .unwrap();

    let lease = consumer.next(Duration::from_millis(500)).unwrap();
    let ptr = lease.data().as_ptr() as usize;
    let snapshot = ring.snapshot();
    // The view must point into the mapping, not into a copy handed to the
    // consumer, and it must land exactly on the slot's payload offset.
    let expected_offset = tenzor_core::payload_base(lease.meta().slot_index, ring.slot_capacity());
    assert_eq!(
        ptr,
        ring.base_addr() + expected_offset,
        "consumer view is not the slab itself"
    );
    assert_eq!(ptr % 64, 0, "payload is not cache-line aligned");
    assert_eq!(
        &snapshot[expected_offset..expected_offset + lease.data().len()],
        lease.data(),
        "view content differs from the slab at the slot payload offset"
    );
    assert_eq!(lease.meta().shape(), &[8, 8]);
    assert_eq!(lease.meta().strides(), &[32, 4]);
}

#[test]
fn drop_newest_returns_none_and_counts_the_drop() {
    let ring = make_ring("dropnewest", 2, 4096);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    let data = payload(1, 16);
    let view = TensorView::contiguous(DType::I32, &[16], as_bytes(&data)).unwrap();
    for _ in 0..2 {
        producer
            .publish(&view, Backpressure::Block, Duration::from_millis(500))
            .unwrap()
            .unwrap();
    }
    let _held_a = consumer.next(Duration::from_millis(500)).unwrap();
    let _held_b = consumer.next(Duration::from_millis(500)).unwrap();

    let dropped = producer
        .publish(&view, Backpressure::DropNewest, Duration::from_millis(0))
        .unwrap();
    assert!(dropped.is_none(), "drop_newest must not block or allocate");
    assert_eq!(ring.stats().dropped, 1);
}

#[test]
fn sequence_wraparound_is_handled() {
    let ring = make_ring("wrap", 4, 4096);
    ring.set_next_sequence(u64::MAX - 2);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    let mut expected = Vec::new();
    for _ in 0..6 {
        let seq = ring.stats().next_sequence;
        expected.push(seq);
        let data = payload(seq, 16);
        let view = TensorView::contiguous(DType::I32, &[16], as_bytes(&data)).unwrap();
        producer
            .publish(&view, Backpressure::Block, Duration::from_millis(500))
            .unwrap()
            .unwrap();
        let lease = consumer.next(Duration::from_millis(500)).unwrap();
        assert_eq!(lease.sequence(), seq, "sequence mismatch across wraparound");
        assert_eq!(unsafe { lease.as_slice::<u32>() }, &data[..]);
    }
    // Proof the boundary was actually crossed rather than skipped.
    assert!(expected.contains(&u64::MAX));
    assert!(expected.contains(&0));
}

#[test]
fn random_shapes_ranks_and_dtypes_round_trip() {
    let ring = make_ring("shapes", 8, 1 << 16);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    let dtypes = [
        DType::U8,
        DType::I8,
        DType::I16,
        DType::I32,
        DType::I64,
        DType::F32,
        DType::F64,
        DType::Bool,
    ];
    let mut state = 0x1234_5678u64;
    for iteration in 0..400u64 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let dtype = dtypes[(state >> 33) as usize % dtypes.len()];
        let rank = 1 + ((state >> 11) as usize % tenzorbus::MAX_NDIM);
        let mut shape = vec![1u32; rank];
        shape[rank - 1] = 1 + ((state >> 21) as u32 % 64);
        if rank > 1 {
            shape[0] = 1 + ((state >> 27) as u32 % 4);
        }
        let elements: usize = shape.iter().map(|d| *d as usize).product();
        let nbytes = elements * dtype.itemsize();
        let bytes: Vec<u8> = (0..nbytes)
            .map(|i| (i as u64).wrapping_add(iteration) as u8)
            .collect();
        let view = TensorView::contiguous(dtype, &shape, &bytes).unwrap();
        producer
            .publish(&view, Backpressure::Block, Duration::from_millis(500))
            .unwrap()
            .unwrap();

        let lease = consumer.next(Duration::from_millis(500)).unwrap();
        assert_eq!(lease.meta().dtype, dtype);
        assert_eq!(lease.meta().shape(), &shape[..]);
        assert_eq!(lease.meta().nbytes, nbytes);
        assert_eq!(lease.data(), &bytes[..], "payload mismatch at {iteration}");
    }
}

#[test]
fn a_publication_is_never_delivered_twice_to_the_same_consumer() {
    // Regression: the per-slot reader bit stays set until release, so a scan
    // driven by the bit alone would hand the same publication back while the
    // first lease was still outstanding, and the second release would then
    // report a double-release on a slot that was never re-published.
    let ring = make_ring("once", 4, 4096);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    for seq in 1..=3u64 {
        let data = payload(seq, 16);
        let view = TensorView::contiguous(DType::I32, &[16], as_bytes(&data)).unwrap();
        producer
            .publish(&view, Backpressure::Block, Duration::from_millis(500))
            .unwrap()
            .unwrap();
    }

    let mut leases = Vec::new();
    for expected in 1..=3u64 {
        let lease = consumer.next(Duration::from_millis(500)).unwrap();
        assert_eq!(lease.sequence(), expected);
        leases.push(lease);
    }
    // A fourth take must block: there is nothing new, even though three slots
    // still carry this consumer's bit.
    assert!(matches!(
        consumer.next(Duration::from_millis(50)),
        Err(Error::Timeout)
    ));

    let mut slots: Vec<usize> = leases.iter().map(|l| l.meta().slot_index).collect();
    slots.sort_unstable();
    slots.dedup();
    assert_eq!(slots.len(), 3, "the same slot was leased more than once");

    for lease in leases {
        lease.release().expect("each lease releases exactly once");
    }
}

#[test]
fn media_timestamps_survive_the_transport() {
    // Phase 4 requirement: a slot must carry the source's capture time, not the
    // wall-clock time at publish, or a consumer cannot align a frame with its clip.
    let ring = make_ring("timestamps", 8, 4096);
    let producer = ring.producer().unwrap();
    let consumer = ring.consumer().unwrap();

    // Media time, deliberately far from wall clock: 0 ms, 500 ms, 1000 ms ...
    let media: Vec<u64> = (0..6).map(|i| i * 500 * 1_000_000).collect();
    for (i, ts) in media.iter().enumerate() {
        let data = payload(i as u64, 16);
        let view = TensorView::contiguous(DType::I32, &[16], as_bytes(&data))
            .unwrap()
            .with_timestamp_ns(*ts);
        producer
            .publish(&view, Backpressure::Block, Duration::from_millis(500))
            .unwrap()
            .unwrap();
    }
    for expected in &media {
        let lease = consumer.next(Duration::from_millis(500)).unwrap();
        assert_eq!(
            lease.meta().timestamp_ns,
            *expected,
            "slot did not carry the source timestamp"
        );
    }

    // Left unset, the ring still stamps something sane rather than zero.
    let data = payload(99, 16);
    let view = TensorView::contiguous(DType::I32, &[16], as_bytes(&data)).unwrap();
    producer
        .publish(&view, Backpressure::Block, Duration::from_millis(500))
        .unwrap()
        .unwrap();
    let lease = consumer.next(Duration::from_millis(500)).unwrap();
    assert!(lease.meta().timestamp_ns > 1_600_000_000_000_000_000);
}

#[test]
fn oversized_tensor_is_rejected_not_truncated() {
    let ring = make_ring("oversize", 2, 256);
    let producer = ring.producer().unwrap();
    let data = payload(1, 256);
    let view = TensorView::contiguous(DType::I32, &[256], as_bytes(&data)).unwrap();
    let result = producer.publish(&view, Backpressure::Block, Duration::from_millis(50));
    assert!(matches!(result, Err(Error::Invalid(_))), "{result:?}");
}

#[test]
fn a_second_live_producer_process_is_refused() {
    let ring = make_ring("single", 4, 4096);
    // Re-taking the role inside the owning process is fine.
    {
        let _first = ring.producer().unwrap();
        assert!(ring.producer().is_ok());
    }

    // Hand the role to another process, then try to take it back.
    let ready = std::env::temp_dir().join(format!("{}.ready", ring.name()));
    let mut child = lab()
        .args([
            "produce",
            "--ring",
            ring.name(),
            "--count",
            "0",
            "--hold-ms",
            "3000",
            "--ready-file",
            ready.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn producer");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "child producer never started");
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = std::fs::remove_file(&ready);

    let refused = ring.producer().err();
    assert!(
        matches!(refused, Some(Error::Capacity(_))),
        "a second live producer process was allowed in: {refused:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
    // Once the holder is gone the role is reclaimable, not permanently lost.
    assert!(ring.producer().is_ok());
}

#[test]
fn consumer_registry_is_bounded() {
    let ring = make_ring("bounded_registry", 2, 256);
    let mut held = Vec::new();
    for _ in 0..tenzorbus::MAX_CONSUMERS {
        held.push(ring.consumer().expect("registry slot"));
    }
    assert!(matches!(ring.consumer(), Err(Error::Capacity(_))));
    held.pop();
    assert!(ring.consumer().is_ok(), "registry slot must be reusable");
}

// ---------------------------------------------------------------------------
// Failure modes driven through real processes
// ---------------------------------------------------------------------------

#[test]
fn consumer_killed_while_holding_a_lease_does_not_pin_the_slot() {
    let ring = make_ring("reap", 2, 4096);
    let producer = ring.producer().unwrap();
    let ready = std::env::temp_dir().join(format!("{}.ready", ring.name()));

    let child = lab()
        .args([
            "consume",
            "--ring",
            ring.name(),
            "--expect",
            "1000",
            "--die-after",
            "1",
            "--timeout-ms",
            "5000",
            "--ready-file",
            ready.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn consumer");

    // Wait until the child has registered before publishing.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "consumer never registered");
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = std::fs::remove_file(&ready);
    // The registry count is the authoritative signal.
    while ring.stats().consumers == 0 {
        assert!(
            Instant::now() < deadline,
            "consumer never appeared in registry"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let data = payload(1, 16);
    let view = TensorView::contiguous(DType::I32, &[16], as_bytes(&data)).unwrap();
    producer
        .publish(&view, Backpressure::Block, Duration::from_millis(1000))
        .unwrap()
        .unwrap();

    // The child takes the lease and is SIGKILLed while holding it.
    let out = child.wait_with_output().expect("child");
    assert!(
        !out.status.success(),
        "the consumer was supposed to die holding a lease"
    );

    // Both slots must become usable again without any manual cleanup: the
    // producer's own liveness sweep has to release the dead reader's bit.
    for _ in 0..8 {
        producer
            .publish(&view, Backpressure::Block, Duration::from_millis(2000))
            .expect("ring stalled behind a dead consumer")
            .expect("slot");
    }
    let stats = ring.stats();
    assert!(
        stats.reaped >= 1,
        "dead consumer was never reaped: {stats:?}"
    );
    assert_eq!(stats.consumers, 0, "registry still counts a dead consumer");
}

#[test]
fn a_zombie_consumer_does_not_pin_a_slot() {
    // Regression, found by the Phase 4 media integration. When the dead consumer
    // is the producer's own child -- a supervisor spawning its workers, which is
    // an ordinary shape -- it stays in the process table as a zombie until
    // someone waits on it, and `kill(pid, 0)` reports a zombie as alive. Liveness
    // based on that alone let a killed consumer pin its slot forever. This test
    // deliberately never reaps the child, so the zombie state is what is under
    // test; `consumer_killed_while_holding_a_lease_does_not_pin_the_slot` reaps
    // and therefore cannot catch it.
    let ring = make_ring("zombie", 2, 4096);
    let producer = ring.producer().unwrap();
    let ready = std::env::temp_dir().join(format!("{}.ready", ring.name()));

    let mut child = lab()
        .args([
            "consume",
            "--ring",
            ring.name(),
            "--expect",
            "1000",
            "--die-after",
            "1",
            "--timeout-ms",
            "5000",
            "--ready-file",
            ready.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn consumer");

    let deadline = Instant::now() + Duration::from_secs(5);
    while ring.stats().consumers == 0 {
        assert!(Instant::now() < deadline, "consumer never registered");
        std::thread::sleep(Duration::from_millis(5));
    }

    let data = payload(1, 16);
    let view = TensorView::contiguous(DType::I32, &[16], as_bytes(&data)).unwrap();
    producer
        .publish(&view, Backpressure::Block, Duration::from_millis(1000))
        .unwrap()
        .unwrap();

    // Wait for the child to become a zombie without reaping it.
    let deadline = Instant::now() + Duration::from_secs(10);
    let stat = format!("/proc/{}/stat", child.id());
    loop {
        let text = std::fs::read_to_string(&stat).unwrap_or_default();
        let state = text
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .unwrap_or("")
            .to_string();
        if state == "Z" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "child never became a zombie (state {state:?})"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_file(&ready);

    // The ring must keep running while the zombie is still un-reaped.
    for _ in 0..8 {
        producer
            .publish(&view, Backpressure::Block, Duration::from_millis(2000))
            .expect("ring stalled behind a zombie consumer")
            .expect("slot");
    }
    let stats = ring.stats();
    assert!(
        stats.reaped >= 1,
        "zombie consumer was never reaped: {stats:?}"
    );
    assert_eq!(stats.consumers, 0);

    let _ = child.wait();
}

#[test]
fn producer_killed_mid_write_leaves_a_recoverable_slot() {
    let ring = make_ring("abandon", 2, 4096);

    let child = lab()
        .args(["abandon", "--ring", ring.name(), "--elements", "8"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn abandoner");
    let (ok, json) = wait_json(child);
    assert!(!ok, "the abandoner was supposed to die uncommitted");
    assert!(json.contains("abandoned_slot"), "{json}");

    // One slot is now STATE_WRITING owned by a dead pid. A fresh producer must
    // reclaim it rather than treating the ring as permanently short a slot.
    let producer = ring.producer().expect("producer after abandoned write");
    let consumer = ring.consumer().unwrap();
    let data = payload(1, 8);
    let view = TensorView::contiguous(DType::I32, &[8], as_bytes(&data)).unwrap();
    for _ in 0..6 {
        producer
            .publish(&view, Backpressure::Block, Duration::from_millis(2000))
            .expect("ring stalled behind an abandoned slot")
            .expect("slot");
        let lease = consumer.next(Duration::from_millis(1000)).unwrap();
        assert_eq!(unsafe { lease.as_slice::<u32>() }, &data[..]);
    }
    assert_eq!(ring.stats().free_slots + ring.stats().committed_slots, 2);
}

#[test]
fn eight_consumers_receive_every_publication_with_random_sleeps() {
    let publications = 2_000u64;
    let ring = make_ring("fan8", 16, 1 << 16);
    let mut children = Vec::new();
    let mut ready_files = Vec::new();
    for i in 0..8 {
        let ready = std::env::temp_dir().join(format!("{}.{i}.ready", ring.name()));
        children.push(
            lab()
                .args([
                    "consume",
                    "--ring",
                    ring.name(),
                    "--expect",
                    &publications.to_string(),
                    "--timeout-ms",
                    "20000",
                    "--sleep-max-us",
                    "120",
                    "--ready-file",
                    ready.to_str().unwrap(),
                ])
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn consumer"),
        );
        ready_files.push(ready);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while ring.stats().consumers < 8 {
        assert!(Instant::now() < deadline, "consumers never registered");
        std::thread::sleep(Duration::from_millis(5));
    }

    let producer = ring.producer().unwrap();
    for _ in 0..publications {
        let seq = ring.stats().next_sequence;
        let data = payload(seq, 1024);
        let view = TensorView::contiguous(DType::I32, &[1024], as_bytes(&data)).unwrap();
        producer
            .publish(&view, Backpressure::Block, Duration::from_millis(20_000))
            .expect("publish under 8-consumer backpressure")
            .expect("slot");
    }

    for (child, ready) in children.into_iter().zip(ready_files) {
        let (ok, json) = wait_json(child);
        let _ = std::fs::remove_file(&ready);
        assert!(ok, "consumer failed: {json}");
        assert_eq!(field(&json, "received"), publications as i64, "{json}");
        assert_eq!(field(&json, "bad"), 0, "corrupt payload observed: {json}");
        assert_eq!(field(&json, "gaps"), 0, "sequence gap observed: {json}");
        assert_eq!(field(&json, "first"), 1, "{json}");
        assert_eq!(field(&json, "last"), publications as i64, "{json}");
    }
    let stats = ring.stats();
    assert_eq!(stats.published, publications);
    assert_eq!(stats.dropped, 0);
}

#[test]
fn sustained_full_ring_under_block_policy_makes_progress() {
    // Two slots, one slow consumer: the ring is full essentially all the time.
    let publications = 1_500u64;
    let ring = make_ring("saturated", 2, 1 << 14);
    let ready = std::env::temp_dir().join(format!("{}.ready", ring.name()));
    let child = lab()
        .args([
            "consume",
            "--ring",
            ring.name(),
            "--expect",
            &publications.to_string(),
            "--timeout-ms",
            "20000",
            "--sleep-max-us",
            "300",
            "--ready-file",
            ready.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn consumer");
    let deadline = Instant::now() + Duration::from_secs(10);
    while ring.stats().consumers < 1 {
        assert!(Instant::now() < deadline, "consumer never registered");
        std::thread::sleep(Duration::from_millis(5));
    }

    let producer = ring.producer().unwrap();
    for _ in 0..publications {
        let seq = ring.stats().next_sequence;
        let data = payload(seq, 256);
        let view = TensorView::contiguous(DType::I32, &[256], as_bytes(&data)).unwrap();
        producer
            .publish(&view, Backpressure::Block, Duration::from_millis(20_000))
            .expect("producer starved on a permanently full ring")
            .expect("slot");
    }
    let (ok, json) = wait_json(child);
    let _ = std::fs::remove_file(&ready);
    assert!(ok, "consumer failed: {json}");
    assert_eq!(field(&json, "received"), publications as i64, "{json}");
    assert_eq!(field(&json, "bad"), 0, "{json}");
    assert_eq!(field(&json, "gaps"), 0, "{json}");
}

#[test]
fn drop_newest_under_saturation_never_corrupts_delivered_tensors() {
    let ring = make_ring("dropsat", 2, 1 << 14);
    let ready = std::env::temp_dir().join(format!("{}.ready", ring.name()));
    let child = lab()
        .args([
            "consume",
            "--ring",
            ring.name(),
            "--expect",
            "200",
            "--timeout-ms",
            "20000",
            "--sleep-max-us",
            "400",
            "--ready-file",
            ready.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn consumer");
    let deadline = Instant::now() + Duration::from_secs(10);
    while ring.stats().consumers < 1 {
        assert!(Instant::now() < deadline, "consumer never registered");
        std::thread::sleep(Duration::from_millis(5));
    }

    let producer = ring.producer().unwrap();
    let mut delivered = 0u64;
    while delivered < 200 {
        let seq = ring.stats().next_sequence;
        let data = payload(seq, 256);
        let view = TensorView::contiguous(DType::I32, &[256], as_bytes(&data)).unwrap();
        if producer
            .publish(&view, Backpressure::DropNewest, Duration::from_millis(0))
            .unwrap()
            .is_some()
        {
            delivered += 1;
        }
        std::thread::sleep(Duration::from_micros(50));
    }
    let (ok, json) = wait_json(child);
    let _ = std::fs::remove_file(&ready);
    assert!(ok, "consumer failed: {json}");
    // Gaps are expected under drop-newest; corruption is not.
    assert_eq!(field(&json, "bad"), 0, "corrupt payload under drop: {json}");
    assert!(
        ring.stats().dropped > 0,
        "the drop path was never exercised"
    );
}

#[test]
fn high_volume_single_consumer_stream_is_byte_exact() {
    // Scaled for CI. Override for a soak run:
    //   TENZORBUS_STRESS_PUBLICATIONS=10000000 cargo test --release
    let publications: u64 = std::env::var("TENZORBUS_STRESS_PUBLICATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50_000);
    let ring = make_ring("volume", 8, 4096);
    let ready = std::env::temp_dir().join(format!("{}.ready", ring.name()));
    let child = lab()
        .args([
            "consume",
            "--ring",
            ring.name(),
            "--expect",
            &publications.to_string(),
            "--timeout-ms",
            "60000",
            "--ready-file",
            ready.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn consumer");
    let deadline = Instant::now() + Duration::from_secs(10);
    while ring.stats().consumers < 1 {
        assert!(Instant::now() < deadline, "consumer never registered");
        std::thread::sleep(Duration::from_millis(5));
    }

    let producer = ring.producer().unwrap();
    let mut data = vec![0u32; 64];
    for _ in 0..publications {
        let seq = ring.stats().next_sequence;
        let base = seq.wrapping_mul(1_000_003) as u32;
        for (i, slot) in data.iter_mut().enumerate() {
            *slot = base.wrapping_add(i as u32);
        }
        let view = TensorView::contiguous(DType::I32, &[64], as_bytes(&data)).unwrap();
        producer
            .publish(&view, Backpressure::Block, Duration::from_millis(60_000))
            .expect("publish")
            .expect("slot");
    }
    let (ok, json) = wait_json(child);
    let _ = std::fs::remove_file(&ready);
    assert!(ok, "consumer failed: {json}");
    assert_eq!(field(&json, "received"), publications as i64, "{json}");
    assert_eq!(field(&json, "bad"), 0, "{json}");
    assert_eq!(field(&json, "gaps"), 0, "{json}");
}
