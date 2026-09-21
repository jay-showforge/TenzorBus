#!/usr/bin/env python3
from __future__ import annotations

import argparse
import base64
import json
import os
import statistics
import time

import numpy as np

from tenzorbus import SharedTensorRing


def pct(xs, p):
    ys = sorted(xs)
    i = min(len(ys) - 1, max(0, int(round((len(ys) - 1) * p))))
    return ys[i]


def bench(iterations: int, shape=(3, 224, 224)):
    rng = np.random.default_rng(42)
    arr = rng.standard_normal(shape, dtype=np.float32)
    ring_name = f"bench_{os.getpid()}_{time.time_ns()}"
    ring = SharedTensorRing.create(ring_name, slot_count=4, slot_capacity=arr.nbytes + 4096, force=True)
    consumer = ring.consumer()
    try:
        # Warmup.
        for _ in range(20):
            ring.publish(arr)
            with consumer.next() as lease:
                _ = float(lease.numpy().flat[0])

        shm_times = []
        for _ in range(iterations):
            t0 = time.perf_counter_ns()
            ring.publish(arr)
            with consumer.next() as lease:
                _ = float(lease.numpy().flat[0])
            shm_times.append((time.perf_counter_ns() - t0) / 1e6)

        # Deliberately representative of the anti-pattern TenzorBus is meant to avoid:
        # tensor bytes -> base64 -> JSON -> parse -> base64 decode -> ndarray view.
        json_times = []
        raw = memoryview(arr).cast("B")
        for _ in range(iterations):
            t0 = time.perf_counter_ns()
            payload = json.dumps({
                "dtype": str(arr.dtype),
                "shape": arr.shape,
                "data": base64.b64encode(raw).decode("ascii"),
            })
            obj = json.loads(payload)
            decoded = base64.b64decode(obj["data"])
            got = np.frombuffer(decoded, dtype=np.dtype(obj["dtype"])).reshape(obj["shape"])
            _ = float(got.flat[0])
            json_times.append((time.perf_counter_ns() - t0) / 1e6)

        result = {
            "environment": {
                "python": os.sys.version.split()[0],
                "pid": os.getpid(),
            },
            "tensor": {"shape": list(shape), "dtype": "float32", "bytes": arr.nbytes},
            "iterations": iterations,
            "tenzorbus_reference": {
                "scope": "producer copy into SHM + consumer zero-copy acquire/read/release",
                "median_ms": statistics.median(shm_times),
                "p95_ms": pct(shm_times, 0.95),
                "min_ms": min(shm_times),
            },
            "json_base64_reference": {
                "scope": "base64 encode + JSON serialize/parse + base64 decode + ndarray view",
                "median_ms": statistics.median(json_times),
                "p95_ms": pct(json_times, 0.95),
                "min_ms": min(json_times),
            },
        }
        result["median_speedup_vs_json_base64"] = result["json_base64_reference"]["median_ms"] / result["tenzorbus_reference"]["median_ms"]
        return result
    finally:
        consumer.close()
        ring.close(unlink=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--iterations", type=int, default=500)
    ap.add_argument("--output")
    args = ap.parse_args()
    result = bench(args.iterations)
    text = json.dumps(result, indent=2)
    print(text)
    if args.output:
        with open(args.output, "w", encoding="utf-8") as f:
            f.write(text + "\n")


if __name__ == "__main__":
    main()
