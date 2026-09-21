#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PYTHONPATH="$ROOT/src${PYTHONPATH:+:$PYTHONPATH}"
python3 "$ROOT/benchmarks/benchmark_transport.py" --iterations 1000 --output "$ROOT/benchmark_results.json"
python3 "$ROOT/benchmarks/benchmark_cross_process.py" --iterations 300 --output "$ROOT/benchmark_cross_process.json"
