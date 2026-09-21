#!/usr/bin/env bash
# Build the Rust workspace and install the Python extension next to the
# reference package, so `PYTHONPATH=src` gives you both:
#
#   import tenzorbus          # executed v0.1 Python reference
#   import tenzorbus_rs       # Rust production ring
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PYTHON="${PYTHON:-python3}"
export PYO3_PYTHON="${PYO3_PYTHON:-$(command -v "$PYTHON")}"

cargo build --release --locked --workspace --manifest-path "$ROOT/rust/Cargo.toml"

SO="$ROOT/rust/target/release/libtenzorbus_rs.so"
if [ ! -f "$SO" ]; then
  echo "extension module not found at $SO" >&2
  exit 1
fi
cp "$SO" "$ROOT/src/tenzorbus_rs.abi3.so"
echo "installed $ROOT/src/tenzorbus_rs.abi3.so"

PYTHONPATH="$ROOT/src" "$PYTHON" - <<'PY'
import tenzorbus_rs as tb
print(f"tenzorbus_rs {tb.__version__}  protocol v{tb.PROTOCOL_VERSION}  max_consumers={tb.MAX_CONSUMERS}")
PY
