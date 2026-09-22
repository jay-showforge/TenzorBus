#!/usr/bin/env bash
# Build and test the production wheel on a native Linux ARM64 host.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/dist-arm64}"
PYTHON="${PYTHON:-python3}"
cd "$ROOT"

if [ "$(uname -s)" != "Linux" ] || [ "$(uname -m)" != "aarch64" ]; then
  echo "native ARM64 validation requires Linux aarch64; found $(uname -s) $(uname -m)" >&2
  exit 2
fi

rm -rf "$OUT"
mkdir -p "$OUT"

cargo build --release --locked --workspace --manifest-path "$ROOT/rust/Cargo.toml"
cargo test --release --locked --workspace --manifest-path "$ROOT/rust/Cargo.toml"

"$PYTHON" -m build --wheel --outdir "$OUT" "$ROOT"
(
  cd "$ROOT/rust/tenzorbus-py"
  "$PYTHON" -m maturin build --release --locked \
    --compatibility manylinux_2_34 --interpreter "$PYTHON" --out "$OUT"
)

native_wheels=("$OUT"/tenzorbus_py-0.1.0a2-cp311-abi3-manylinux_2_34_aarch64.whl)
if [ "${#native_wheels[@]}" -ne 1 ] || [ ! -f "${native_wheels[0]}" ]; then
  echo "expected exactly one alpha.2 cp311-abi3 manylinux_2_34 aarch64 wheel" >&2
  find "$OUT" -maxdepth 1 -type f -printf '%f\n' >&2
  exit 2
fi
native_wheel="${native_wheels[0]}"
portable_wheel="$OUT/tenzorbus-0.1.0a2-py3-none-any.whl"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
"$PYTHON" -m venv "$work/venv"
venv_python="$work/venv/bin/python"
"$venv_python" -m pip install --upgrade pip
"$venv_python" -m pip install numpy "$portable_wheel" "$native_wheel"

env -u PYTHONPATH "$venv_python" -m unittest -v \
  tests.test_ring tests.test_rust_bindings tests.test_lease_lifetime \
  2>&1 | tee "$OUT/arm64-unittest.log"

env -u PYTHONPATH "$venv_python" "$ROOT/scripts/verify_native_wheel.py" \
  --expect-arch aarch64 --report "$OUT/arm64-validation.json" "$native_wheel"

(
  cd "$OUT"
  sha256sum "$(basename "$native_wheel")" > ARM64_SHA256SUMS
  sha256sum --check ARM64_SHA256SUMS
)

echo "native Linux ARM64 validation passed"
cat "$OUT/ARM64_SHA256SUMS"
