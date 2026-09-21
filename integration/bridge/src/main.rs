//! TenzorPipe v0.3.2 -> TenzorBus, with no producer copy.
//!
//! The engine's decoder resizes each selected picture **straight into a reserved
//! TenzorBus slot**. Nothing between the H.264 decoder and the consumers copies
//! the tensor: TenzorPipe asks this sink for a destination, the sink reserves a
//! slot and hands over its payload, the resize writes there, and the commit
//! publishes it.
//!
//! ```text
//! MP4 -> H.264 decode -> resize ----writes into----> TenzorBus slot
//!                                                     |- consumer A
//!                                                     |- consumer B
//!                                                     '- consumer C
//! ```
//!
//! Backpressure is real and it reaches the decoder: `reserve` blocks while every
//! slot is still held by a reader, so a slow consumer slows the decode rather
//! than growing a queue.
//!
//! Usage:
//!
//! ```text
//! tenzorbus-ingest stream --media clip.mp4 --ring frames --slots 8 [--copy-path]
//! ```
//!
//! `--copy-path` declines the destination offer and publishes with `publish`
//! instead, so the two paths can be compared on identical input.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tenzor_pipe::{EpochSink, parse_args};
use tenzorbus::{Backpressure, DType, Producer, Ring, RingOptions, SlotWriter, TensorView};

/// Publishes each epoch's video tensor into a TenzorBus ring.
struct BusSink<'p> {
    producer: &'p Producer,
    shape: [u32; 3],
    len: usize,
    timeout: Duration,
    /// Offer to fill the slot in place. False reproduces the copy path.
    direct: bool,
    /// The slot reserved for the epoch currently being decoded.
    pending: Option<SlotWriter<'p>>,
    published: u64,
    destinations_taken: u64,
    copied: u64,
    /// (sequence, slot) for every publication, so a harness can prove the slot a
    /// consumer read is the slot the decoder wrote into.
    slot_trace: Vec<(u64, usize)>,
    /// Negative control: perturb one element of this epoch before committing.
    corrupt_epoch: Option<usize>,
    first_sequence: Option<u64>,
    last_sequence: u64,
}

impl<'p> BusSink<'p> {
    fn new(producer: &'p Producer, resolution: usize, direct: bool, timeout: Duration) -> Self {
        let r = resolution as u32;
        Self {
            producer,
            shape: [3, r, r],
            len: 3 * resolution * resolution,
            timeout,
            direct,
            pending: None,
            published: 0,
            destinations_taken: 0,
            copied: 0,
            slot_trace: Vec::new(),
            corrupt_epoch: None,
            first_sequence: None,
            last_sequence: 0,
        }
    }

    fn record_slot(&mut self, sequence: u64, slot: usize) {
        self.slot_trace.push((sequence, slot));
    }

    fn record(&mut self, sequence: u64) {
        if self.first_sequence.is_none() {
            self.first_sequence = Some(sequence);
        }
        self.last_sequence = sequence;
        self.published += 1;
    }
}

impl EpochSink for BusSink<'_> {
    fn video_destination(
        &mut self,
        _epoch: usize,
        pts: i64,
        len: usize,
    ) -> Result<Option<&mut [f32]>> {
        if !self.direct {
            return Ok(None);
        }
        if len != self.len {
            bail!(
                "engine asked for {len} floats, ring slots hold {}",
                self.len
            );
        }
        // Blocking here is the point: backpressure from the consumers reaches the
        // decoder instead of turning into an unbounded queue.
        let mut writer = self
            .producer
            .reserve(DType::F32, &self.shape, Backpressure::Block, self.timeout)
            .map_err(|e| anyhow::anyhow!("reserve a TenzorBus slot: {e}"))?
            .context("ring had no reclaimable slot within the publish timeout")?;
        // Media time, not wall clock: this is what lets a consumer line the frame
        // up with the rest of the clip.
        writer.set_timestamp_ns(pts.max(0) as u64 * 1_000_000);
        self.destinations_taken += 1;
        self.pending = Some(writer);
        let writer = self.pending.as_mut().expect("just stored");
        Ok(Some(unsafe { writer.payload_as::<f32>() }))
    }

    fn epoch_ready(&mut self, epoch: usize, pts: i64, tensor: Option<&[f32]>) -> Result<()> {
        match (self.pending.take(), tensor) {
            // The decoder wrote into the slot this sink handed out.
            (Some(mut writer), None) => {
                if self.corrupt_epoch == Some(epoch) {
                    // Deliberately wrong, so a harness can confirm the
                    // consumer-side verification is capable of failing.
                    let payload = unsafe { writer.payload_as::<f32>() };
                    payload[0] += 1.0;
                }
                let slot = writer.slot_index();
                let result = writer.commit();
                self.record_slot(result.sequence, slot);
                self.record(result.sequence);
            }
            // No destination was taken, so the engine owns the tensor and it has
            // to be copied in. This is the path the parallel and concurrent
            // executors take, and the repeated-epoch case on this one.
            (None, Some(values)) => {
                let bytes = unsafe {
                    std::slice::from_raw_parts(values.as_ptr() as *const u8, values.len() * 4)
                };
                let view = TensorView::contiguous(DType::F32, &self.shape, bytes)
                    .map_err(|e| anyhow::anyhow!("build a tensor view: {e}"))?
                    .with_timestamp_ns(pts.max(0) as u64 * 1_000_000);
                let result = self
                    .producer
                    .publish(&view, Backpressure::Block, self.timeout)
                    .map_err(|e| anyhow::anyhow!("publish: {e}"))?
                    .context("ring had no reclaimable slot within the publish timeout")?;
                self.copied += 1;
                self.record_slot(result.sequence, result.slot_index);
                self.record(result.sequence);
            }
            // A destination was taken *and* a tensor arrived: the engine would be
            // contradicting itself, and committing either one could publish the
            // wrong bytes. Refuse loudly.
            (Some(writer), Some(_)) => {
                writer.abort();
                bail!("engine supplied a tensor for an epoch whose destination it had taken");
            }
            (None, None) => {
                bail!("engine reported an epoch with neither a tensor nor a destination")
            }
        }
        Ok(())
    }

    fn abort_epoch(&mut self, _epoch: usize) {
        if let Some(writer) = self.pending.take() {
            writer.abort();
        }
    }
}

fn flag(args: &[String], key: &str) -> bool {
    args.iter().any(|a| a == key)
}

fn value(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn number(args: &[String], key: &str, default: u64) -> u64 {
    value(args, key).map_or(default, |v| v.parse().unwrap_or(default))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("stream") {
        eprintln!(
            "usage: tenzorbus-ingest stream --media FILE --ring NAME [--slots N] \
             [--resolution N] [--window-sec F] [--slot-bytes N] [--timeout-s N] \
             [--copy-path] [--attach] [--ready-file PATH]"
        );
        std::process::exit(2);
    }

    let media = value(&args, "--media").context("--media is required")?;
    let ring_name = value(&args, "--ring").context("--ring is required")?;
    let resolution = number(&args, "--resolution", 224) as usize;
    let slots = number(&args, "--slots", 8) as usize;
    let frame_bytes = 3 * resolution * resolution * 4;
    let slot_bytes = number(&args, "--slot-bytes", frame_bytes as u64) as usize;
    let timeout = Duration::from_secs(number(&args, "--timeout-s", 60));
    let direct = !flag(&args, "--copy-path");
    let corrupt_epoch = value(&args, "--corrupt-epoch").and_then(|v| v.parse::<usize>().ok());

    let ring = if flag(&args, "--attach") {
        Ring::attach(&ring_name).map_err(|e| anyhow::anyhow!("attach ring: {e}"))?
    } else {
        Ring::create(
            &ring_name,
            RingOptions {
                slot_count: slots,
                slot_capacity: slot_bytes.max(frame_bytes),
                force: true,
            },
        )
        .map_err(|e| anyhow::anyhow!("create ring: {e}"))?
    };
    ring.keep_on_drop();
    let producer = ring
        .producer()
        .map_err(|e| anyhow::anyhow!("claim the producer role: {e}"))?;

    // Signal readiness only once the ring exists and the producer role is held,
    // so a harness can start consumers without racing us.
    if let Some(path) = value(&args, "--ready-file") {
        std::fs::write(path, b"ready").context("write the ready file")?;
    }

    // Wait for the consumer count the caller expects, so no epoch is published
    // before every consumer is registered and therefore addressed by it.
    if let Some(expected) = value(&args, "--await-consumers") {
        let expected: u32 = expected.parse().context("--await-consumers")?;
        let deadline = Instant::now() + Duration::from_secs(120);
        while ring.stats().consumers < expected {
            if Instant::now() > deadline {
                bail!(
                    "only {} of {expected} consumers registered",
                    ring.stats().consumers
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // `convert_with_sink` writes no file, so --output is a placeholder the engine
    // never touches. Sequential + one worker is the path that can hand the
    // decoder a destination.
    let mut argv: Vec<String> = vec![
        "tenzor".into(),
        "-i".into(),
        media.clone(),
        "-o".into(),
        "/dev/null/unused.tenzor".into(),
        "--quiet".into(),
        "--execution".into(),
        "sequential".into(),
        "--video-workers".into(),
        "1".into(),
        "--resolution".into(),
        resolution.to_string(),
    ];
    if let Some(window) = value(&args, "--window-sec") {
        argv.push("--window-sec".into());
        argv.push(window);
    }
    let cli = parse_args(argv).context("build TenzorPipe arguments")?;

    let started = Instant::now();
    let mut sink = BusSink::new(&producer, resolution, direct, timeout);
    sink.corrupt_epoch = corrupt_epoch;
    let summary = tenzor_pipe::convert_with_sink(&cli, &mut sink)?;
    let elapsed = started.elapsed();

    let stats = ring.stats();
    println!(
        "{{\"media\":\"{}\",\"ring\":\"{}\",\"mode\":\"{}\",\"epochs\":{},\
\"published\":{},\"destinations_taken\":{},\"producer_copies\":{},\
\"first_sequence\":{},\"last_sequence\":{},\"resolution\":{},\"frame_bytes\":{},\
\"slots\":{},\"elapsed_s\":{:.4},\"engine_seconds\":{:.4},\
\"ring_published\":{},\"ring_dropped\":{},\"ring_reaped\":{},\"slot_trace\":{}}}",
        media,
        ring_name,
        if direct { "direct" } else { "copy" },
        summary.epochs,
        sink.published,
        sink.destinations_taken,
        sink.copied,
        sink.first_sequence.unwrap_or(0),
        sink.last_sequence,
        resolution,
        frame_bytes,
        slots,
        elapsed.as_secs_f64(),
        summary.seconds,
        stats.published,
        stats.dropped,
        stats.reaped,
        {
            let pairs: Vec<String> = sink
                .slot_trace
                .iter()
                .map(|(seq, slot)| format!("[{seq},{slot}]"))
                .collect();
            format!("[{}]", pairs.join(","))
        },
    );

    if sink.published as usize != summary.epochs {
        bail!("published {} of {} epochs", sink.published, summary.epochs);
    }
    Ok(())
}
