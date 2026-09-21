#!/usr/bin/env python3
"""Fail closed unless the authoritative EPYC JSON, CSV, and report agree."""

from __future__ import annotations

import csv
import hashlib
import json
import pathlib
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
EVIDENCE = ROOT / "evidence" / "epyc-benchmark"
RAW = EVIDENCE / "epyc-kvm-final-benchmark-2026-09-21-complete.json"
CSV = EVIDENCE / "TenzorBus-Final-49-Case-Benchmark.csv"
REPORT = EVIDENCE / "TenzorBus-Final-49-Case-Benchmark.md"
EXPECTED_SHA256 = "ba86271180b80da10a1c542312b14b2a8c643713eefb728c2aa12db45771debe"
PATHS = {
    "tenzorbus_copy",
    "tenzorbus_direct",
    "tenzorbus_direct_nofill",
    "unix_socket",
    "fifo",
    "http_binary",
    "http_json_b64",
}
CSV_FIELDS = (
    "payload_bytes",
    "consumers",
    "iterations",
    "copies_total",
    "copies_producer_side",
    "copies_per_consumer",
)
FLOAT_FIELDS = (
    "latency_median_ms",
    "latency_p95_ms",
    "latency_p99_ms",
    "publications_per_s",
    "producer_MiB_per_s",
    "delivered_MiB_per_s",
)


def fail(message: str) -> None:
    raise SystemExit(f"authoritative EPYC evidence invalid: {message}")


def main() -> int:
    for path in (RAW, CSV, REPORT):
        if not path.is_file() or path.stat().st_size == 0:
            fail(f"missing or empty {path.relative_to(ROOT)}")

    actual_hash = hashlib.sha256(RAW.read_bytes()).hexdigest()
    if actual_hash != EXPECTED_SHA256:
        fail(f"raw JSON SHA-256 is {actual_hash}, expected {EXPECTED_SHA256}")

    raw = json.loads(RAW.read_text(encoding="utf-8"))
    validation = raw.get("validation", {})
    expected_validation = {
        "passed": True,
        "expected_cases": 49,
        "completed_cases": 49,
        "error_cases": 0,
        "incomplete_cases": 0,
        "halted_on_error": None,
    }
    if validation != expected_validation:
        fail(f"unexpected validation record: {validation!r}")
    if raw.get("host", {}).get("cpus") != 9:
        fail("raw evidence does not identify the 9-vCPU host")

    cases = raw.get("cases", [])
    if len(cases) != 49:
        fail(f"raw evidence has {len(cases)} cases, expected 49")
    counts = {name: 0 for name in PATHS}
    by_key: dict[tuple[str, int, int], dict] = {}
    for case in cases:
        path = case.get("path")
        if path not in PATHS:
            fail(f"unknown transport {path!r}")
        counts[path] += 1
        key = (path, int(case["payload_bytes"]), int(case["consumers"]))
        if key in by_key:
            fail(f"duplicate case {key!r}")
        by_key[key] = case
        if not case.get("all_consumers_received_everything"):
            fail(f"incomplete delivery for {key!r}")
        if path == "unix_socket" and case.get("unix_socket_transport") != (
            "AF_UNIX/SOCK_STREAM socketpair inherited across exec"
        ):
            fail(f"Unix case lacks the accepted real socketpair mechanism: {key!r}")
    if set(counts.values()) != {7}:
        fail(f"transport case counts are not seven each: {counts!r}")

    with CSV.open(newline="", encoding="utf-8-sig") as handle:
        rows = list(csv.DictReader(handle))
    if len(rows) != 49:
        fail(f"CSV has {len(rows)} rows, expected 49")
    for row in rows:
        key = (row["path"], int(row["payload_bytes"]), int(row["consumers"]))
        case = by_key.get(key)
        if case is None:
            fail(f"CSV row has no raw case: {key!r}")
        for field in CSV_FIELDS:
            if int(row[field]) != int(case[field]):
                fail(f"CSV mismatch for {key!r} field {field}")
        for field in FLOAT_FIELDS:
            if float(row[field]) != float(case[field]):
                fail(f"CSV mismatch for {key!r} field {field}")
        for field in ("zero_copy_consumers", "all_consumers_received_everything"):
            if (row[field].lower() == "true") is not bool(case[field]):
                fail(f"CSV mismatch for {key!r} field {field}")

    report = " ".join(REPORT.read_text(encoding="utf-8").lower().split())
    required_phrases = (
        "49/49",
        "zero benchmark errors",
        "amd epyc 9v74",
        "af_unix/sock_stream",
        "tenzorbus_direct_nofill",
        "not directly",
        "comparable to paths that copy an existing payload",
        "does not win every single-consumer",
        "shared-memory fan-out",
    )
    missing = [phrase for phrase in required_phrases if phrase not in report]
    if missing:
        fail(f"report is missing required disclosure(s): {missing!r}")

    print(f"verified authoritative EPYC evidence: 49/49, sha256={actual_hash}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
        fail(str(exc))
