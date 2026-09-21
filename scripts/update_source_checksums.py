#!/usr/bin/env python3
"""Write or verify SOURCE_SHA256SUMS for every publishable source file."""

from __future__ import annotations

import argparse
import hashlib
import os
import pathlib
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "SOURCE_SHA256SUMS"
IGNORED_DIRS = {
    ".git",
    ".pytest_cache",
    ".venv",
    "__pycache__",
    "build",
    "dist",
    "target",
}
IGNORED_NAMES = {
    "SOURCE_SHA256SUMS",
    "benchmark_matrix.json",
    "integration_soak_results.json",
    "live_soak_results.json",
    "release_candidate_gates.json",
    "stress_rust_results.json",
    "tsan_results.json",
}


def tracked_files() -> list[pathlib.Path] | None:
    if not (ROOT / ".git").exists():
        return None
    result = subprocess.run(
        ["git", "-C", str(ROOT), "ls-files", "--cached", "-z"],
        capture_output=True,
        check=True,
    )
    return [ROOT / os.fsdecode(item) for item in result.stdout.split(b"\0") if item]


def walk_files() -> list[pathlib.Path]:
    return [path for path in ROOT.rglob("*") if path.is_file()]


def include(path: pathlib.Path) -> bool:
    rel = path.relative_to(ROOT)
    posix = rel.as_posix()
    if path.name in IGNORED_NAMES:
        return False
    if posix == "integration/tenzorpipe" or posix.startswith("integration/tenzorpipe/"):
        return False
    if any(part in IGNORED_DIRS or part.endswith(".egg-info") for part in rel.parts):
        return False
    if path.suffix in {".pyc", ".so", ".whl"}:
        return False
    return True


def digest(path: pathlib.Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def expected_content() -> tuple[str, int]:
    files = tracked_files()
    if files is None:
        files = walk_files()
    included = sorted(
        (path for path in files if include(path)),
        key=lambda path: path.relative_to(ROOT).as_posix(),
    )
    content = "".join(
        f"{digest(path)}  ./{path.relative_to(ROOT).as_posix()}\n"
        for path in included
    )
    return content, len(included)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--write", action="store_true", help="rewrite the manifest")
    parser.add_argument("--check", action="store_true", help="verify hashes and file coverage")
    args = parser.parse_args()
    if args.write == args.check:
        parser.error("choose exactly one of --write or --check")

    expected, count = expected_content()
    if args.write:
        temporary = MANIFEST.with_suffix(".tmp")
        temporary.write_text(expected, encoding="utf-8", newline="\n")
        temporary.replace(MANIFEST)
        print(f"wrote {MANIFEST} ({count} entries)")
        return 0

    if not MANIFEST.is_file():
        print(f"missing {MANIFEST}", file=sys.stderr)
        return 1
    actual = MANIFEST.read_text(encoding="utf-8")
    if actual != expected:
        print("SOURCE_SHA256SUMS is stale or has incomplete file coverage", file=sys.stderr)
        return 1
    print(f"verified SOURCE_SHA256SUMS ({count} entries)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
