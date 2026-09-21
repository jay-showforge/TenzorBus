#!/usr/bin/env python3
"""Phase 5: a fair transport matrix -- TenzorBus against realistic local alternatives.

Methodology, stated up front because it is the part that matters
---------------------------------------------------------------

*What is measured.* One number per publication: the wall-clock interval from the
moment the producer has a tensor in hand and begins handing it over, to the moment
a consumer process has a usable array over those bytes. That interval is the
transport, and nothing else.

*What is deliberately excluded.* No media decode: the payload is generated once
before timing starts and reused, so TenzorPipe's decoder never runs inside a
measurement. No model inference: consumers run no model. Those are separate costs
and blending them into a transport number would make it meaningless.

*What every consumer does with the bytes, identically in every path.* It builds a
NumPy array over them and sums a fixed stride of 4096 elements. That work is the
same in all paths, so it cancels out of comparisons, and it forces real reads
rather than letting a zero-copy path skip touching memory. It is deliberately not
a full-buffer sum: that would add the same large constant to every path and
flatter the slower ones.

*Clock.* `time.monotonic_ns()`, which is system-wide on Linux and therefore
comparable across processes. The send timestamp travels with the payload: in the
slot's own timestamp field for TenzorBus, and as an 8-byte prefix (or header) for
the socket, pipe and HTTP paths. The prefix costs those paths 8 bytes out of
tens of kilobytes.

*Fan-out.* With N consumers, a point-to-point transport must deliver the payload N
times, because that is what those transports actually do. TenzorBus publishes once
and all N read the same slot. That asymmetry is the thing under measurement, not a
thumb on the scale.

*Copies per path* are counted analytically, not measured, and reported alongside.

Usage:
    benchmarks/benchmark_matrix.py run --output benchmark_matrix.json
    benchmarks/benchmark_matrix.py consume --path unix_socket --endpoint ... --expect N
"""
from __future__ import annotations

import argparse
import contextlib
import json
import math
import os
import platform
import resource
import socket
import struct
import subprocess
import sys
import tempfile
import time
from collections import Counter

import numpy as np

STRIDE_SAMPLES = 4096
HDR = struct.Struct("<q")  # send timestamp, monotonic ns
UNIX_PATHNAME = "af_unix_sock_stream_pathname"
UNIX_SOCKETPAIR = "af_unix_sock_stream_socketpair_inherited"

# Analytical copy counts, per publication, for N consumers. "user->kernel" and
# "kernel->user" are the copies the kernel performs for a stream transport.
COPY_MODEL = {
    "tenzorbus_copy": {
        "producer": 1,
        "per_consumer": 0,
        "detail": "one memcpy into the slot; every consumer reads that slot in place",
    },
    "tenzorbus_direct": {
        "producer": 1,
        "per_consumer": 0,
        "detail": "the write lands directly in the slot; filling from an existing array is still one copy",
    },
    "tenzorbus_direct_nofill": {
        "producer": 0,
        "per_consumer": 0,
        "detail": "a producer generating in place performs no copy at all; the generation itself is excluded",
    },
    "unix_socket": {
        "producer": 1,
        "per_consumer": 1,
        "detail": "write() copies user->kernel once per consumer, read() copies kernel->user",
    },
    "fifo": {
        "producer": 1,
        "per_consumer": 1,
        "detail": "same two kernel copies as a socket, through a named pipe",
    },
    "http_binary": {
        "producer": 1,
        "per_consumer": 2,
        "detail": "kernel copies both ways, plus the server buffering the body before handing it over",
    },
    "http_json_b64": {
        "producer": 3,
        "per_consumer": 3,
        "detail": "base64 encode, JSON serialise, kernel copy out; then kernel copy in, JSON parse, base64 decode",
    },
}


def rusage():
    ru = resource.getrusage(resource.RUSAGE_SELF)
    return {
        "max_rss_kib": ru.ru_maxrss,
        "cpu_s": round(ru.ru_utime + ru.ru_stime, 4),
    }


def select_unix_socket_transport(workdir):
    """Select a real AF_UNIX/SOCK_STREAM transport for this host.

    Some sandbox policies reject ``socket(AF_UNIX, ...)`` before a pathname is
    involved while still allowing ``socketpair(AF_UNIX, SOCK_STREAM)``.  The
    latter remains a real Unix stream socket and can be inherited by a child
    process across ``exec``.
    """
    probe_path = os.path.join(workdir, "unix-socket-probe")
    probe = None
    try:
        probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        probe.bind(probe_path)
        return UNIX_PATHNAME
    except OSError:
        left = right = None
        try:
            left, right = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
            return UNIX_SOCKETPAIR
        except OSError as exc:
            raise RuntimeError(
                "neither pathname AF_UNIX/SOCK_STREAM nor AF_UNIX/SOCK_STREAM "
                "socketpair is permitted"
            ) from exc
        finally:
            if left is not None:
                left.close()
            if right is not None:
                right.close()
    finally:
        if probe is not None:
            probe.close()
        with contextlib.suppress(OSError):
            os.unlink(probe_path)


def touch(array: np.ndarray) -> float:
    """The identical, deliberately small amount of work every consumer does."""
    flat = array.reshape(-1)
    step = max(1, flat.size // STRIDE_SAMPLES)
    return float(flat[::step].sum())


def summarise(latencies_ms, label, extra=None):
    values = sorted(latencies_ms)
    if not values:
        return {"label": label, "samples": 0}

    def pct(p):
        return round(values[min(len(values) - 1, int(len(values) * p))], 4)

    out = {
        "label": label,
        "samples": len(values),
        "median_ms": pct(0.5),
        "p95_ms": pct(0.95),
        "p99_ms": pct(0.99),
        "min_ms": round(values[0], 4),
        "max_ms": round(values[-1], 4),
    }
    if extra:
        out.update(extra)
    return out


# ---------------------------------------------------------------------------
# Consumers, one per transport. Each prints a JSON summary on stdout.
# ---------------------------------------------------------------------------


def consume_tenzorbus(args):
    import tenzorbus_rs as tb

    ring = tb.attach(args.endpoint)
    consumer = ring.consumer()
    if args.ready_file:
        open(args.ready_file, "w").write("ready")
    latencies, checksum = [], 0.0
    for _ in range(args.expect):
        with consumer.next(timeout=args.timeout) as lease:
            arrival = time.monotonic_ns()
            view = lease.numpy()
            checksum += touch(view)
            latencies.append((arrival - lease.timestamp_ns) / 1e6)
            del view
    consumer.close()
    return latencies, {"checksum": checksum, "zero_copy": True}


def consume_stream(args, sock_or_fd, nbytes):
    """Shared reader for the socket and FIFO paths."""
    latencies, checksum = [], 0.0
    total = HDR.size + nbytes
    buf = bytearray(total)
    for _ in range(args.expect):
        got = 0
        while got < total:
            n = sock_or_fd.recv_into(memoryview(buf)[got:], total - got)
            if n == 0:
                raise RuntimeError("peer closed early")
            got += n
        arrival = time.monotonic_ns()
        (sent,) = HDR.unpack_from(buf, 0)
        view = np.frombuffer(buf, dtype=np.float32, count=nbytes // 4, offset=HDR.size)
        checksum += touch(view)
        latencies.append((arrival - sent) / 1e6)
    return latencies, {"checksum": checksum, "zero_copy": False}


class _FdReader:
    def __init__(self, fd):
        self.fd = fd

    def recv_into(self, view, n):
        return os.readv(self.fd, [view[:n]])


def consume_unix_socket(args):
    if args.socket_fd is not None:
        conn = socket.socket(fileno=args.socket_fd)
        if args.ready_file:
            open(args.ready_file, "w").write("ready")
        try:
            return consume_stream(args, conn, args.nbytes)
        finally:
            conn.close()

    if not args.endpoint:
        raise ValueError("pathname Unix socket consumer requires --endpoint")
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(args.endpoint)
    server.listen(1)
    if args.ready_file:
        open(args.ready_file, "w").write("ready")
    conn, _ = server.accept()
    try:
        return consume_stream(args, conn, args.nbytes)
    finally:
        conn.close()
        server.close()
        with contextlib.suppress(OSError):
            os.unlink(args.endpoint)


def consume_fifo(args):
    if args.ready_file:
        open(args.ready_file, "w").write("ready")
    fd = os.open(args.endpoint, os.O_RDONLY)
    try:
        return consume_stream(args, _FdReader(fd), args.nbytes)
    finally:
        os.close(fd)


def consume_http(args, json_mode):
    import base64
    from http.server import BaseHTTPRequestHandler, HTTPServer

    state = {"latencies": [], "checksum": 0.0, "left": args.expect}

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *a):
            pass

        def do_POST(self):
            length = int(self.headers["Content-Length"])
            body = self.rfile.read(length)
            arrival = time.monotonic_ns()
            if json_mode:
                doc = json.loads(body)
                raw = base64.b64decode(doc["data"])
                sent = doc["sent_ns"]
                view = np.frombuffer(raw, dtype=np.float32)
            else:
                sent = int(self.headers["X-Sent-Ns"])
                view = np.frombuffer(body, dtype=np.float32)
            state["checksum"] += touch(view)
            state["latencies"].append((arrival - sent) / 1e6)
            state["left"] -= 1
            self.send_response(204)
            self.send_header("Content-Length", "0")
            self.end_headers()

    host, port = args.endpoint.split(":")
    server = HTTPServer((host, int(port)), Handler)
    if args.ready_file:
        open(args.ready_file, "w").write("ready")
    while state["left"] > 0:
        server.handle_request()
    server.server_close()
    return state["latencies"], {"checksum": state["checksum"], "zero_copy": False}


CONSUMERS = {
    "tenzorbus_copy": consume_tenzorbus,
    "tenzorbus_direct": consume_tenzorbus,
    "tenzorbus_direct_nofill": consume_tenzorbus,
    "unix_socket": consume_unix_socket,
    "fifo": consume_fifo,
    "http_binary": lambda a: consume_http(a, False),
    "http_json_b64": lambda a: consume_http(a, True),
}


def consume(args):
    latencies, extra = CONSUMERS[args.path](args)
    extra.update(rusage())
    print(json.dumps(summarise(latencies, args.path, extra)))
    return 0


# ---------------------------------------------------------------------------
# Producers
# ---------------------------------------------------------------------------


def produce_tenzorbus(ring_name, payload, iterations, mode):
    """`mode` is one of copy, direct, direct_nofill. See PATH_NOTES."""
    import tenzorbus_rs as tb

    ring = tb.attach(ring_name)
    producer = ring.producer()
    shape = payload.shape
    started = time.monotonic_ns()
    if mode == "copy":
        for _ in range(iterations):
            # The timestamp is evaluated before publish runs, so the interval the
            # consumer computes includes the producer's memcpy into the slot --
            # the same accounting the socket and HTTP paths get.
            producer.publish(payload, timeout=60.0, timestamp_ns=time.monotonic_ns())
    elif mode == "direct":
        for _ in range(iterations):
            writer = producer.reserve("float32", shape, timeout=60.0)
            # Clock starts BEFORE the fill, so this is directly comparable to
            # every other path: producer begins handing over -> consumer has it.
            writer.set_timestamp_ns(time.monotonic_ns())
            array = writer.numpy()
            np.copyto(array, payload)
            del array
            writer.commit()
    elif mode == "direct_nofill":
        for _ in range(iterations):
            writer = producer.reserve("float32", shape, timeout=60.0)
            array = writer.numpy()
            np.copyto(array, payload)
            del array
            # Clock starts AFTER the fill. This is the transport floor for a
            # producer that GENERATES its tensor in place (a decoder writing its
            # output into the slot): the generation is excluded for the same
            # reason media decode is excluded everywhere else. It is not
            # comparable to the other paths, and is reported as a floor only.
            writer.set_timestamp_ns(time.monotonic_ns())
            writer.commit()
    else:
        raise ValueError(mode)
    elapsed_s = (time.monotonic_ns() - started) / 1e9
    return elapsed_s, ring.stats()


def produce_stream(endpoints, payload, iterations, kind, connected_sockets=None):
    raw = payload.tobytes()
    sinks = []
    if connected_sockets is not None:
        sinks = [sock.sendall for sock in connected_sockets]
    else:
        for endpoint in endpoints:
            if kind == "unix_socket":
                s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                s.connect(endpoint)
                sinks.append(s.sendall)
            else:
                fd = os.open(endpoint, os.O_WRONLY)
                sinks.append(lambda buf, fd=fd: os.write(fd, buf))
    started = time.monotonic_ns()
    for _ in range(iterations):
        for send in sinks:
            send(HDR.pack(time.monotonic_ns()) + raw)
    elapsed_s = (time.monotonic_ns() - started) / 1e9
    return elapsed_s, None


def produce_http(endpoints, payload, iterations, json_mode):
    import base64
    import http.client

    raw = payload.tobytes()
    # For the JSON path the base64/JSON build is part of the transport cost and is
    # therefore done per publication, inside the timed region. Prebuilding it (as
    # the v0.1 reference benchmark did) flatters HTTP by excluding work a real
    # sender cannot avoid.
    conns = []
    for endpoint in endpoints:
        host, port = endpoint.split(":")
        c = http.client.HTTPConnection(host, int(port))
        c.connect()
        conns.append(c)
    started = time.monotonic_ns()
    for _ in range(iterations):
        for c in conns:
            if json_mode:
                body = json.dumps(
                    {"sent_ns": time.monotonic_ns(), "data": base64.b64encode(raw).decode()}
                ).encode()
                headers = {"Content-Type": "application/json"}
            else:
                body = raw
                headers = {
                    "Content-Type": "application/octet-stream",
                    "X-Sent-Ns": str(time.monotonic_ns()),
                }
            c.request("POST", "/", body=body, headers=headers)
            c.getresponse().read()
    elapsed_s = (time.monotonic_ns() - started) / 1e9
    for c in conns:
        c.close()
    return elapsed_s, None


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------


def _child_report(child, path, index, iterations):
    try:
        out, err = child.communicate(timeout=180)
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(f"{path} consumer {index} timed out") from exc
    if child.returncode != 0:
        detail = (err or out or "no child output").strip()
        raise RuntimeError(
            f"{path} consumer {index} exited {child.returncode}: {detail}"
        )
    lines = [line for line in out.strip().splitlines() if line.startswith("{")]
    if not lines:
        raise RuntimeError(f"{path} consumer {index} produced no JSON report")
    try:
        report = json.loads(lines[-1])
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"{path} consumer {index} emitted invalid JSON") from exc
    if report.get("error"):
        raise RuntimeError(f"{path} consumer {index}: {report['error']}")
    if report.get("samples") != iterations:
        raise RuntimeError(
            f"{path} consumer {index} delivered {report.get('samples')} of "
            f"{iterations} publications"
        )
    return report


def validate_case(case):
    if case.get("error"):
        raise ValueError(f"errored case: {case['error']}")
    required = (
        "latency_median_ms",
        "latency_p95_ms",
        "latency_p99_ms",
        "publications_per_s",
    )
    for key in required:
        value = case.get(key)
        if not isinstance(value, (int, float)) or not math.isfinite(value):
            raise ValueError(f"{case.get('path')}: invalid {key}: {value!r}")
    consumers = case.get("consumers")
    iterations = case.get("iterations")
    delivered = case.get("delivered_per_consumer")
    if not isinstance(delivered, list) or len(delivered) != consumers:
        raise ValueError(f"{case.get('path')}: missing consumer delivery reports")
    if any(count != iterations for count in delivered):
        raise ValueError(f"{case.get('path')}: incomplete delivery: {delivered}")
    if case.get("all_consumers_received_everything") is not True:
        raise ValueError(f"{case.get('path')}: delivery verification is not true")
    if case.get("path") == "unix_socket" and case.get("unix_socket_transport") not in {
        UNIX_PATHNAME,
        UNIX_SOCKETPAIR,
    }:
        raise ValueError("Unix row does not identify a real AF_UNIX/SOCK_STREAM transport")


def validate_matrix(report, plan, paths, unix_socket_transport):
    cases = report.get("cases", [])
    if len(cases) != len(plan):
        raise ValueError(f"expected {len(plan)} cases, got {len(cases)}")
    expected = [(path, nbytes, consumers) for path, nbytes, consumers in plan]
    actual = [
        (case.get("path"), case.get("payload_bytes"), case.get("consumers"))
        for case in cases
    ]
    if actual != expected:
        raise ValueError("completed cases do not match the planned matrix")
    for case in cases:
        validate_case(case)
    counts = Counter(case["path"] for case in cases)
    for path in paths:
        if counts[path] != 7:
            raise ValueError(f"{path}: expected 7 complete cases, got {counts[path]}")
    if "unix_socket" in paths:
        unix_rows = [case for case in cases if case["path"] == "unix_socket"]
        if len(unix_rows) != 7:
            raise ValueError(f"expected 7 Unix socket rows, got {len(unix_rows)}")
        if any(
            row.get("unix_socket_transport") != unix_socket_transport
            for row in unix_rows
        ):
            raise ValueError("Unix socket transport identity changed within the matrix")


def run_case(
    path,
    nbytes,
    consumers,
    iterations,
    workdir,
    port_base,
    unix_socket_transport=None,
):
    payload = np.linspace(-1, 1, nbytes // 4, dtype=np.float32)
    endpoints, ready_files = [], []
    ring_name = None
    ring = None
    tb = None
    producer_sockets = []

    if path.startswith("tenzorbus"):
        import tenzorbus_rs as tb

        ring_name = f"bm_{os.getpid()}_{time.monotonic_ns() % 1_000_000}"
        ring = tb.create(ring_name, slots=8, slot_bytes=nbytes, force=True)
        endpoints = [ring_name] * consumers
    elif path == "unix_socket":
        if unix_socket_transport == UNIX_PATHNAME:
            endpoints = [os.path.join(workdir, f"sock.{i}") for i in range(consumers)]
        elif unix_socket_transport == UNIX_SOCKETPAIR:
            endpoints = [f"socketpair:{i}" for i in range(consumers)]
        else:
            raise ValueError(f"invalid Unix socket transport: {unix_socket_transport!r}")
    elif path == "fifo":
        endpoints = [os.path.join(workdir, f"fifo.{i}") for i in range(consumers)]
        for e in endpoints:
            os.mkfifo(e)
    else:
        endpoints = [f"127.0.0.1:{port_base + i}" for i in range(consumers)]

    children = []
    try:
        for i, endpoint in enumerate(endpoints):
            ready = os.path.join(workdir, f"ready.{path}.{i}")
            ready_files.append(ready)
            command = [
                sys.executable,
                os.path.abspath(__file__),
                "consume",
                "--path",
                path,
                "--expect",
                str(iterations),
                "--nbytes",
                str(nbytes),
                "--ready-file",
                ready,
                "--timeout",
                "60",
            ]
            popen_kwargs = {
                "stdout": subprocess.PIPE,
                "stderr": subprocess.PIPE,
                "text": True,
            }
            if path == "unix_socket" and unix_socket_transport == UNIX_SOCKETPAIR:
                producer_sock, child_sock = socket.socketpair(
                    socket.AF_UNIX, socket.SOCK_STREAM
                )
                producer_sockets.append(producer_sock)
                command.extend(["--socket-fd", str(child_sock.fileno())])
                popen_kwargs["pass_fds"] = (child_sock.fileno(),)
                child = subprocess.Popen(command, **popen_kwargs)
                child_sock.close()
            else:
                command.extend(["--endpoint", endpoint])
                child = subprocess.Popen(command, **popen_kwargs)
            children.append(child)

        deadline = time.monotonic() + 60
        while not all(os.path.exists(r) for r in ready_files):
            for index, child in enumerate(children):
                if child.poll() is not None:
                    _child_report(child, path, index, iterations)
            if time.monotonic() > deadline:
                raise RuntimeError(f"{path}: consumers never became ready")
            time.sleep(0.01)
        if path.startswith("tenzorbus"):
            while ring.stats()["consumers"] < consumers:
                for index, child in enumerate(children):
                    if child.poll() is not None:
                        _child_report(child, path, index, iterations)
                if time.monotonic() > deadline:
                    raise RuntimeError(f"{path}: consumers never registered")
                time.sleep(0.01)

        if path == "tenzorbus_copy":
            elapsed_s, stats = produce_tenzorbus(ring_name, payload, iterations, "copy")
        elif path == "tenzorbus_direct":
            elapsed_s, stats = produce_tenzorbus(ring_name, payload, iterations, "direct")
        elif path == "tenzorbus_direct_nofill":
            elapsed_s, stats = produce_tenzorbus(
                ring_name, payload, iterations, "direct_nofill"
            )
        elif path in ("unix_socket", "fifo"):
            elapsed_s, stats = produce_stream(
                endpoints,
                payload,
                iterations,
                path,
                connected_sockets=producer_sockets or None,
            )
        elif path == "http_binary":
            elapsed_s, stats = produce_http(endpoints, payload, iterations, False)
        else:
            elapsed_s, stats = produce_http(endpoints, payload, iterations, True)

        reports = [
            _child_report(child, path, index, iterations)
            for index, child in enumerate(children)
        ]
        medians = [report["median_ms"] for report in reports]
        p95s = [report["p95_ms"] for report in reports]
        p99s = [report["p99_ms"] for report in reports]
        model = COPY_MODEL[path]
        case = {
        "path": path,
        "payload_bytes": nbytes,
        "consumers": consumers,
        "iterations": iterations,
        "delivered_per_consumer": [r.get("samples") for r in reports],
        "latency_median_ms": round(float(np.median(medians)), 4) if medians else None,
        "latency_p95_ms": round(max(p95s), 4) if p95s else None,
        "latency_p99_ms": round(max(p99s), 4) if p99s else None,
        "producer_elapsed_s": round(elapsed_s, 4),
        "publications_per_s": round(iterations / elapsed_s, 1) if elapsed_s else None,
        "producer_MiB_per_s": round(iterations * nbytes / elapsed_s / (1 << 20), 1)
        if elapsed_s
        else None,
        "delivered_MiB_per_s": round(
            iterations * nbytes * consumers / elapsed_s / (1 << 20), 1
        )
        if elapsed_s
        else None,
        "producer_rusage": rusage(),
        "consumer_rusage": [
            {"max_rss_kib": r.get("max_rss_kib"), "cpu_s": r.get("cpu_s")} for r in reports
        ],
        "zero_copy_consumers": all(r.get("zero_copy") for r in reports),
        "copies_producer_side": model["producer"],
        "copies_per_consumer": model["per_consumer"],
        "copies_total": model["producer"] + model["per_consumer"] * consumers,
        "copy_model": model["detail"],
        "path_note": PATH_NOTES[path],
        "comparable_to_other_paths": path != "tenzorbus_direct_nofill",
        "all_consumers_received_everything": all(
            r.get("samples") == iterations for r in reports
        ),
        "ring_stats": {k: v for k, v in (stats or {}).items() if k != "name"} if stats else None,
        }
        if path == "unix_socket":
            case["unix_socket_transport"] = unix_socket_transport
        validate_case(case)
        return case
    finally:
        for sock in producer_sockets:
            with contextlib.suppress(OSError):
                sock.close()
        for child in children:
            if child.poll() is None:
                child.terminate()
                with contextlib.suppress(subprocess.TimeoutExpired):
                    child.wait(timeout=5)
            if child.poll() is None:
                child.kill()
                child.wait()
        if path.startswith("tenzorbus") and ring is not None:
            with contextlib.suppress(Exception):
                ring.close(unlink=True)
            with contextlib.suppress(Exception):
                tb.unlink(ring_name)
        for endpoint in endpoints if path in ("fifo", "unix_socket") else []:
            if endpoint.startswith("socketpair:"):
                continue
            with contextlib.suppress(OSError):
                os.unlink(endpoint)
        for ready in ready_files:
            with contextlib.suppress(OSError):
                os.unlink(ready)


ALL_PATHS = [
    "tenzorbus_copy",
    "tenzorbus_direct",
    "tenzorbus_direct_nofill",
    "unix_socket",
    "fifo",
    "http_binary",
    "http_json_b64",
]

PATH_NOTES = {
    "tenzorbus_copy": (
        "publish(): the tensor already exists elsewhere, so the transport copies "
        "it once into the slot. Comparable to every other path here."
    ),
    "tenzorbus_direct": (
        "reserve()/commit() with the clock started before the payload is written, "
        "so the write is inside the measured interval. Comparable to every other "
        "path. Note that filling from an existing array is still one copy: "
        "direct-write removes the producer copy only for a producer that can "
        "GENERATE into the slot, which is what tenzorbus_direct_nofill isolates."
    ),
    "tenzorbus_direct_nofill": (
        "The transport floor for a producer that generates its tensor in place: "
        "reserve + commit + consumer acquire, with the generation excluded for the "
        "same reason media decode is excluded everywhere. NOT comparable to the "
        "other paths, and must never be quoted against them."
    ),
    "unix_socket": (
        "SOCK_STREAM over AF_UNIX, one connection per consumer. The runner uses "
        "pathname sockets when permitted and real inherited socketpairs when a "
        "seccomp policy blocks socket(AF_UNIX, ...) before pathname handling."
    ),
    "fifo": "A named pipe per consumer.",
    "http_binary": "POST with an octet-stream body to a localhost HTTP server per consumer.",
    "http_json_b64": (
        "POST of a JSON document with a base64 payload, encoded per publication "
        "inside the timed region. The anti-pattern, measured honestly: the v0.1 "
        "reference benchmark prebuilt the body, which excluded work a real sender "
        "cannot avoid."
    ),
}

# 602,112 bytes is a real TenzorPipe video tensor: float32 [3, 224, 224].
REAL_FRAME = 3 * 224 * 224 * 4
SIZES = [64 * 1024, REAL_FRAME, 4 << 20, 16 << 20]
FANOUT = [1, 2, 4, 8]


def iterations_for(nbytes):
    """Keep each case to roughly a second of traffic rather than a fixed count."""
    budget = 192 << 20
    return max(40, min(1500, budget // nbytes))


def run(args):
    paths = args.paths.split(",") if args.paths else ALL_PATHS
    unknown_paths = [path for path in paths if path not in ALL_PATHS]
    if unknown_paths:
        raise ValueError(f"unknown benchmark paths: {unknown_paths}")
    cases, port = [], 18300

    plan = []
    # Axis 1: payload size at one consumer.
    for nbytes in SIZES:
        for path in paths:
            plan.append((path, nbytes, 1))
    # Axis 2: fan-out at the real frame size.
    for consumers in FANOUT:
        if consumers == 1:
            continue
        for path in paths:
            plan.append((path, REAL_FRAME, consumers))

    with tempfile.TemporaryDirectory(prefix="tenzorbus-bm-") as workdir:
        unix_socket_transport = (
            select_unix_socket_transport(workdir) if "unix_socket" in paths else None
        )
        report = {
            "benchmark_status": "running",
            "methodology": __doc__.strip(),
            "host": {
                "platform": platform.platform(),
                "machine": platform.machine(),
                "cpus": os.cpu_count(),
                "python": platform.python_version(),
                "numpy": np.__version__,
            },
            "payload_sizes_bytes": SIZES,
            "real_frame_note": f"{REAL_FRAME} bytes is a real TenzorPipe video tensor, float32 [3,224,224]",
            "fanout": FANOUT,
            "excluded": ["media decode", "model inference"],
            "consumer_work": f"NumPy array over the payload plus a {STRIDE_SAMPLES}-sample strided sum, identical in every path",
            "path_notes": PATH_NOTES,
            "unix_socket_transport": unix_socket_transport,
            "expected_cases": len(plan),
            "cases": cases,
        }

        for path, nbytes, consumers in plan:
            iterations = iterations_for(nbytes)
            if path == "http_json_b64":
                # base64+JSON at 16 MiB is ~22 MiB of text per publication; keep the
                # case honest but bounded.
                iterations = max(10, iterations // 10)
            label = f"{path} {nbytes}B x{consumers}"
            try:
                case = run_case(
                    path,
                    nbytes,
                    consumers,
                    iterations,
                    workdir,
                    port,
                    unix_socket_transport=unix_socket_transport,
                )
                cases.append(case)
                print(
                    f"  {label:44s} median={case['latency_median_ms']:>9} ms  "
                    f"p99={case['latency_p99_ms']:>9} ms  "
                    f"{case['publications_per_s']:>9} pub/s  copies={case['copies_total']}",
                    flush=True,
                )
            except Exception as exc:
                failure = {
                    "path": path,
                    "payload_bytes": nbytes,
                    "consumers": consumers,
                    "iterations": iterations,
                    "error": f"{type(exc).__name__}: {exc}",
                }
                cases.append(failure)
                report.update(
                    {
                        "benchmark_status": "failed",
                        "completed_cases": len(cases) - 1,
                        "benchmark_errors": 1,
                    }
                )
                with open(args.output, "w") as fh:
                    json.dump(report, fh, indent=2)
                print(f"  {label:44s} FAILED: {exc}", file=sys.stderr, flush=True)
                print(f"\nwrote failed report {args.output}", file=sys.stderr)
                return 1
            port += 16

        try:
            validate_matrix(report, plan, paths, unix_socket_transport)
        except Exception as exc:
            report.update(
                {
                    "benchmark_status": "failed",
                    "completed_cases": len(cases),
                    "benchmark_errors": 1,
                    "validation_error": f"{type(exc).__name__}: {exc}",
                }
            )
            with open(args.output, "w") as fh:
                json.dump(report, fh, indent=2)
            print(f"matrix validation FAILED: {exc}", file=sys.stderr)
            return 1

        report.update(
            {
                "benchmark_status": "passed",
                "completed_cases": len(cases),
                "benchmark_errors": 0,
                "cases_per_transport": dict(Counter(case["path"] for case in cases)),
            }
        )
        with open(args.output, "w") as fh:
            json.dump(report, fh, indent=2)
        print(f"\nwrote {args.output}: {len(cases)}/{len(plan)} cases, 0 errors")
        return 0


def main(argv=None):
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("run")
    r.add_argument("--output", default="benchmark_matrix.json")
    r.add_argument("--paths", default=None, help="comma-separated subset of paths")

    c = sub.add_parser("consume")
    c.add_argument("--path", required=True, choices=ALL_PATHS)
    c.add_argument("--endpoint", default=None)
    c.add_argument("--socket-fd", type=int, default=None)
    c.add_argument("--expect", type=int, required=True)
    c.add_argument("--nbytes", type=int, required=True)
    c.add_argument("--ready-file", default=None)
    c.add_argument("--timeout", type=float, default=60.0)

    args = ap.parse_args(argv)
    return run(args) if args.cmd == "run" else consume(args)


if __name__ == "__main__":
    raise SystemExit(main())
