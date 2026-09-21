#!/usr/bin/env bash
# Soak the live direct-write path: TenzorPipe's decoder writing straight into
# TenzorBus slots, harder and longer than the unittest gate.
#
# Needs the bridge (scripts/build_bridge.sh), the tenzorpipe package, the built
# extension and ffmpeg.
#
# Usage: scripts/run_live_soak.sh [output.json]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/live_soak_results.json}"
export PYTHONPATH="$ROOT/src${PYTHONPATH:+:$PYTHONPATH}"
DEMO="$ROOT/demos/tenzorpipe_to_bus.py"
INGEST="$ROOT/integration/bridge/target/release/tenzorbus-ingest"
FIXTURES="$ROOT/fixtures"
mkdir -p "$FIXTURES"

if [ ! -x "$INGEST" ]; then
  echo "bridge not built: run scripts/build_bridge.sh" >&2
  exit 2
fi

CLIP="$FIXTURES/soak_720p_60s.mp4"
if [ ! -f "$CLIP" ]; then
  echo "generating a 60 s 720p H.264/AAC fixture"
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=1280x720:rate=30:duration=60" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=60" \
    -c:v libx264 -pix_fmt yuv420p -g 60 -bf 2 -c:a aac -shortest "$CLIP"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
REF="$TMP/reference.tenzor"

# The reference every consumer verifies against is the ORDINARY .tenzor path.
echo "building the .tenzor reference with the unmodified path"
if [ -n "${TENZORPIPE_BIN:-}" ]; then
  "$TENZORPIPE_BIN" -i "$CLIP" -o "$REF" --quiet
else
  python3 -c "import sys,tenzorpipe as tp; tp.ingest(sys.argv[1], sys.argv[2])" "$CLIP" "$REF"
fi
EPOCHS="$(python3 -c "import sys,tenzorpipe as tp;d=tp.load(sys.argv[1]);print(len(d));d.close()" "$REF")"
echo "reference has $EPOCHS epochs"

: > "$TMP/parts"

soak_case() {
  local name="$1" consumers="$2" slots="$3"; shift 3
  local extra=("$@")
  echo "== $name (consumers=$consumers slots=$slots ${extra[*]:-}) =="
  local ring="lsoak_$$_${name}"
  local pids=() roles=(detector embedder preview)
  for i in $(seq 0 $((consumers - 1))); do
    local role="${roles[$((i % 3))]}"
    local args=(consume --backend rust --ring "$ring" --tenzor "$REF"
                --expect "$EPOCHS" --role "$role" --timeout 180)
    if [ -n "${CONSUMER_SLEEP:-}" ]; then args+=(--sleep-max-ms "$CONSUMER_SLEEP"); fi
    if [ -n "${DIE_AFTER:-}" ] && [ "$i" -eq 0 ]; then args+=(--die-after "$DIE_AFTER"); fi
    python3 "$DEMO" "${args[@]}" > "$TMP/$name.$i.json" 2>"$TMP/$name.$i.err" &
    pids+=("$!")
  done

  set +e
  "$INGEST" stream --media "$CLIP" --ring "$ring" --slots "$slots" \
    --await-consumers "$consumers" --timeout-s 180 "${extra[@]}" \
    > "$TMP/$name.bridge.json" 2>"$TMP/$name.bridge.err"
  local rc=$?
  set -e
  for pid in "${pids[@]}"; do wait "$pid" || true; done
  python3 -c "import sys,tenzorbus_rs as tb; tb.unlink(sys.argv[1])" "$ring" 2>/dev/null || true

  python3 - "$TMP" "$name" "$consumers" "$rc" >> "$TMP/parts" <<'PY'
import json, pathlib, sys
tmp, name, consumers, rc = pathlib.Path(sys.argv[1]), sys.argv[2], int(sys.argv[3]), int(sys.argv[4])

def last(path):
    text = pathlib.Path(path).read_text() if pathlib.Path(path).exists() else ""
    dec, i, found = json.JSONDecoder(), 0, None
    while i < len(text):
        while i < len(text) and text[i] != "{":
            i += 1
        if i >= len(text):
            break
        try:
            obj, end = dec.raw_decode(text, i)
        except json.JSONDecodeError:
            i += 1
            continue
        found = obj
        i = end
    return found

bridge = last(tmp / f"{name}.bridge.json") or {"error": "no bridge report"}
bridge["exit_code"] = rc
consumer_reports = []
for i in range(consumers):
    report = last(tmp / f"{name}.{i}.json") or {"error": "no consumer report"}
    consumer_reports.append(report)
print(json.dumps({name: {"bridge": bridge, "consumers": consumer_reports}}) + ",")
PY
  echo "[$name] bridge exit=$rc"
}

soak_case direct_fanout8      8 16
CONSUMER_SLEEP=3 soak_case direct_saturated_2slot 6 2
unset CONSUMER_SLEEP
DIE_AFTER=41 soak_case direct_crash_midstream    6 3
unset DIE_AFTER
soak_case direct_single        1 4
soak_case copy_path_fanout4    4 8 --copy-path
soak_case direct_tight_fit     4 2

python3 - "$TMP/parts" "$OUT" "$EPOCHS" <<'PY'
import json, platform, subprocess, sys
parts = open(sys.argv[1]).read().strip().rstrip(",")
epochs = int(sys.argv[3])
merged = {}
for chunk in json.loads("[" + parts + "]"):
    merged.update(chunk)
try:
    cpus = int(subprocess.run(["nproc"], capture_output=True, text=True).stdout.strip())
except Exception:
    cpus = None
report = {
    "host": f"{platform.system()} {platform.release()} {platform.machine()}",
    "cpus": cpus,
    "epochs_per_case": epochs,
    "reference": "the ordinary .tenzor path, read independently by every consumer",
    "cases": merged,
}
json.dump(report, open(sys.argv[2], "w"), indent=2)
print(f"wrote {sys.argv[2]}")

problems = 0
for name, case in merged.items():
    bridge = case["bridge"]
    if bridge.get("published") != epochs:
        print(f"  {name}: bridge published {bridge.get('published')} of {epochs}", file=sys.stderr)
        problems += 1
    if name.startswith("direct") and bridge.get("producer_copies") not in (0, None):
        print(f"  {name}: {bridge['producer_copies']} producer copies on a direct case", file=sys.stderr)
        problems += 1
    slot_trace = {tuple(p) for p in bridge.get("slot_trace", [])}
    for c in case["consumers"]:
        # A deliberately killed consumer is allowed to be short; corruption never is.
        for key in ("content_mismatches", "shape_mismatches", "dtype_mismatches",
                    "timestamp_mismatches", "sequence_gaps", "out_of_order",
                    "not_zero_copy", "lease_violations"):
            problems += c.get(key, 0)
        observed = {tuple(p) for p in c.get("slots", [])}
        if slot_trace and not observed <= slot_trace:
            print(f"  {name}/{c.get('role')}: read a slot the producer never wrote", file=sys.stderr)
            problems += 1
print("problems across all live soak cases:", problems)
sys.exit(1 if problems else 0)
PY
