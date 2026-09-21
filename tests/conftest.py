"""Pytest configuration for the TenzorBus suite.

Integration tests skip when their prerequisites are absent. That is right for
day-to-day development and wrong when verifying a release candidate: the suite
reports green while the claims that matter never run.

Set TENZORBUS_REQUIRE_INTEGRATION=1 for a verification run. Two things then
change:

* a preflight check runs once, before any test, and fails with a single message
  naming every missing prerequisite -- rather than letting the absence surface
  as a dozen unrelated-looking errors further down the log;
* any skip that still occurs becomes a failure naming what was missing.
"""
from __future__ import annotations

import os
import pathlib
import shutil

import pytest

STRICT = os.environ.get("TENZORBUS_REQUIRE_INTEGRATION") == "1"
ROOT = pathlib.Path(__file__).resolve().parents[1]


def _missing_prerequisites() -> list[str]:
    """Everything the integration suite needs, with how to obtain it."""
    missing: list[str] = []

    if shutil.which("ffmpeg") is None:
        missing.append(
            "ffmpeg -- needed to build the media fixtures. Install it, or put a "
            "static build on PATH. This is the prerequisite most often lost by a "
            "script that rewrites PATH without including it."
        )

    try:
        import tenzorbus_rs  # noqa: F401
    except Exception:
        missing.append(
            "the tenzorbus_rs extension -- build and install it: "
            "maturin build --release --out dist --manifest-path rust/tenzorbus-py/Cargo.toml "
            "&& pip install dist/tenzorbus_py-*.whl"
        )

    try:
        import tenzorpipe  # noqa: F401
    except Exception:
        missing.append(
            "the tenzorpipe package -- build the patched TenzorPipe v0.3.2 and "
            "pip install its wheel"
        )

    engine = os.environ.get("TENZORPIPE_BIN")
    if not engine:
        missing.append("TENZORPIPE_BIN -- point it at a patched TenzorPipe v0.3.2 release binary")
    elif not os.access(engine, os.X_OK):
        missing.append(f"TENZORPIPE_BIN is not executable: {engine}")

    bridge = ROOT / "integration" / "bridge" / "target" / "release" / "tenzorbus-ingest"
    if not bridge.exists():
        missing.append(
            "the direct-write bridge -- TENZORPIPE_DIR=/path/to/patched/tenzorpipe "
            "bash scripts/build_bridge.sh"
        )

    return missing


def pytest_report_header(config):
    if not STRICT:
        return ("integration tests may skip; set TENZORBUS_REQUIRE_INTEGRATION=1 "
                "to forbid it")
    return "TENZORBUS_REQUIRE_INTEGRATION=1: prerequisites enforced, skips are failures"


def pytest_sessionstart(session):
    """Fail once, clearly, instead of many times, confusingly."""
    if not STRICT:
        return
    missing = _missing_prerequisites()
    if not missing:
        return
    lines = [
        "",
        "TENZORBUS_REQUIRE_INTEGRATION=1 is set, but the integration suite cannot run.",
        "",
        "Missing prerequisites:",
    ]
    lines += [f"  - {m}" for m in missing]
    lines += [
        "",
        "Without these the integration tests would skip, and a release verification",
        "that skips its own integration tests proves nothing. Install what is listed",
        "above and re-run, or clear TENZORBUS_REQUIRE_INTEGRATION for a development run.",
        "",
    ]
    pytest.exit("\n".join(lines), returncode=2)


@pytest.hookimpl(hookwrapper=True, trylast=True)
def pytest_runtest_makereport(item, call):
    outcome = yield
    report = outcome.get_result()
    if not (STRICT and report.skipped):
        return
    reason = report.longrepr
    if isinstance(reason, tuple) and len(reason) == 3:
        reason = reason[2]
    report.outcome = "failed"
    report.longrepr = (
        f"{item.nodeid} was skipped while TENZORBUS_REQUIRE_INTEGRATION=1.\n"
        f"reason: {reason}\n"
        "A release-candidate verification must not skip integration tests. "
        "Install the prerequisite and re-run, or clear the variable for a "
        "development run."
    )
