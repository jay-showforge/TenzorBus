#!/usr/bin/env bash
# ThreadSanitizer gate for the TenzorBus ring.
#
# What this can and cannot see
# ----------------------------
# TSan is a single-process tool. It cannot observe another process touching the
# same MAP_SHARED pages, so it never covers TenzorBus's cross-process traffic
# directly. What it does cover is the synchronisation itself: the atomic slot
# state, the reader bitmask and the non-atomic payload memcpy they order. That is
# the same code a separate process runs, and it is where both concurrency bugs
# found in Phase 2 lived. Cross-process behaviour stays covered by the real
# multi-process tests in rust/tenzorbus/tests/production_ring.rs.
#
# The gate is self-verifying: it runs the suite a second time with the
# `tsan-positive-control` feature, which downgrades the commit release/acquire
# pair to Relaxed and enables a test-only explicit data race, and FAILS if TSan
# does not then report a race in TenzorBus code.
# That is what makes "0 warnings naming our code" mean something, rather than
# proving the suppressions swallowed everything.
#
# Usage: scripts/run_tsan.sh [output.json]
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/tsan_results.json}"
TARGET=x86_64-unknown-linux-gnu
SUPP="$ROOT/scripts/tsan-suppressions.txt"
LOGDIR="$(mktemp -d)"
trap 'rm -rf "$LOGDIR"' EXIT

# The precompiled standard library in the stable toolchain is not instrumented,
# so mixing it with instrumented crate code needs this opt-in. The consequence is
# recorded in the report: races *inside* std cannot be seen, and std's own
# lock-free channels produce false positives, which the suppression file covers.
export RUSTC_BOOTSTRAP=1
export RUSTFLAGS="-Zsanitizer=thread -Cunsafe-allow-abi-mismatch=sanitizer"
export TSAN_OPTIONS="suppressions=$SUPP halt_on_error=0"

OURS_RE='tenzorbus/src/|tenzor-core/src/|tenzorbus/tests/tsan_threads.rs|tenzorbus::ring|tenzorbus::futex|tenzorbus::shm|tenzor_core::'

echo "== TSan: clean build =="
( cd "$ROOT/rust" && cargo test --target "$TARGET" --test tsan_threads ) \
  > "$LOGDIR/clean.log" 2>&1
CLEAN_RC=$?
CLEAN_WARN=$(grep -c "WARNING: ThreadSanitizer" "$LOGDIR/clean.log" || true)
CLEAN_OURS=$(grep -Ec "$OURS_RE" "$LOGDIR/clean.log" || true)
CLEAN_PASSED=$(grep -c "test result: ok" "$LOGDIR/clean.log" || true)
echo "   tests_ok_lines=$CLEAN_PASSED  tsan_warnings=$CLEAN_WARN  frames_in_tenzorbus=$CLEAN_OURS"

# The test-only explicit race makes detection reliable, while the existing
# relaxed-ordering control still exercises the ring. Keep bounded retries for
# runner scheduling variance and require at least one detection; the gate still
# fails closed when the injected race is never observed.
CONTROL_ATTEMPTS="${TSAN_CONTROL_ATTEMPTS:-6}"
echo "== TSan: positive control (test-only race + relaxed commit ordering) =="
CTRL_RC=0; CTRL_WARN=0; CTRL_OURS=0; CTRL_TRIES=0
for attempt in $(seq 1 "$CONTROL_ATTEMPTS"); do
  CTRL_TRIES=$attempt
  ( cd "$ROOT/rust" && cargo test --target "$TARGET" --test tsan_threads \
      --features tsan-positive-control -- --test-threads=1 ) \
    > "$LOGDIR/control.log" 2>&1
  CTRL_RC=$?
  CTRL_WARN=$(grep -c "WARNING: ThreadSanitizer" "$LOGDIR/control.log" || true)
  CTRL_OURS=$(grep -Ec "$OURS_RE" "$LOGDIR/control.log" || true)
  echo "   attempt $attempt: tsan_warnings=$CTRL_WARN  frames_in_tenzorbus=$CTRL_OURS"
  if [ "$CTRL_WARN" -gt 0 ] && [ "$CTRL_OURS" -gt 0 ]; then
    break
  fi
done

CLEAN_OK=0
[ "$CLEAN_OURS" -eq 0 ] && [ "$CLEAN_PASSED" -ge 1 ] && CLEAN_OK=1
CONTROL_OK=0
[ "$CTRL_WARN" -gt 0 ] && [ "$CTRL_OURS" -gt 0 ] && CONTROL_OK=1

python3 - "$OUT" "$LOGDIR/clean.log" "$LOGDIR/control.log" \
  "$CLEAN_RC" "$CLEAN_WARN" "$CLEAN_OURS" "$CTRL_RC" "$CTRL_WARN" "$CTRL_OURS" "$CTRL_TRIES" <<'PY'
import json, platform, re, subprocess, sys
out, clean_log, control_log = sys.argv[1:4]
clean_rc, clean_warn, clean_ours, ctrl_rc, ctrl_warn, ctrl_ours = map(int, sys.argv[4:10])

def top_frames(path, limit=6):
    text = open(path, errors="replace").read()
    frames = []
    for block in text.split("WARNING: ThreadSanitizer")[1:]:
        for line in block.splitlines():
            m = re.match(r"#\d+ (.+?) [/<]", line.strip())
            if m:
                frames.append(m.group(1))
                break
    return frames[:limit]

try:
    rustc = subprocess.run(["rustc", "--version"], capture_output=True, text=True).stdout.strip()
except Exception:
    rustc = None

report = {
    "host": f"{platform.system()} {platform.release()} {platform.machine()}",
    "rustc": rustc,
    "instrumented_std": False,
    "scope": (
        "TSan is single-process and cannot observe cross-process access to the "
        "shared mapping; it covers the atomics and the payload memcpy they order. "
        "Cross-process behaviour is covered by tests/production_ring.rs instead."
    ),
    "caveat": (
        "The stable toolchain ships a precompiled, uninstrumented std "
        "(-Zbuild-std needs nightly + rust-src). Races inside std are therefore "
        "invisible, and std's own lock-free channels raise false positives; "
        "scripts/tsan-suppressions.txt covers exactly those."
    ),
    "clean_run": {
        "exit_code": clean_rc,
        "tests_passed": "test result: ok" in open(clean_log, errors="replace").read(),
        "tsan_warnings": clean_warn,
        "warnings_naming_tenzorbus": clean_ours,
        "warning_top_frames": top_frames(clean_log),
    },
    "positive_control": {
        "feature": "tsan-positive-control",
        "exit_code": ctrl_rc,
        "tsan_warnings": ctrl_warn,
        "warnings_naming_tenzorbus": ctrl_ours,
        "detected_the_injected_race": ctrl_warn > 0 and ctrl_ours > 0,
        "attempts_needed": int(sys.argv[10]),
        "note": (
            "The feature enables a test-only explicit data race and relaxes the "
            "ring commit ordering. The control is retried up to "
            "TSAN_CONTROL_ATTEMPTS times for runner scheduling variance and "
            "passes only after TSan reports a warning in TenzorBus code."
        ),
        "warning_top_frames": top_frames(control_log),
    },
}
json.dump(report, open(out, "w"), indent=2)
print(f"wrote {out}")
PY

echo
if [ "$CLEAN_OK" -eq 1 ]; then
  echo "clean run:        PASS  ($CLEAN_WARN TSan warning(s), 0 naming tenzorbus)"
else
  echo "clean run:        FAIL  ($CLEAN_WARN warning(s), $CLEAN_OURS naming tenzorbus)"
  grep -B2 -A8 "WARNING: ThreadSanitizer" "$LOGDIR/clean.log" | head -40
fi
if [ "$CONTROL_OK" -eq 1 ]; then
  echo "positive control: PASS  (injected race detected in tenzorbus after $CTRL_TRIES attempt(s))"
else
  echo "positive control: FAIL  (the gate could not detect a deliberately broken ordering)"
fi

[ "$CLEAN_OK" -eq 1 ] && [ "$CONTROL_OK" -eq 1 ] && exit 0
exit 1
