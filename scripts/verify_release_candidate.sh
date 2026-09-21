#!/usr/bin/env bash
# Run every required release gate and fail on any failure, skip, skew, or omission.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPORT="${REPORT:-$ROOT/release_candidate_gates.json}"
REQUIRED_RUSTC="${REQUIRED_RUSTC:-1.98.1}"
TENZORPIPE_DIR="${TENZORPIPE_DIR:-}"
declare -a NAMES=() STATUS=() DETAIL=()

record() {
  NAMES+=("$1")
  STATUS+=("$2")
  DETAIL+=("$3")
  printf '%-52s %s  %s\n' "$1" "$2" "$3"
}

gate() {
  local name="$1"
  shift
  local log
  log="$(mktemp)"
  if "$@" >"$log" 2>&1; then
    record "$name" PASS "$(tail -1 "$log" | cut -c1-120)"
  else
    record "$name" FAIL "$(tail -5 "$log" | tr '\n' ' ' | cut -c1-240)"
  fi
  rm -f "$log"
}

tenzorpipe_matrix() {
  local temporary generated artifacts log
  temporary="$(mktemp -d)"
  generated="$temporary/fixtures"
  artifacts="$temporary/artifacts"
  log="$temporary/matrix.log"
  if bash "$TENZORPIPE_DIR/scripts/gen_skip_fixtures.sh" "$generated" && \
      env -C "$TENZORPIPE_DIR" python3 scripts/gen_short_tail_fixtures.py && \
      bash "$TENZORPIPE_DIR/scripts/gen_hevc_fixtures.sh" && \
      TENZOR_GEN_FIXTURES="$generated" TENZOR_OUTPUT_ROOT="$artifacts" \
      RESULT="$artifacts/skip-identity.json" \
      python3 "$TENZORPIPE_DIR/scripts/test_skip_identity.py" >"$log" 2>&1 && \
      grep -q "TOTAL 1224 FAIL 0" "$log"; then
    tail -1 "$log"
    rm -rf "$temporary"
    return 0
  fi
  tail -20 "$log" >&2 2>/dev/null || true
  rm -rf "$temporary"
  return 1
}

echo "=== toolchain ==="
ACTUAL_RUSTC="$(rustc --version 2>/dev/null | awk '{print $2}')"
if [ "$ACTUAL_RUSTC" = "$REQUIRED_RUSTC" ]; then
  record "toolchain is $REQUIRED_RUSTC" PASS "$ACTUAL_RUSTC"
else
  record "toolchain is $REQUIRED_RUSTC" FAIL "found ${ACTUAL_RUSTC:-none}; overrides cannot make a release pass"
fi

echo
echo "=== TenzorBus source and evidence ==="
gate "source checksum manifest" env -C "$ROOT" \
  python3 scripts/update_source_checksums.py --check
gate "authoritative EPYC evidence" env -C "$ROOT" \
  python3 scripts/verify_authoritative_epyc.py
if [ -n "${RELEASE_ASSETS_DIR:-}" ] && [ -d "$RELEASE_ASSETS_DIR" ]; then
  gate "release artifact set" env -C "$ROOT" \
    python3 scripts/verify_release_assets.py "$RELEASE_ASSETS_DIR"
else
  record "release artifact set" FAIL "set RELEASE_ASSETS_DIR to the exact publishable assets"
fi
gate "approved GitHub hero digest" bash -c \
  "printf '%s  %s\n' '2d7620b3baa8b3196d3419de8596916fcad46a396e1c8ac541d5a75c0f2e82a5' '$ROOT/docs/assets/tenzorbus-github-hero.png' | sha256sum --check --status"
gate "root and extension licences match" cmp \
  "$ROOT/LICENSE" "$ROOT/rust/tenzorbus-py/LICENSE"

echo
echo "=== TenzorBus build and tests ==="
gate "build workspace + extension" bash "$ROOT/scripts/build_rust.sh"
gate "rust tests" env -C "$ROOT/rust" cargo test --release --locked --workspace
gate "clippy -D warnings" env -C "$ROOT/rust" \
  cargo clippy --locked --workspace --all-targets --release -- -D warnings
gate "rustfmt" env -C "$ROOT/rust" cargo fmt --all --check
gate "dependency licence policy" env -C "$ROOT/rust" cargo deny check licenses
gate "threadsanitizer (clean + positive control)" bash "$ROOT/scripts/run_tsan.sh"

echo
echo "=== TenzorPipe ==="
if [ -n "$TENZORPIPE_DIR" ] && [ -f "$TENZORPIPE_DIR/src/sink.rs" ]; then
  gate "tenzorpipe rust tests" env -C "$TENZORPIPE_DIR" cargo test --release --locked
  gate "tenzorpipe clippy -D warnings" env -C "$TENZORPIPE_DIR" \
    cargo clippy --locked --workspace --all-targets --release -- -D warnings
  gate "tenzorpipe rustfmt" env -C "$TENZORPIPE_DIR" cargo fmt --all --check
  gate "tenzorpipe python tests (non-matrix)" env -C "$TENZORPIPE_DIR" \
    python3 -m pytest tests/ -q -m "not matrix" \
      -k "not recorded_regression_matrix_has_no_drift"
  if [ "${SKIP_TENZORPIPE_MATRIX:-0}" = "1" ]; then
    record "tenzorpipe 1,224-case regression matrix" FAIL \
      "required gate cannot be skipped with SKIP_TENZORPIPE_MATRIX"
  else
    gate "tenzorpipe 1,224-case regression matrix" tenzorpipe_matrix
  fi
  gate "tenzorpipe determinism on this arch" env -C "$TENZORPIPE_DIR" \
    python3 scripts/test_arch_identity.py
  if [ -x "${PRISTINE_TENZOR:-}" ]; then
    gate ".tenzor output byte-identical to pristine v0.3.2" \
      env -C "$TENZORPIPE_DIR" OLD="$PRISTINE_TENZOR" \
      python3 scripts/test_sink_refactor_identity.py
  else
    record ".tenzor output byte-identical to pristine v0.3.2" FAIL \
      "set PRISTINE_TENZOR to an unpatched v0.3.2 tenzor binary"
  fi
  gate "build the direct-write bridge" env TENZORPIPE_DIR="$TENZORPIPE_DIR" \
    bash "$ROOT/scripts/build_bridge.sh"
else
  record "tenzorpipe rust tests" FAIL "set TENZORPIPE_DIR to a patched v0.3.2 checkout"
  record "tenzorpipe clippy -D warnings" FAIL "TenzorPipe checkout unavailable"
  record "tenzorpipe rustfmt" FAIL "TenzorPipe checkout unavailable"
  record "tenzorpipe python tests (non-matrix)" FAIL "TenzorPipe checkout unavailable"
  record "tenzorpipe 1,224-case regression matrix" FAIL "TenzorPipe checkout unavailable"
  record "tenzorpipe determinism on this arch" FAIL "TenzorPipe checkout unavailable"
  record ".tenzor output byte-identical to pristine v0.3.2" FAIL "TenzorPipe checkout unavailable"
  record "build the direct-write bridge" FAIL "TenzorPipe checkout unavailable"
fi

echo
echo "=== integration ==="
gate "benchmark harness regressions" env -C "$ROOT" \
  python3 -m unittest tests.test_benchmark_harness -v
gate "python suite (strict; incl. live direct write)" env -C "$ROOT" \
  PYTHONPATH="$ROOT/src" TENZORBUS_REQUIRE_INTEGRATION=1 \
  TENZORPIPE_BIN="${TENZORPIPE_BIN:-}" \
  python3 -m pytest tests/ -q

if [ "${SKIP_SOAKS:-0}" = "1" ]; then
  record "ring stress soaks" FAIL "required gate cannot be skipped with SKIP_SOAKS"
  record "media integration soak" FAIL "required gate cannot be skipped with SKIP_SOAKS"
  record "live direct-write soak" FAIL "required gate cannot be skipped with SKIP_SOAKS"
  record "benchmark matrix" FAIL "required gate cannot be skipped with SKIP_SOAKS"
else
  echo
  echo "=== soaks (minutes) ==="
  gate "ring stress soaks" bash "$ROOT/scripts/run_rust_stress.sh"
  gate "media integration soak" env TENZORPIPE_BIN="${TENZORPIPE_BIN:-}" \
    bash "$ROOT/scripts/run_integration_soak.sh"
  gate "live direct-write soak" env TENZORPIPE_BIN="${TENZORPIPE_BIN:-}" \
    bash "$ROOT/scripts/run_live_soak.sh"
  gate "benchmark matrix" env PYTHONPATH="$ROOT/src" \
    python3 "$ROOT/benchmarks/benchmark_matrix.py" run --output "$ROOT/benchmark_matrix.json"
fi

python3 - "$REPORT" "$ACTUAL_RUSTC" "$REQUIRED_RUSTC" \
  "$(printf '%s\n' "${NAMES[@]}" | base64 -w0)" \
  "$(printf '%s\n' "${STATUS[@]}" | base64 -w0)" \
  "$(printf '%s\n' "${DETAIL[@]}" | base64 -w0)" <<'PY'
import base64
import collections
import json
import os
import platform
import sys

report, actual, required = sys.argv[1:4]
names, statuses, details = (
    base64.b64decode(value).decode().splitlines() for value in sys.argv[4:7]
)
gates = [
    {"gate": name, "status": status, "detail": detail}
    for name, status, detail in zip(names, statuses, details)
]
required_gates = [
    f"toolchain is {required}",
    "source checksum manifest",
    "authoritative EPYC evidence",
    "release artifact set",
    "approved GitHub hero digest",
    "root and extension licences match",
    "build workspace + extension",
    "rust tests",
    "clippy -D warnings",
    "rustfmt",
    "dependency licence policy",
    "threadsanitizer (clean + positive control)",
    "tenzorpipe rust tests",
    "tenzorpipe clippy -D warnings",
    "tenzorpipe rustfmt",
    "tenzorpipe python tests (non-matrix)",
    "tenzorpipe 1,224-case regression matrix",
    "tenzorpipe determinism on this arch",
    ".tenzor output byte-identical to pristine v0.3.2",
    "build the direct-write bridge",
    "benchmark harness regressions",
    "python suite (strict; incl. live direct write)",
    "ring stress soaks",
    "media integration soak",
    "live direct-write soak",
    "benchmark matrix",
]
counts = collections.Counter(names)
missing = [name for name in required_gates if counts[name] == 0]
duplicates = [name for name in required_gates if counts[name] > 1]
unexpected = [name for name in names if name not in required_gates]
non_pass = [gate["gate"] for gate in gates if gate["status"] != "PASS"]
passed = not (missing or duplicates or unexpected or non_pass)
out = {
    "passed": passed,
    "host": platform.platform(),
    "cpus": os.cpu_count(),
    "rustc": actual,
    "rustc_required": required,
    "required_gate_count": len(required_gates),
    "gates": gates,
    "missing_required_gates": missing,
    "duplicate_required_gates": duplicates,
    "unexpected_gates": unexpected,
    "non_pass_gates": non_pass,
}
with open(report, "w", encoding="utf-8") as handle:
    json.dump(out, handle, indent=2)
    handle.write("\n")
print(f"\nwrote {report}")
print(f"{sum(g['status'] == 'PASS' for g in gates)} passed; "
      f"{len(non_pass)} non-pass; {len(missing)} missing; "
      f"{len(duplicates)} duplicate; {len(unexpected)} unexpected")
if not passed:
    for category, values in (
        ("NON-PASS", non_pass),
        ("MISSING", missing),
        ("DUPLICATE", duplicates),
        ("UNEXPECTED", unexpected),
    ):
        for value in values:
            print(f"  {category}: {value}")
raise SystemExit(0 if passed else 1)
PY
