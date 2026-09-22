#!/usr/bin/env bash
# Build the complete unpublished v0.1.0-alpha.1 asset set from one clean commit.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="${DIST:-$ROOT/dist}"
VERSION="v0.1.0-alpha.1"
EPYC_DIR="$ROOT/evidence/epyc-benchmark"

cd "$ROOT"
if [ -n "$(git status --porcelain --untracked-files=all)" ]; then
  echo "release assets must be built from a clean Git tree" >&2
  exit 2
fi

python3 scripts/update_source_checksums.py --check
python3 scripts/verify_authoritative_epyc.py
cmp LICENSE rust/tenzorbus-py/LICENSE

rm -rf "$DIST"
mkdir -p "$DIST"

python3 -m build --outdir "$DIST" "$ROOT"
(
  cd "$ROOT/rust/tenzorbus-py"
  python3 -m maturin build --release --locked --out "$DIST"
)

git archive --format=zip --prefix="TenzorBus-$VERSION/" \
  --output="$DIST/TenzorBus-$VERSION-source.zip" HEAD
git archive --format=tar.gz --prefix="TenzorBus-$VERSION/" \
  --output="$DIST/TenzorBus-$VERSION-source.tar.gz" HEAD

cp "$EPYC_DIR/epyc-kvm-final-benchmark-2026-09-21-complete.json" "$DIST/"
cp "$EPYC_DIR/TenzorBus-Final-49-Case-Benchmark.csv" "$DIST/"
cp "$EPYC_DIR/TenzorBus-Final-49-Case-Benchmark.md" "$DIST/"

production_wheels=("$DIST"/tenzorbus_py-0.1.0a1-cp311-abi3-manylinux*_x86_64.whl)
if [ "${#production_wheels[@]}" -ne 1 ] || [ ! -f "${production_wheels[0]}" ]; then
  echo "expected exactly one production Linux x86-64 tenzorbus_rs wheel" >&2
  exit 2
fi

python3 -m zipfile -t "$DIST/tenzorbus-0.1.0a1-py3-none-any.whl"
python3 -m zipfile -t "${production_wheels[0]}"
python3 -m zipfile -t "$DIST/TenzorBus-$VERSION-source.zip"
tar -tzf "$DIST/tenzorbus-0.1.0a1.tar.gz" >/dev/null
tar -tzf "$DIST/TenzorBus-$VERSION-source.tar.gz" >/dev/null
zipinfo -1 "${production_wheels[0]}" > "$DIST/.production-wheel-files"
grep -q 'tenzorbus_rs.*\.so' "$DIST/.production-wheel-files"
zipinfo -1 "$DIST/TenzorBus-$VERSION-source.zip" > "$DIST/.source-zip-files"
grep -q '/SOURCE_SHA256SUMS$' "$DIST/.source-zip-files"
grep -q '/evidence/epyc-benchmark/epyc-kvm-final-benchmark-2026-09-21-complete.json$' \
  "$DIST/.source-zip-files"
rm -f "$DIST/.production-wheel-files" "$DIST/.source-zip-files"

(
  cd "$DIST"
  find . -maxdepth 1 -type f ! -name 'SHA256SUMS' -printf '%f\0' | \
    sort -z | xargs -0 sha256sum > SHA256SUMS
  sha256sum --check SHA256SUMS
)

echo "built and verified unpublished release assets in $DIST"
cat "$DIST/SHA256SUMS"
