#!/usr/bin/env bash
# Phase 2 stress soaks for the Rust production ring.
#
# These are the long-running scenarios from HANDOFF_TO_WORK.md that are too
# slow for `cargo test`. The deterministic failure modes (kill -9 a consumer
# holding a lease, kill a producer mid-write, sequence wraparound, random
# shapes/dtypes) live in `rust/tenzorbus/tests/production_ring.rs` and run on
# every `cargo test`.
#
# Usage: scripts/run_rust_stress.sh [output.json]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/stress_rust_results.json}"
LAB="$ROOT/rust/target/release/tenzorbus-lab"

cargo build --release --manifest-path "$ROOT/rust/Cargo.toml" >/dev/null
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

run_case() {
  local name="$1" ring="$2" slots="$3" capacity="$4" consumers="$5"
  shift 5
  local produce_args=("$@")

  "$LAB" unlink --ring "$ring" >/dev/null 2>&1 || true
  "$LAB" create --ring "$ring" --slots "$slots" --capacity "$capacity" >/dev/null

  local pids=()
  for i in $(seq 1 "$consumers"); do
    "$LAB" consume --ring "$ring" --expect 100000000 --until-idle-ms 3000 \
      --timeout-ms 60000 ${CONSUMER_EXTRA:-} > "$TMP/$name.$i.json" 2>&1 &
    pids+=("$!")
  done
  # Wait for every consumer to appear in the registry before publishing.
  local waited=0
  while [ "$("$LAB" stats --ring "$ring" | sed 's/.*"consumers":\([0-9]*\).*/\1/')" -lt "$consumers" ]; do
    sleep 0.05
    waited=$((waited + 1))
    if [ "$waited" -gt 200 ]; then echo "consumers never registered for $name" >&2; exit 1; fi
  done

  "$LAB" produce --ring "$ring" "${produce_args[@]}" > "$TMP/$name.producer.json"
  for pid in "${pids[@]}"; do wait "$pid" || true; done
  "$LAB" stats --ring "$ring" > "$TMP/$name.stats.json"
  "$LAB" unlink --ring "$ring" >/dev/null

  {
    echo "  \"$name\": {"
    echo "    \"producer\": $(cat "$TMP/$name.producer.json"),"
    echo -n "    \"consumers\": ["
    local sep=""
    for i in $(seq 1 "$consumers"); do
      echo -n "$sep$(cat "$TMP/$name.$i.json")"
      sep=", "
    done
    echo "],"
    echo "    \"ring\": $(cat "$TMP/$name.stats.json")"
    echo -n "  }"
  } >> "$TMP/report.parts"
  echo "," >> "$TMP/report.parts"
  echo "[$name] done"
}

: > "$TMP/report.parts"

echo "== 1 producer / 1 consumer, 10M small publications =="
run_case one_to_one tzstress_1c 8 4096 1 \
  --count 10000000 --elements 64 --timeout-ms 60000

echo "== 1 producer / 8 consumers, 250k publications, random consumer sleeps =="
CONSUMER_EXTRA="--sleep-max-us 50" \
run_case one_to_eight tzstress_8c 16 65536 8 \
  --count 250000 --elements 1024 --timeout-ms 60000
unset CONSUMER_EXTRA

echo "== full ring under block policy, 2 slots, slow consumer, 180s =="
CONSUMER_EXTRA="--sleep-max-us 300" \
run_case saturated_block tzstress_block 2 16384 1 \
  --count 100000000 --elements 256 --duration-s 180 --timeout-ms 60000
unset CONSUMER_EXTRA

echo "== full ring under drop-newest policy, 2 slots, slow consumer, 60s =="
CONSUMER_EXTRA="--sleep-max-us 300" \
run_case saturated_drop tzstress_drop 2 16384 1 \
  --count 100000000 --elements 256 --duration-s 60 --policy drop_newest
unset CONSUMER_EXTRA

echo "== randomised shapes / ranks / sizes, 200k publications, 4 consumers =="
run_case varied_shapes tzstress_vary 16 65536 4 \
  --count 200000 --elements 4096 --vary --timeout-ms 60000

{
  echo "{"
  echo "  \"host\": \"$(uname -srm)\","
  echo "  \"rustc\": \"$(rustc --version)\","
  echo "  \"cpus\": $(nproc),"
  echo "  \"generated\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\","
  sed '$ s/,$//' "$TMP/report.parts"
  echo "}"
} > "$OUT"

echo
echo "wrote $OUT"
python3 - "$OUT" <<'PY'
import json, sys
report = json.load(open(sys.argv[1]))
bad = 0
for name, case in report.items():
    if not isinstance(case, dict) or "consumers" not in case:
        continue
    for c in case["consumers"]:
        bad += c.get("bad", 0) + c.get("lease_violations", 0)
        if name != "saturated_drop":
            bad += c.get("gaps", 0)
print("corruption/violation/gap total across all soaks:", bad)
sys.exit(1 if bad else 0)
PY
