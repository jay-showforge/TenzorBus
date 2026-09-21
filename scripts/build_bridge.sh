#!/usr/bin/env bash
# Build the TenzorPipe -> TenzorBus direct-write bridge.
#
# The bridge needs a TenzorPipe v0.3.2 checkout with the EpochSink patch applied
# (patches/tenzorpipe-v0.3.2-direct-write.patch). Point TENZORPIPE_DIR at it; the
# script links it in as integration/tenzorpipe, which is where the crate's path
# dependency looks.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TENZORPIPE_DIR="${TENZORPIPE_DIR:-}"
LINK="$ROOT/integration/tenzorpipe"

if [ -z "$TENZORPIPE_DIR" ]; then
  if [ -e "$LINK" ]; then
    echo "using existing $LINK"
  else
    echo "set TENZORPIPE_DIR to a patched TenzorPipe v0.3.2 checkout" >&2
    exit 2
  fi
else
  TENZORPIPE_DIR="$(cd "$TENZORPIPE_DIR" && pwd)"
  if [ ! -f "$TENZORPIPE_DIR/src/sink.rs" ]; then
    echo "$TENZORPIPE_DIR has no src/sink.rs: apply patches/tenzorpipe-v0.3.2-direct-write.patch first" >&2
    exit 2
  fi
  rm -rf "$LINK"
  ln -s "$TENZORPIPE_DIR" "$LINK"
  echo "linked $LINK -> $TENZORPIPE_DIR"
fi

cargo build --release --manifest-path "$ROOT/integration/bridge/Cargo.toml"
echo "built $ROOT/integration/bridge/target/release/tenzorbus-ingest"
"$ROOT/integration/bridge/target/release/tenzorbus-ingest" 2>&1 | head -1 || true
