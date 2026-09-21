#!/usr/bin/env bash
# Reproduce the publishable TenzorBus benchmark matrix on the EPYC host.
#
# The WSL2 run that accompanied verification was a methodology check only: those
# numbers are development measurements and must not be published. This script
# re-runs the same harness, unchanged, so the only difference between the two
# result files is the machine underneath.
#
#   TENZORPIPE_BIN=/path/to/patched/tenzor bash scripts/rerun_benchmarks_epyc.sh [out.json]
#
# It configures nothing interactively and alters no methodology: same harness,
# same payload shape, same consumer counts, same statistics.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/epyc-benchmark_matrix.json}"

need() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1" >&2; exit 2; }; }
need cargo
need python3

# The release toolchain is pinned. Refuse to produce publishable numbers from
# anything else: codegen differences would silently change the result.
PINNED=1.98.1
HAVE="$(rustc --version | awk '{print $2}')"
if [ "$HAVE" != "$PINNED" ]; then
  echo "rustc is $HAVE but the release toolchain is $PINNED." >&2
  echo "  rustup toolchain install $PINNED && rustup override set $PINNED" >&2
  exit 2
fi

: "${TENZORPIPE_BIN:?set TENZORPIPE_BIN to a patched TenzorPipe v0.3.2 release binary}"
[ -x "$TENZORPIPE_BIN" ] || { echo "TENZORPIPE_BIN is not executable: $TENZORPIPE_BIN" >&2; exit 2; }
# Executable is not enough: confirm it really is TenzorPipe before producing numbers
# that will be published under its name.
if ! "$TENZORPIPE_BIN" --version 2>/dev/null | grep -qi "^tenzor"; then
  echo "TENZORPIPE_BIN does not identify as TenzorPipe: $TENZORPIPE_BIN" >&2
  echo "  expected '--version' to report a tenzor build" >&2
  exit 2
fi

# Never overwrite a previous publishable result by accident.
if [ -e "$OUT" ]; then
  echo "refusing to overwrite an existing result file: $OUT" >&2
  echo "  move it aside or pass a different output path" >&2
  exit 2
fi

export PYTHONPATH="${PYTHONPATH:-}:$ROOT/src"
python3 - <<'PY' || { echo "python needs numpy and the tenzorbus_rs extension on this interpreter" >&2; exit 2; }
import numpy, tenzorbus_rs  # noqa: F401
PY

echo "== building the bus at the pinned toolchain"
cargo build --release --locked --manifest-path "$ROOT/rust/Cargo.toml" >/dev/null

echo "== host"
{
  uname -sr
  grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs
  echo "cores: $(nproc)"
  free -g | awk '/^Mem:/{print "mem: " $2 " GiB"}'
  df -h /dev/shm | awk 'NR==2{print "/dev/shm: " $2}'
  rustc --version
  python3 -V
} | sed 's/^/   /'

echo "== benchmark matrix (publishable run)"
python3 "$ROOT/benchmarks/benchmark_matrix.py" run --output "$OUT"

echo
echo "wrote $OUT"
echo "Compare against the WSL2 development run for methodology only, never for absolute numbers."
