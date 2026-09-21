//! Multi-process test/stress driver for the TenzorBus production ring.
//!
//! Every failure-mode test in `tests/` drives real OS processes through this
//! binary rather than forking inside the test harness, so `kill -9` means what
//! it says. It doubles as a hand-runnable stress tool:
//!
//! ```text
//! tenzorbus-lab create   --ring frames --slots 16 --capacity 1048576
//! tenzorbus-lab consume  --ring frames --expect 1000 --sleep-max-us 200
//! tenzorbus-lab produce  --ring frames --count 1000 --elements 1024
//! ```

use std::time::{Duration, Instant};

use tenzorbus::{Backpressure, DType, Ring, RingOptions, TensorView};

/// Deterministic payload: element `i` of sequence `s` is `s * 1_000_003 + i`.
/// A stale slot, a torn copy, or a mis-addressed publication all show up as a
/// mismatch on a specific element rather than as a vague checksum failure.
fn fill(seq: u64, elements: usize, out: &mut Vec<u32>) {
    out.clear();
    out.reserve(elements);
    let base = seq.wrapping_mul(1_000_003) as u32;
    for i in 0..elements {
        out.push(base.wrapping_add(i as u32));
    }
}

fn verify(seq: u64, data: &[u32]) -> Option<usize> {
    let base = seq.wrapping_mul(1_000_003) as u32;
    for (i, v) in data.iter().enumerate() {
        if *v != base.wrapping_add(i as u32) {
            return Some(i);
        }
    }
    None
}

fn arg(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn num(args: &[String], key: &str, default: u64) -> u64 {
    arg(args, key)
        .map(|v| v.parse().unwrap_or_else(|_| panic!("bad value for {key}")))
        .unwrap_or(default)
}

fn flag(args: &[String], key: &str) -> bool {
    args.iter().any(|a| a == key)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");
    let ring_name = arg(&args, "--ring").unwrap_or_else(|| "lab".to_string());

    match cmd {
        "create" => {
            let ring = Ring::create(
                &ring_name,
                RingOptions {
                    slot_count: num(&args, "--slots", 8) as usize,
                    slot_capacity: num(&args, "--capacity", 1 << 20) as usize,
                    force: true,
                },
            )
            .expect("create");
            ring.keep_on_drop();
            println!("{{\"created\":\"{}\"}}", ring.name());
        }
        "unlink" => {
            let _ = tenzorbus::shm::unlink(&ring_name);
            println!("{{\"unlinked\":\"{ring_name}\"}}");
        }
        "produce" => produce(&args, &ring_name),
        "consume" => consume(&args, &ring_name),
        "abandon" => abandon(&args, &ring_name),
        "stats" => {
            let ring = Ring::attach(&ring_name).expect("attach");
            ring.keep_on_drop();
            let s = ring.stats();
            println!(
                "{{\"published\":{},\"dropped\":{},\"reaped\":{},\"consumers\":{},\"free\":{},\"committed\":{},\"next_seq\":{}}}",
                s.published,
                s.dropped,
                s.reaped,
                s.consumers,
                s.free_slots,
                s.committed_slots,
                s.next_sequence
            );
        }
        _ => {
            eprintln!(
                "usage: tenzorbus-lab <create|produce|consume|abandon|stats|unlink> --ring NAME [...]"
            );
            std::process::exit(2);
        }
    }
}

fn produce(args: &[String], ring_name: &str) {
    let ring = Ring::attach(ring_name).expect("attach");
    ring.keep_on_drop();
    let producer = ring.producer().expect("producer");

    let count = num(args, "--count", 1000);
    let elements = num(args, "--elements", 1024) as usize;
    let timeout_ms = num(args, "--timeout-ms", 5000);
    let stagger_us = num(args, "--stagger-us", 0);
    let policy = if arg(args, "--policy").as_deref() == Some("drop_newest") {
        Backpressure::DropNewest
    } else {
        Backpressure::Block
    };
    // Randomised shapes exercise ndim/dtype/size variation in the header.
    let vary = flag(args, "--vary");
    if let Some(seq) = arg(args, "--start-sequence") {
        ring.set_next_sequence(seq.parse().expect("start-sequence"));
    }

    // Keep the producer role claimed for a while, so another process can prove
    // the single-producer contract is enforced.
    let hold_ms = num(args, "--hold-ms", 0);
    if let Some(path) = arg(args, "--ready-file") {
        std::fs::write(&path, b"ready").expect("ready file");
    }

    let mut buf: Vec<u32> = Vec::new();
    let mut published = 0u64;
    let mut dropped = 0u64;
    let started = Instant::now();
    let mut rng = Xorshift::new(0x5eed_1234 ^ std::process::id() as u64);

    let duration_s = num(args, "--duration-s", 0);
    for n in 0..count {
        if duration_s > 0 && started.elapsed().as_secs() >= duration_s {
            break;
        }
        let this_elements = if vary {
            1 + (rng.next() as usize % elements)
        } else {
            elements
        };
        let seq = ring.stats().next_sequence;
        fill(seq, this_elements, &mut buf);
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len() * 4) };
        let shape: Vec<u32> = if vary && this_elements % 4 == 0 {
            vec![(this_elements / 4) as u32, 4]
        } else {
            vec![this_elements as u32]
        };
        let view = TensorView::contiguous(DType::I32, &shape, bytes).expect("view");
        match producer.publish(&view, policy, Duration::from_millis(timeout_ms)) {
            Ok(Some(_)) => published += 1,
            Ok(None) => dropped += 1,
            Err(e) => {
                eprintln!("publish failed at {n}: {e}");
                println!("{{\"published\":{published},\"dropped\":{dropped},\"error\":\"{e}\"}}");
                std::process::exit(1);
            }
        }
        if stagger_us > 0 {
            std::thread::sleep(Duration::from_micros(stagger_us));
        }
    }
    if hold_ms > 0 {
        std::thread::sleep(Duration::from_millis(hold_ms));
    }
    let elapsed = started.elapsed();
    let (rss_kib, cpu_s) = rusage();
    println!(
        "{{\"published\":{published},\"dropped\":{dropped},\"elapsed_ms\":{:.3},\"per_sec\":{:.1},\"max_rss_kib\":{rss_kib},\"cpu_s\":{:.3}}}",
        elapsed.as_secs_f64() * 1000.0,
        published as f64 / elapsed.as_secs_f64().max(1e-9),
        cpu_s
    );
}

fn consume(args: &[String], ring_name: &str) {
    let ring = Ring::attach(ring_name).expect("attach");
    ring.keep_on_drop();
    let consumer = ring.consumer().expect("consumer");

    let expect = num(args, "--expect", 1000);
    let timeout_ms = num(args, "--timeout-ms", 10_000);
    let sleep_max_us = num(args, "--sleep-max-us", 0);
    // 0 = never die. Otherwise SIGKILL this process while holding lease #N,
    // which is the "consumer dies with an outstanding lease" fault.
    let die_after = num(args, "--die-after", 0);
    // Stop successfully once the stream has been quiet this long. Used by the
    // duration-bounded soaks, where the consumer cannot know the final count.
    let until_idle_ms = num(args, "--until-idle-ms", 0);
    let ready_file = arg(args, "--ready-file");

    if let Some(path) = &ready_file {
        std::fs::write(path, b"ready").expect("ready file");
    }

    let mut rng = Xorshift::new(0xc0ffee ^ std::process::id() as u64);
    let mut received = 0u64;
    let mut bad_elements = 0u64;
    let mut first = 0u64;
    let mut last = 0u64;
    let mut gaps = 0u64;
    let mut lease_violations = 0u64;
    let started = Instant::now();

    while received < expect {
        let wait_ms = if until_idle_ms > 0 {
            until_idle_ms
        } else {
            timeout_ms
        };
        let lease = match consumer.next(Duration::from_millis(wait_ms)) {
            Ok(l) => l,
            Err(_) if until_idle_ms > 0 && received > 0 => break,
            Err(e) => {
                println!(
                    "{{\"received\":{received},\"bad\":{bad_elements},\"first\":{first},\"last\":{last},\"gaps\":{gaps},\"lease_violations\":{lease_violations},\"error\":\"{e}\"}}"
                );
                std::process::exit(1);
            }
        };
        let seq = lease.sequence();
        let data: &[u32] = unsafe { lease.as_slice::<u32>() };
        if verify(seq, data).is_some() {
            bad_elements += 1;
        }
        if received == 0 {
            first = seq;
        } else if seq != last.wrapping_add(1) {
            gaps += 1;
        }
        last = seq;
        received += 1;

        if die_after > 0 && received == die_after {
            // Still holding the lease: no release, no unregister, no unwinding.
            unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        }
        if sleep_max_us > 0 {
            let us = rng.next() % sleep_max_us;
            std::thread::sleep(Duration::from_micros(us));
        }
        // Release explicitly: a slot recycled underneath an outstanding lease
        // reports here, instead of being swallowed by Drop.
        if lease.release().is_err() {
            lease_violations += 1;
        }
    }
    let elapsed = started.elapsed();
    let (rss_kib, cpu_s) = rusage();
    println!(
        "{{\"received\":{received},\"bad\":{bad_elements},\"first\":{first},\"last\":{last},\"gaps\":{gaps},\"lease_violations\":{lease_violations},\"elapsed_ms\":{:.3},\"max_rss_kib\":{rss_kib},\"cpu_s\":{:.3}}}",
        elapsed.as_secs_f64() * 1000.0,
        cpu_s
    );
}

/// Reserve a slot and exit without committing, leaving a STATE_WRITING slot
/// owned by a dead pid. Recovery is the ring's problem, which is the point.
fn abandon(args: &[String], ring_name: &str) {
    let ring = Ring::attach(ring_name).expect("attach");
    ring.keep_on_drop();
    let producer = ring.producer().expect("producer");
    let elements = num(args, "--elements", 16) as usize;
    let mut buf = Vec::new();
    fill(1, elements, &mut buf);
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len() * 4) };
    let view = TensorView::contiguous(DType::I32, &[elements as u32], bytes).expect("view");
    let slot = producer.reserve_without_commit(&view).expect("reserve");
    println!("{{\"abandoned_slot\":{slot}}}");
    use std::io::Write;
    std::io::stdout().flush().ok();
    unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
}

/// Peak RSS in KiB and CPU seconds for this process, so every stress run
/// reports its own cost instead of relying on an external timing tool.
fn rusage() -> (u64, f64) {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let cpu = ru.ru_utime.tv_sec as f64
        + ru.ru_utime.tv_usec as f64 / 1e6
        + ru.ru_stime.tv_sec as f64
        + ru.ru_stime.tv_usec as f64 / 1e6;
    (ru.ru_maxrss as u64, cpu)
}

struct Xorshift(u64);

impl Xorshift {
    fn new(seed: u64) -> Self {
        Xorshift(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}
