#!/usr/bin/env bash
# Phase 4A soak: real TenzorPipe media through the real ring, harder than the
# unittest gate. Needs the tenzorpipe package, ffmpeg and the built extension.
#
# Usage: scripts/run_integration_soak.sh [output.json]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/integration_soak_results.json}"
export PYTHONPATH="$ROOT/src${PYTHONPATH:+:$PYTHONPATH}"
DEMO="$ROOT/demos/tenzorpipe_to_bus.py"
FIXTURES="$ROOT/fixtures"
mkdir -p "$FIXTURES"

LONG="$FIXTURES/soak_720p_60s.mp4"
if [ ! -f "$LONG" ]; then
  echo "generating a 60 s 720p H.264/AAC fixture"
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=1280x720:rate=30:duration=60" \
    -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=60" \
    -c:v libx264 -pix_fmt yuv420p -g 60 -bf 2 -c:a aac -shortest "$LONG"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
: > "$TMP/parts"

soak_case() {
  local name="$1"; shift
  echo "== $name =="
  set +e
  python3 "$DEMO" run --media "$LONG" "$@" > "$TMP/$name.out" 2>"$TMP/$name.err"
  local rc=$?
  set -e
  python3 - "$TMP/$name.out" "$name" "$rc" >> "$TMP/parts" <<'PY'
import json, sys
text = open(sys.argv[1]).read()
name, rc = sys.argv[2], int(sys.argv[3])
dec, i, report = json.JSONDecoder(), 0, None
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
    if isinstance(obj, dict) and "consumer_reports" in obj:
        report = obj
    i = end
if report is None:
    report = {"error": "no report", "consumer_reports": []}
report["case"] = name
report["exit_code"] = rc
print(json.dumps({name: report}) + ",")
PY
  echo "[$name] exit=$rc"
}

soak_case fanout8            --consumers 8 --slots 16
soak_case saturated_2slot    --consumers 8 --slots 2  --sleep-max-ms 4
soak_case crash_midstream    --consumers 6 --slots 3  --die-after 17
soak_case single_consumer    --consumers 1 --slots 4
soak_case tight_slot_fit     --consumers 4 --slots 2

python3 - "$TMP/parts" "$OUT" <<'PY'
import json, platform, subprocess, sys
parts = open(sys.argv[1]).read().strip().rstrip(",")
merged = {}
for chunk in json.loads("[" + parts + "]"):
    merged.update(chunk)
try:
    cpus = int(subprocess.run(["nproc"], capture_output=True, text=True).stdout.strip())
except Exception:
    cpus = None
out = {
    "host": f"{platform.system()} {platform.release()} {platform.machine()}",
    "cpus": cpus,
    "python": platform.python_version(),
    "cases": merged,
}
json.dump(out, open(sys.argv[2], "w"), indent=2)
print(f"wrote {sys.argv[2]}")

bad = 0
for name, case in merged.items():
    for c in case.get("consumer_reports", []):
        # A deliberately killed consumer is allowed to be short; corruption never is.
        bad += (c.get("content_mismatches", 0) + c.get("shape_mismatches", 0)
                + c.get("dtype_mismatches", 0) + c.get("timestamp_mismatches", 0)
                + c.get("out_of_order", 0) + c.get("not_zero_copy", 0)
                + c.get("lease_violations", 0) + c.get("sequence_gaps", 0))
    if case.get("published") != case.get("epochs"):
        print(f"  {name}: producer did not publish every epoch", file=sys.stderr)
        bad += 1
print("corruption / ordering / zero-copy violations across all soak cases:", bad)
sys.exit(1 if bad else 0)
PY
