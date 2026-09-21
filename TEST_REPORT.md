# TenzorBus v0.1 alpha — executed test report

> Historical early-phase report. Missing dependencies and skipped tests noted
> here were resolved later; use `FINAL_VERIFICATION.md` for current status.

Date: 2026-09-20

## Environment

- Linux ChatGPT execution sandbox
- Python 3.13.5
- CPU-only PyTorch 2.10.0 available globally
- FFmpeg 7.1.5 available as fixture generator only
- Rust toolchain: **not installed**
- `pyarrow`: **not installed**, and outbound package installation is blocked
- Real TenzorPipe artifact used: **v0.2.0 release bundle** from the user's prior project files

## Default unit/integration suite

Command:

```bash
export PYTHONPATH="$PWD/src"
export TENZORPIPE_BIN=/path/to/tenzor-v0.2.0
python3 -m unittest discover -s tests -v
```

Result:

```text
Ran 7 tests
OK (skipped=1)
```

Passed:

1. bounded ring refuses to overwrite active readers
2. two separate consumer processes receive the same publication/slot
3. NumPy view aliases the shared-memory payload
4. PyTorch `frombuffer` view aliases the shared-memory payload
5. actual TenzorPipe release ingests generated H.264/AAC media
6. two TenzorPipe ingests produce byte-identical Arrow IPC outputs

Skipped:

- full Python TenzorPipe loader → TenzorBus bridge because `pyarrow` is unavailable in this sandbox

The runnable adapter is `demos/tenzorpipe_to_bus.py`. This is the first test Work/CI should execute after installing the current TenzorPipe package plus PyArrow.

## Real TenzorPipe engine smoke

Fixture:

- 5.0 seconds
- H.264, 640×360, 30 fps, B-frames
- AAC, 48 kHz

Command used the genuine release binary with `--video-workers 1 --profile`.

Observed:

- decoded access units: 150
- selected/resized video tensors: 10
- wall time reported by TenzorPipe: **0.162 s** on one run
- max RSS from `/usr/bin/time -v`: **23,276 KiB**
- output size: ~6.1 MiB
- Arrow file magic at start/end: PASS
- repeat SHA-256 identity: PASS

Both output hashes:

```text
ced94921c5d3e330a9b2ade7ccdec687ed294859458d3febb07884fb1f117d8c
```

This confirms actual TenzorPipe code is part of the test story, rather than a simulated tensor generator.

## Shared-memory 8-consumer stress

Script: `tests/stress_multi_consumer.py`

Configuration:

- 1 producer
- 8 independent consumer processes
- 500 sequential tensor publications
- 4,000 total consumer deliveries
- shape `[3,64,64]` float32
- alternating consumers add random scheduling jitter up to 1 ms

Result:

- **PASS**
- corrupted deliveries: **0**
- sequence gaps/reordering: **0**
- dropped publications: **0**
- every consumer received sequences 1…500 exactly
- elapsed: **0.412 s**
- reference publisher throughput during this test: **~1,213 publications/s**

Raw result: `stress_results.json`.

## Reference benchmark 1 — shared memory vs local serialization

Tensor: `[3,224,224]`, float32, 602,112 bytes.

1,000 iterations.

| Path | Median | p95 |
|---|---:|---:|
| TenzorBus reference: producer copy + zero-copy consumer | **0.0388 ms** | **0.0549 ms** |
| base64 + JSON encode/decode | **3.4303 ms** | **4.4010 ms** |

Measured median ratio: **88.45×**.

This is a serialization microbenchmark, not an HTTP round trip and not a production performance claim.

Raw result: `benchmark_results.json`.

## Reference benchmark 2 — true cross-process handoff

Tensor: `[3,224,224]`, float32, 602,112 bytes.

300 iterations.

| Path | Median | p95 | Max |
|---|---:|---:|---:|
| TenzorBus Python reference, separate consumer process | **0.2725 ms** | **0.3557 ms** | 1.0342 ms |
| localhost HTTP + prebuilt base64 JSON body | **2.2133 ms** | **3.1428 ms** | 12.8517 ms |

Measured median ratio: **8.12×**.

The HTTP test intentionally prebuilds the sender JSON/base64 payload, excluding sender serialization and therefore favoring HTTP. It still includes localhost HTTP transport, server parse, base64 decode and receiving array view.

Raw result: `benchmark_cross_process.json`.

## Packaging / UI smoke

- Python package `py_compile`: PASS
- Python wheel build: PASS
- wheel install in fresh venv: PASS
- installed-wheel shared-memory publish/read: PASS
- Tenzor Live Lab HTTP page: PASS (HTTP 200)
- benchmark JSON endpoint: PASS (HTTP 200)

Built wheel:

```text
dist/tenzorbus-0.1.0a0-py3-none-any.whl
SHA-256 cbffc5e900eef3c8e590ef87163cd32329650778166a89f51d5fce087f5283af
```

## Bug found during testing

The initial two-process test retained a NumPy zero-copy view after releasing the slot lease. Python correctly raised:

```text
BufferError: cannot close exported pointers exist
```

The test and demo were corrected so derived zero-copy views are destroyed before lease release. The protocol documentation now makes this lifetime rule explicit. This is an important API invariant for the production PyO3 implementation.

## Environment-limited items — not claimed as passing

1. Rust crates have not been compiled here because `rustc`/`cargo` are absent.
2. Full `.tenzor` → PyArrow → PyTorch → TenzorBus live bridge has not executed here because PyArrow is absent.
3. Current public TenzorPipe source should be used for the production integration; the locally executable retained artifact is v0.2.0.
4. Windows/macOS mapping and synchronization have not been implemented or tested.
5. Crash recovery after a consumer dies while holding a slot remains a Rust production task.

Those are explicit Work gates in `HANDOFF_TO_WORK.md`.
