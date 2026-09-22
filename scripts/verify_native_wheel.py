#!/usr/bin/env python3
"""Verify an installed native production wheel with real shared-memory I/O."""

from __future__ import annotations

import argparse
import gc
import hashlib
import json
import os
import pathlib
import platform
import re
import time
import zipfile

import numpy as np
import tenzorbus_rs as tzb


EXPECTED_VERSION = "0.1.0-alpha.2"
WHEEL_RE = re.compile(
    r"^tenzorbus_py-0\.1\.0a2-cp311-abi3-manylinux_2_34_(x86_64|aarch64)\.whl$"
)


def fail(message: str) -> None:
    raise SystemExit(f"native wheel verification failed: {message}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("wheel", type=pathlib.Path)
    parser.add_argument("--expect-arch", choices=("x86_64", "aarch64"), required=True)
    parser.add_argument("--report", type=pathlib.Path, required=True)
    args = parser.parse_args()

    wheel = args.wheel.resolve()
    match = WHEEL_RE.fullmatch(wheel.name)
    if not wheel.is_file() or match is None:
        fail(f"unexpected wheel filename: {wheel.name}")
    if match.group(1) != args.expect_arch:
        fail(f"wheel is for {match.group(1)}, expected {args.expect_arch}")

    machine = platform.machine().lower()
    normalized_machine = "aarch64" if machine in {"aarch64", "arm64"} else machine
    if normalized_machine != args.expect_arch:
        fail(f"runtime machine is {machine}, expected {args.expect_arch}")
    if platform.system() != "Linux":
        fail(f"runtime system is {platform.system()}, expected Linux")
    if tzb.__version__ != EXPECTED_VERSION:
        fail(f"extension version is {tzb.__version__}, expected {EXPECTED_VERSION}")

    with zipfile.ZipFile(wheel) as archive:
        bad = archive.testzip()
        names = archive.namelist()
    if bad or not any(name.endswith(".so") and "tenzorbus_rs" in name for name in names):
        fail("wheel is corrupt or lacks the tenzorbus_rs extension")

    digest = hashlib.sha256(wheel.read_bytes()).hexdigest()
    name = f"arm64_wheel_{os.getpid()}_{time.time_ns()}"
    creator = attached = producer = consumer = None
    try:
        creator = tzb.create(name, slots=4, slot_bytes=4096)
        attached = tzb.attach(name)
        producer = creator.producer()
        consumer = attached.consumer()

        source = np.arange(24, dtype=np.float32).reshape(2, 3, 4)
        producer.publish(source)
        with consumer.next(timeout=2.0) as lease:
            view = lease.numpy()
            if view.__array_interface__["data"][0] != lease.data_address:
                fail("NumPy view does not alias the shared-memory slot")
            np.testing.assert_array_equal(view, source)
            del view

        writer = producer.reserve("int32", (16,))
        direct = writer.numpy()
        if direct.__array_interface__["data"][0] != writer.data_address:
            fail("producer NumPy view does not alias the reserved slot")
        direct[:] = np.arange(16, dtype=np.int32)
        del direct
        writer.commit()
        with consumer.next(timeout=2.0) as lease:
            view = lease.numpy()
            try:
                if view.__array_interface__["data"][0] != lease.data_address:
                    fail("consumer NumPy view does not alias the committed slot")
                np.testing.assert_array_equal(view, np.arange(16, dtype=np.int32))
            finally:
                del view

        tzb.unlink(name)
        try:
            tzb.attach(name)
        except RuntimeError:
            pass
        else:
            fail("attach succeeded after unlink")

        # POSIX unlink removes the name, not existing mappings.
        producer.publish(np.arange(8, dtype=np.int32))
        with consumer.next(timeout=2.0) as lease:
            view = lease.numpy()
            np.testing.assert_array_equal(view, np.arange(8, dtype=np.int32))
            del view
    finally:
        producer = consumer = attached = creator = None
        gc.collect()
        try:
            tzb.unlink(name)
        except RuntimeError:
            pass

    report = {
        "passed": True,
        "system": platform.system(),
        "machine": machine,
        "python": platform.python_version(),
        "extension_version": tzb.__version__,
        "protocol_version": tzb.PROTOCOL_VERSION,
        "wheel": wheel.name,
        "sha256": digest,
        "checks": [
            "native architecture",
            "wheel tag and extension payload",
            "create and attach",
            "zero-copy NumPy alias",
            "reserve and commit",
            "unlink semantics",
            "live mappings after unlink",
        ],
    }
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
