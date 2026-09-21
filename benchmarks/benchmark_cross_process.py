#!/usr/bin/env python3
from __future__ import annotations

import argparse
import base64
import json
import multiprocessing as mp
import os
import statistics
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.request import Request, urlopen

import numpy as np

from tenzorbus import SharedTensorRing


def percentile(xs, p):
    ys = sorted(xs)
    return ys[min(len(ys) - 1, int((len(ys) - 1) * p))]


def _shm_consumer(name, count, ready, out):
    ring = SharedTensorRing.attach(name)
    consumer = ring.consumer()
    ready.set()
    lat = []
    try:
        for _ in range(count):
            with consumer.next(timeout=10) as lease:
                arr = lease.numpy()
                _ = float(arr.flat[0])
                lat.append((time.perf_counter_ns() - lease.meta.timestamp_ns) / 1e6)
                del arr
        out.put(lat)
    finally:
        consumer.close()
        ring.close(unlink=False)


class _Handler(BaseHTTPRequestHandler):
    latencies = []
    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        started_ns = int(self.headers.get("X-Started-Ns", "0"))
        obj = json.loads(body)
        raw = base64.b64decode(obj["data"])
        arr = np.frombuffer(raw, dtype=np.dtype(obj["dtype"])).reshape(obj["shape"])
        _ = float(arr.flat[0])
        if started_ns:
            self.__class__.latencies.append((time.perf_counter_ns() - started_ns) / 1e6)
        self.send_response(204)
        self.end_headers()
    def log_message(self, *args):
        pass


def summary(xs):
    return {
        "median_ms": statistics.median(xs),
        "p95_ms": percentile(xs, 0.95),
        "min_ms": min(xs),
        "max_ms": max(xs),
    }


def run(count=300, shape=(3,224,224)):
    arr = np.random.default_rng(7).standard_normal(shape, dtype=np.float32)

    name = f"xproc_{os.getpid()}_{time.time_ns()}"
    ring = SharedTensorRing.create(name, slot_count=8, slot_capacity=arr.nbytes + 4096, force=True)
    ready, out = mp.Event(), mp.Queue()
    proc = mp.Process(target=_shm_consumer, args=(name, count, ready, out))
    proc.start()
    assert ready.wait(5)
    try:
        # Small pacing prevents measuring intentional queue buildup; goal is transport latency.
        for _ in range(count):
            started = time.perf_counter_ns()
            ring.publish(arr, timestamp_ns=started, timeout=5)
            while True:
                # Keep at most one tensor outstanding so latency represents handoff rather than queueing.
                with ring.lock:
                    outstanding = 0
                    for i in range(ring.slot_count):
                        from tenzorbus import protocol as p
                        base = p.slot_base(i, ring.slot_capacity)
                        if p.read_u32(ring.buf, base + p.S_STATE) == p.STATE_COMMITTED and p.read_u16(ring.buf, base + p.S_READERS) > 0:
                            outstanding += 1
                if outstanding == 0:
                    break
                time.sleep(0.00005)
        shm_lat = out.get(timeout=10)
    finally:
        proc.join(10)
        ring.close(unlink=True)
    if proc.exitcode != 0:
        raise RuntimeError(f"consumer exited {proc.exitcode}")

    _Handler.latencies = []
    server = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    port = server.server_address[1]
    encoded = base64.b64encode(memoryview(arr).cast("B")).decode("ascii")
    payload = json.dumps({"dtype": str(arr.dtype), "shape": list(arr.shape), "data": encoded}).encode()
    try:
        for _ in range(count):
            started = time.perf_counter_ns()
            req = Request(f"http://127.0.0.1:{port}/tensor", data=payload, method="POST", headers={
                "Content-Type":"application/json", "X-Started-Ns":str(started)
            })
            with urlopen(req, timeout=5) as resp:
                resp.read()
        http_lat = list(_Handler.latencies)
    finally:
        server.shutdown(); server.server_close(); thread.join(timeout=2)

    return {
        "tensor": {"shape": list(shape), "dtype":"float32", "bytes":arr.nbytes},
        "iterations": count,
        "tenzorbus_cross_process": {
            "scope":"producer copy + shared-memory commit + separate-process acquire + zero-copy read",
            **summary(shm_lat),
        },
        "localhost_http_json": {
            "scope":"prebuilt base64 JSON payload over localhost HTTP + server JSON/base64 decode",
            "note":"excludes per-request base64/JSON encoding on sender, so this favors HTTP",
            **summary(http_lat),
        },
        "median_speedup": statistics.median(http_lat) / statistics.median(shm_lat),
    }


def main():
    ap=argparse.ArgumentParser(); ap.add_argument('--iterations',type=int,default=300); ap.add_argument('--output')
    a=ap.parse_args(); r=run(a.iterations); t=json.dumps(r,indent=2); print(t)
    if a.output: open(a.output,'w').write(t+'\n')

if __name__=='__main__': main()
