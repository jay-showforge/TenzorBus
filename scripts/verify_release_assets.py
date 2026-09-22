#!/usr/bin/env python3
"""Verify the complete unpublished release asset directory fail-closed."""

from __future__ import annotations

import argparse
import hashlib
import pathlib
import tarfile
import zipfile


VERSION = "v0.1.0-alpha.2"
RAW_NAME = "epyc-kvm-final-benchmark-2026-09-21-complete.json"
RAW_SHA256 = "ba86271180b80da10a1c542312b14b2a8c643713eefb728c2aa12db45771debe"
FIXED_REQUIRED = {
    "tenzorbus-0.1.0a2-py3-none-any.whl",
    "tenzorbus-0.1.0a2.tar.gz",
    f"TenzorBus-{VERSION}-source.zip",
    f"TenzorBus-{VERSION}-source.tar.gz",
    RAW_NAME,
    "TenzorBus-Final-49-Case-Benchmark.csv",
    "TenzorBus-Final-49-Case-Benchmark.md",
}


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def fail(message: str) -> None:
    raise SystemExit(f"release assets invalid: {message}")


def verify_source_zip(path: pathlib.Path) -> None:
    prefix = f"TenzorBus-{VERSION}/"
    with zipfile.ZipFile(path) as archive:
        bad = archive.testzip()
        if bad:
            fail(f"source ZIP CRC failure: {bad}")
        files = {name for name in archive.namelist() if not name.endswith("/")}
        manifest_name = prefix + "SOURCE_SHA256SUMS"
        raw_name = prefix + "evidence/epyc-benchmark/" + RAW_NAME
        for required in (manifest_name, raw_name, prefix + "docs/assets/tenzorbus-github-hero.png"):
            if required not in files:
                fail(f"source ZIP missing {required}")
        manifest = archive.read(manifest_name).decode("utf-8").splitlines()
        covered: set[str] = set()
        for line in manifest:
            digest, rel = line.split("  ./", 1)
            member = prefix + rel
            if member not in files:
                fail(f"SOURCE_SHA256SUMS names absent source member {rel}")
            if sha256_bytes(archive.read(member)) != digest:
                fail(f"SOURCE_SHA256SUMS mismatch for {rel}")
            covered.add(member)
        expected = files - {manifest_name}
        if covered != expected:
            missing = sorted(expected - covered)
            extra = sorted(covered - expected)
            fail(f"source manifest coverage differs; missing={missing}, extra={extra}")
        if sha256_bytes(archive.read(raw_name)) != RAW_SHA256:
            fail("authoritative raw EPYC JSON changed inside source ZIP")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=pathlib.Path)
    parser.add_argument(
        "--required-native-arches",
        default="x86_64,aarch64",
        help="comma-separated native wheel architectures required in this set",
    )
    args = parser.parse_args()
    directory = args.directory.resolve()
    if not directory.is_dir():
        fail(f"not a directory: {directory}")

    checksum_path = directory / "SHA256SUMS"
    if not checksum_path.is_file():
        fail("missing SHA256SUMS")
    listed: dict[str, str] = {}
    for line in checksum_path.read_text(encoding="utf-8").splitlines():
        digest, name = line.split(maxsplit=1)
        name = name.lstrip(" *")
        if name in listed:
            fail(f"duplicate checksum entry {name}")
        listed[name] = digest

    actual_files = {
        path.name for path in directory.iterdir()
        if path.is_file() and path.name != "SHA256SUMS"
    }
    if set(listed) != actual_files:
        fail(
            "checksum coverage differs from asset files: "
            f"unlisted={sorted(actual_files - set(listed))}, "
            f"missing={sorted(set(listed) - actual_files)}"
        )
    for name, expected in listed.items():
        actual = hashlib.sha256((directory / name).read_bytes()).hexdigest()
        if actual != expected:
            fail(f"checksum mismatch for {name}")

    if not FIXED_REQUIRED <= actual_files:
        fail(f"missing required assets: {sorted(FIXED_REQUIRED - actual_files)}")
    allowed_arches = {"x86_64", "aarch64"}
    required_arches = {
        value.strip()
        for value in args.required_native_arches.split(",")
        if value.strip()
    }
    if not required_arches or not required_arches <= allowed_arches:
        fail(f"invalid required native architectures: {sorted(required_arches)}")
    production: list[pathlib.Path] = []
    for arch in sorted(required_arches):
        matches = sorted(
            directory.glob(
                f"tenzorbus_py-0.1.0a2-cp311-abi3-manylinux_2_34_{arch}.whl"
            )
        )
        if len(matches) != 1:
            fail(f"expected one production Linux {arch} wheel, found {len(matches)}")
        production.extend(matches)

    with zipfile.ZipFile(directory / "tenzorbus-0.1.0a2-py3-none-any.whl") as archive:
        if archive.testzip() or not any(name.endswith("tenzorbus/ring.py") for name in archive.namelist()):
            fail("portable Python wheel is corrupt or incomplete")
    for wheel in production:
        with zipfile.ZipFile(wheel) as archive:
            names = archive.namelist()
            if archive.testzip() or not any(
                "tenzorbus_rs" in name and name.endswith(".so") for name in names
            ):
                fail(f"production wheel lacks the tenzorbus_rs shared library: {wheel.name}")

    verify_source_zip(directory / f"TenzorBus-{VERSION}-source.zip")
    for name in ("tenzorbus-0.1.0a2.tar.gz", f"TenzorBus-{VERSION}-source.tar.gz"):
        with tarfile.open(directory / name, "r:gz") as archive:
            archive.getmembers()
    if hashlib.sha256((directory / RAW_NAME).read_bytes()).hexdigest() != RAW_SHA256:
        fail("standalone authoritative raw EPYC JSON changed")

    print(f"verified {len(actual_files)} release assets and complete checksum coverage")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
