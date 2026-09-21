#!/usr/bin/env python3
"""Real TenzorPipe v0.3.2 -> TenzorBus -> N independent consumers.

TenzorPipe actually decodes the media; TenzorBus publishes each real video tensor
into its shared slab; every consumer is a separate OS process that verifies what it
received against TenzorPipe's own tensors, read independently from the same
`.tenzor` file. Nothing here is mocked and no consumer trusts the producer's word
for what the payload should be.

    MP4
     |
     v  real TenzorPipe engine (H.264 decode, resize, CHW float32 in [-1, 1])
    .tenzor  (Arrow IPC)
     |
     v  producer: one copy into a TenzorBus slot, carrying the media timestamp
    TenzorBus ring
     |
     +--> consumer A (detector role)   zero-copy NumPy view
     +--> consumer B (embedder role)   zero-copy PyTorch view
     +--> consumer C (preview role)    zero-copy NumPy view

Usage:

    # one-shot, prints a JSON report
    python3 demos/tenzorpipe_to_bus.py run --media clip.mp4 --consumers 3

    # the worker side, normally spawned by `run`
    python3 demos/tenzorpipe_to_bus.py consume --ring NAME --tenzor FILE --expect 20
"""
from __future__ import annotations

import argparse
import contextlib
import json
import os
import subprocess
import sys
import tempfile
import time

import numpy as np

# Video tensors are 3x224x224 float32 by default: 602,112 bytes per epoch.
DEFAULT_SLOT_BYTES = 3 * 224 * 224 * 4


# ---------------------------------------------------------------------------
# Backends
# ---------------------------------------------------------------------------


class RustBackend:
    """The Rust production transport, via the tenzorbus_rs extension module."""

    name = "rust"

    def __init__(self):
        import tenzorbus_rs

        self._m = tenzorbus_rs

    def create(self, name, slots, slot_bytes):
        return self._m.create(name, slots=slots, slot_bytes=slot_bytes, force=True)

    def attach(self, name):
        return self._m.attach(name)

    def unlink(self, name):
        with contextlib.suppress(Exception):
            self._m.unlink(name)

    def consumers_registered(self, ring):
        return ring.stats()["consumers"]

    def publish(self, producer, frame, timestamp_ns, timeout):
        return producer.publish(frame, timeout=timeout, timestamp_ns=timestamp_ns)

    def lease_numpy(self, lease):
        return lease.numpy()

    def lease_torch(self, lease):
        return lease.torch()

    def lease_info(self, lease):
        return {
            "sequence": lease.sequence,
            "slot_index": lease.slot_index,
            "timestamp_ns": lease.timestamp_ns,
            "dtype": lease.dtype,
            "shape": tuple(lease.shape),
            "nbytes": lease.nbytes,
            "data_address": lease.data_address,
        }


class ReferenceBackend:
    """The executed v0.1 Python reference ring. Kept runnable for comparison."""

    name = "reference"

    def __init__(self):
        from tenzorbus import SharedTensorRing

        self._ring_cls = SharedTensorRing

    def create(self, name, slots, slot_bytes):
        return self._ring_cls.create(
            name, slot_count=slots, slot_capacity=slot_bytes, force=True
        )

    def attach(self, name):
        return self._ring_cls.attach(name)

    def unlink(self, name):
        pass  # the reference ring unlinks on close

    def consumers_registered(self, ring):
        return ring.stats()["consumers"]

    def publish(self, producer, frame, timestamp_ns, timeout):
        # The reference ring publishes from the ring object itself.
        result = producer.publish(frame, timestamp_ns=timestamp_ns, timeout=timeout)
        if result is None:
            return None
        return {
            "sequence": result.sequence,
            "slot_index": result.slot_index,
            "nbytes": result.nbytes,
            "readers": result.readers,
            "timestamp_ns": timestamp_ns,
        }

    def lease_numpy(self, lease):
        return lease.numpy()

    def lease_torch(self, lease):
        return lease.torch()

    def lease_info(self, lease):
        m = lease.meta
        return {
            "sequence": m.sequence,
            "slot_index": m.slot_index,
            "timestamp_ns": m.timestamp_ns,
            "dtype": m.dtype,
            "shape": tuple(m.shape),
            "nbytes": m.nbytes,
            "data_address": None,  # the reference exposes no slot address
        }


def backend(kind):
    return RustBackend() if kind == "rust" else ReferenceBackend()


# ---------------------------------------------------------------------------
# TenzorPipe side
# ---------------------------------------------------------------------------


def ingest(media, out_path, *, resolution=None, window_sec=None, video_workers=None):
    """Run the real TenzorPipe engine. Returns its own result dict."""
    import tenzorpipe as tp

    return tp.ingest(
        media,
        out_path,
        resolution=resolution,
        window_sec=window_sec,
        video_workers=video_workers,
    )


def open_dataset(tenzor_path):
    import tenzorpipe as tp

    return tp.load(tenzor_path)


def source_frames(dataset):
    """Yield (index, C-contiguous float32 frame view, media timestamp in ns).

    The frame aliases TenzorPipe's memory-mapped Arrow buffer: this generator
    performs no copy of its own, so the only copy on the publish path is
    TenzorBus's single memcpy into the slot.
    """
    index = 0
    for batch in dataset.iter_batches():
        video = batch["video"]
        stamps = batch.get("video_timestamp_ms", batch["timestamp_ms"])
        for row in range(video.shape[0]):
            frame = np.asarray(video[row])
            yield index, frame, int(stamps[row]) * 1_000_000
            index += 1


# ---------------------------------------------------------------------------
# Consumer worker (a separate OS process)
# ---------------------------------------------------------------------------


def consume(args):
    be = backend(args.backend)
    ring = be.attach(args.ring)
    consumer = ring.consumer()
    dataset = open_dataset(args.tenzor)

    if args.ready_file:
        with open(args.ready_file, "w") as fh:
            fh.write("ready")

    report = {
        "role": args.role,
        "pid": os.getpid(),
        "backend": be.name,
        "received": 0,
        "content_mismatches": 0,
        "shape_mismatches": 0,
        "dtype_mismatches": 0,
        "timestamp_mismatches": 0,
        "sequence_gaps": 0,
        "out_of_order": 0,
        "zero_copy_numpy": 0,
        "zero_copy_torch": 0,
        "not_zero_copy": 0,
        "lease_violations": 0,
        "first_sequence": None,
        "last_sequence": None,
        # (sequence, slot) as observed by this consumer, so a harness can prove
        # the slot it read is the slot the producer wrote into.
        "slots": [],
    }
    rng = np.random.default_rng(os.getpid())
    previous = None
    started = time.monotonic()

    try:
        while report["received"] < args.expect:
            try:
                lease = consumer.next(timeout=args.timeout)
            except Exception as exc:  # timeout or transport error
                report["error"] = f"{type(exc).__name__}: {exc}"
                break

            info = be.lease_info(lease)
            seq = info["sequence"]
            # The publication order is the epoch order, so sequence N carries
            # TenzorPipe's epoch N-1. The consumer re-reads that epoch from the
            # .tenzor file itself rather than trusting the producer.
            expected_frame = np.asarray(dataset[seq - 1]["video"])
            expected_ts = int(dataset[seq - 1]["video_timestamp_ms"]) * 1_000_000

            if args.role == "embedder":
                view = be.lease_torch(lease)
                got_bytes = view.numpy().tobytes()
                address = view.data_ptr()
                shape = tuple(view.shape)
                dtype = str(view.dtype).replace("torch.", "")
            else:
                view = be.lease_numpy(lease)
                got_bytes = view.tobytes()
                address = view.__array_interface__["data"][0]
                shape = tuple(view.shape)
                dtype = str(view.dtype)

            if info["data_address"] is not None:
                if address == info["data_address"]:
                    key = (
                        "zero_copy_torch"
                        if args.role == "embedder"
                        else "zero_copy_numpy"
                    )
                    report[key] += 1
                else:
                    report["not_zero_copy"] += 1

            if shape != tuple(expected_frame.shape):
                report["shape_mismatches"] += 1
            if dtype != str(expected_frame.dtype):
                report["dtype_mismatches"] += 1
            if got_bytes != expected_frame.tobytes():
                report["content_mismatches"] += 1
            if info["timestamp_ns"] != expected_ts:
                report["timestamp_mismatches"] += 1

            if previous is None:
                report["first_sequence"] = seq
            else:
                if seq <= previous:
                    report["out_of_order"] += 1
                elif seq != previous + 1:
                    report["sequence_gaps"] += 1
            report["slots"].append([seq, info["slot_index"]])
            previous = seq
            report["last_sequence"] = seq
            report["received"] += 1

            del view
            if args.sleep_max_ms:
                time.sleep(float(rng.random()) * args.sleep_max_ms / 1000.0)

            if args.die_after and report["received"] == args.die_after:
                # SIGKILL while still holding the lease: no release, no cleanup.
                sys.stdout.write(json.dumps(report) + "\n")
                sys.stdout.flush()
                os.kill(os.getpid(), 9)

            try:
                release = getattr(lease, "release", None)
                if release is not None:
                    release()
            except Exception as exc:
                report["lease_violations"] += 1
                report["release_error"] = f"{type(exc).__name__}: {exc}"
    finally:
        report["elapsed_s"] = round(time.monotonic() - started, 4)
        with contextlib.suppress(Exception):
            dataset.close()
        with contextlib.suppress(Exception):
            consumer.close()

    print(json.dumps(report))
    ok = (
        report["received"] == args.expect
        and report["content_mismatches"] == 0
        and report["shape_mismatches"] == 0
        and report["dtype_mismatches"] == 0
        and report["timestamp_mismatches"] == 0
        and report["sequence_gaps"] == 0
        and report["out_of_order"] == 0
        and report["not_zero_copy"] == 0
        and report["lease_violations"] == 0
        and "error" not in report
    )
    return 0 if ok else 1


# ---------------------------------------------------------------------------
# Orchestrator
# ---------------------------------------------------------------------------

ROLES = ["detector", "embedder", "preview"]


def run(args):
    be = backend(args.backend)
    workdir = tempfile.TemporaryDirectory(prefix="tenzorbus-tp-")
    tenzor_path = os.path.join(workdir.name, "clip.tenzor")

    decode_started = time.monotonic()
    info = ingest(
        args.media,
        tenzor_path,
        resolution=args.resolution,
        window_sec=args.window_sec,
        video_workers=args.video_workers,
    )
    decode_s = time.monotonic() - decode_started

    dataset = open_dataset(tenzor_path)
    epochs = len(dataset)
    frame_bytes = int(np.prod(dataset.video_shape)) * 4
    slot_bytes = max(args.slot_bytes or frame_bytes, frame_bytes)

    ring_name = f"tp_{os.getpid()}_{time.time_ns() % 1_000_000}"
    ring = be.create(ring_name, args.slots, slot_bytes)

    children, ready_files = [], []
    for i in range(args.consumers):
        role = ROLES[i % len(ROLES)]
        ready = os.path.join(workdir.name, f"ready.{i}")
        cmd = [
            sys.executable, os.path.abspath(__file__), "consume",
            "--backend", args.backend,
            "--ring", ring_name,
            "--tenzor", tenzor_path,
            "--expect", str(epochs),
            "--role", role,
            "--ready-file", ready,
            "--timeout", str(args.consumer_timeout),
        ]
        if args.sleep_max_ms:
            cmd += ["--sleep-max-ms", str(args.sleep_max_ms)]
        if args.die_after and i == 0:
            cmd += ["--die-after", str(args.die_after)]
        children.append((role, subprocess.Popen(cmd, stdout=subprocess.PIPE, text=True)))
        ready_files.append(ready)

    deadline = time.monotonic() + 60
    while be.consumers_registered(ring) < args.consumers:
        if time.monotonic() > deadline:
            raise SystemExit("consumers never registered")
        time.sleep(0.02)

    producer = ring.producer() if hasattr(ring, "producer") else ring
    published, transport_s = 0, 0.0
    publish_latencies = []

    frames = list(source_frames(dataset))
    if args.inject == "swap-frames" and len(frames) >= 2:
        # Negative control: publish epoch 1's tensor under sequence 1 and epoch
        # 0's under sequence 2. Consumers re-derive the expected tensor from the
        # .tenzor file, so they must notice. A gate that cannot fail is not a gate.
        a, b = frames[0], frames[1]
        frames[0], frames[1] = (a[0], b[1], a[2]), (b[0], a[1], b[2])
    elif args.inject == "swap-timestamps" and len(frames) >= 2:
        a, b = frames[0], frames[1]
        frames[0], frames[1] = (a[0], a[1], b[2]), (b[0], b[1], a[2])

    for _, frame, ts_ns in frames:
        t0 = time.perf_counter()
        result = be.publish(producer, frame, ts_ns, args.publish_timeout)
        dt = time.perf_counter() - t0
        transport_s += dt
        publish_latencies.append(dt * 1000.0)
        if result is not None:
            published += 1
        del frame

    consumer_reports = []
    for role, child in children:
        out, _ = child.communicate(timeout=args.consumer_timeout + 60)
        lines = [line for line in out.strip().splitlines() if line.startswith("{")]
        parsed = json.loads(lines[-1]) if lines else {"role": role, "error": "no report"}
        parsed["exit_code"] = child.returncode
        consumer_reports.append(parsed)

    stats = ring.stats()
    latencies = sorted(publish_latencies)

    def pct(p):
        if not latencies:
            return None
        return round(latencies[min(len(latencies) - 1, int(len(latencies) * p))], 4)

    report = {
        "backend": be.name,
        "media": os.path.abspath(args.media),
        "tenzorpipe": _tenzorpipe_version(),
        "ingest": info,
        "epochs": epochs,
        "video_shape": list(dataset.video_shape),
        "audio_shape": list(dataset.audio_shape),
        "frame_bytes": frame_bytes,
        "slot_bytes": slot_bytes,
        "slots": args.slots,
        "consumers": args.consumers,
        "published": published,
        "decode_seconds": round(decode_s, 4),
        "transport_seconds": round(transport_s, 6),
        "publish_ms_median": pct(0.5),
        "publish_ms_p95": pct(0.95),
        "publish_ms_p99": pct(0.99),
        "ring_stats": {k: v for k, v in stats.items() if k != "name"},
        "consumer_reports": consumer_reports,
        "injected_fault": args.inject,
    }

    dataset.close()
    with contextlib.suppress(Exception):
        ring.close(unlink=True)
    be.unlink(ring_name)
    workdir.cleanup()

    print(json.dumps(report, indent=2))
    healthy = published == epochs and all(
        r.get("exit_code") == 0 for r in consumer_reports
    )
    if args.die_after:
        # One consumer was killed on purpose; it is expected to fail.
        healthy = published == epochs
    if args.inject != "none":
        # Inverted: the run is correct only if the consumers caught the fault.
        caught = any(
            r.get("content_mismatches", 0) > 0 or r.get("timestamp_mismatches", 0) > 0
            for r in consumer_reports
        )
        return 0 if caught else 1
    return 0 if healthy else 1


def _tenzorpipe_version():
    try:
        import tenzorpipe as tp

        return tp.__version__
    except Exception:
        return None


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("run", help="ingest media and publish it to N consumers")
    r.add_argument("--media", required=True)
    r.add_argument("--backend", default="rust", choices=["rust", "reference"])
    r.add_argument("--consumers", type=int, default=3)
    r.add_argument("--slots", type=int, default=8)
    r.add_argument("--slot-bytes", type=int, default=None)
    r.add_argument("--resolution", type=int, default=None)
    r.add_argument("--window-sec", type=float, default=None)
    r.add_argument("--video-workers", type=int, default=None)
    r.add_argument("--publish-timeout", type=float, default=30.0)
    r.add_argument("--consumer-timeout", type=float, default=60.0)
    r.add_argument("--sleep-max-ms", type=float, default=0.0)
    r.add_argument("--die-after", type=int, default=0,
                   help="SIGKILL the first consumer while it holds lease N")
    r.add_argument("--inject", default="none",
                   choices=["none", "swap-frames", "swap-timestamps"],
                   help="negative control: deliberately mis-publish so the "
                        "consumer-side verification is shown to be able to fail")

    c = sub.add_parser("consume", help="one consumer process")
    c.add_argument("--backend", default="rust", choices=["rust", "reference"])
    c.add_argument("--ring", required=True)
    c.add_argument("--tenzor", required=True)
    c.add_argument("--expect", type=int, required=True)
    c.add_argument("--role", default="detector", choices=ROLES)
    c.add_argument("--ready-file", default=None)
    c.add_argument("--timeout", type=float, default=60.0)
    c.add_argument("--sleep-max-ms", type=float, default=0.0)
    c.add_argument("--die-after", type=int, default=0)

    args = ap.parse_args(argv)
    return run(args) if args.cmd == "run" else consume(args)


if __name__ == "__main__":
    raise SystemExit(main())
