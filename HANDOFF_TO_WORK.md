# TenzorBus → Work handoff

> Historical implementation plan, retained for provenance. The work described
> here is complete; use `README.md` and `FINAL_VERIFICATION.md` going forward.

## Goal

Turn the executed v0.1 Python shared-memory reference into the production Rust TenzorBus transport **without changing the proven semantics**, then run the full real TenzorPipe → TenzorBus → multi-consumer demo.

Do not redesign this into a model server, networking framework, distributed cluster, or game SDK yet.

## What is already proven

Executed in the ChatGPT sandbox:

- real TenzorPipe v0.2.0 release binary ingests generated H.264/AAC media
- two independent TenzorPipe runs produce byte-identical Arrow IPC artifacts
- TenzorBus reference ring is bounded
- active readers are never overwritten
- NumPy consumer view aliases the shared-memory slot
- PyTorch `frombuffer` consumer view aliases the shared-memory slot
- two separate consumer processes receive the same sequence/slot publication
- cross-process reference benchmark completed
- local HTTP/JSON comparison completed
- first lifetime bug (NumPy view outliving lease) was reproduced and fixed/documented

Current test result: **7 discovered / 6 pass / 1 skip**. Skip reason: sandbox lacks `pyarrow`, so the Python TenzorPipe loader cannot open the generated `.tenzor` file here.

## Source-of-truth files

- `src/tenzorbus/protocol.py` — executed byte layout
- `src/tenzorbus/ring.py` — executed behavior/reference semantics
- `tests/test_ring.py` — transport invariants
- `tests/test_tenzorpipe_release.py` — actual TenzorPipe binary proof
- `benchmarks/*.py` — measurement harnesses
- `rust/tenzor-core/src/lib.rs` — C-compatible tensor header/layout scaffold
- `rust/tenzorbus/src/lib.rs` — production crate scaffold
- `docs/PROTOCOL.md` — ownership/backpressure contract
- `benchmark_results.json`
- `benchmark_cross_process.json`

## Phase 1 — compile and freeze protocol

1. Use Rust 1.98.1+ initially for parity with the TenzorPipe release toolchain.
2. Compile `rust/` and make every Rust unit test pass.
3. Add byte-layout tests that compare Rust offsets/sizes against Python `protocol.py`.
4. Add golden header bytes produced by Python and parsed by Rust, and vice versa.
5. Do not change the protocol casually after this gate.

## Phase 2 — Linux production ring

Implement Linux first:

- named `shm_open`/`mmap` or an equally portable named shared mapping strategy
- atomic slot state transitions
- release/acquire ordering around commit and reclaim
- consumer registration table with PID/heartbeat/generation
- crash recovery so a dead consumer cannot permanently pin a slot
- futex/eventfd/native wakeup so consumers do not poll
- block + drop-newest policies
- one-producer/multi-consumer contract first

Stress tests:

- 1 producer / 1 consumer: 10M small publications
- 1 producer / 8 consumers
- random consumer sleeps
- kill -9 consumer while holding a lease
- kill producer mid-write
- wrap sequence counter in a test build
- random tensor sizes/ranks/dtypes
- full ring for minutes under block policy
- full ring under drop-newest policy
- verify no stale/torn payload is ever observed

## Phase 3 — Python bindings

Build PyO3 bindings with:

```python
bus = tenzorbus.create("frames", slots=16, slot_bytes=...)
producer = bus.producer()
consumer = tenzorbus.attach("frames").consumer()

with consumer.next() as lease:
    t = lease.torch()      # direct view, no consumer copy
```

Lifetime must be explicit. A tensor/view cannot safely outlive its lease unless copied.

## Phase 4 — current TenzorPipe integration

The sandbox artifact available for executed integration is v0.2.0. That remains historical executed evidence only. **The production integration target is TenzorPipe v0.3.2**; rebase and validate against the v0.3.2 source/release before publication.

Do not merely read a `.tenzor` file and call that integration.

First integration gate:

```text
MP4
 ↓
actual TenzorPipe engine
 ↓
actual video tensor
 ↓
TenzorBus slot
 ↓
consumer A: detector/test checksum
consumer B: embedder/test checksum
consumer C: preview/test checksum
```

Stage 1 may use the existing Python loader and incur the one producer copy into SHM.

Stage 2 should add an **opt-in direct writer** so cooperating TenzorPipe code reserves a TenzorBus slot and fills it directly. Preserve the existing `.tenzor` writer and all TenzorPipe output invariants; the integration must remain backward-compatible, not become a breaking rewrite.

Run TenzorPipe's entire current regression suite after every shared-core extraction.

## Phase 5 — fair benchmarks

Do not publish the placeholder numbers from early mockups.

Benchmark at least:

- raw bytes over localhost Unix socket / named pipe
- localhost HTTP binary body
- localhost HTTP JSON/base64 (anti-pattern comparison)
- TenzorBus producer-copy path
- TenzorBus direct-write path
- 1, 2, 4, 8 consumers

Report:

- median / p95 / p99
- payload sizes: 64 KiB, 588 KiB, 4 MiB, 16 MiB
- CPU usage
- RSS
- throughput
- copies attributable to each path
- exact machine/OS/compiler

Keep inference time out of transport latency.

## Phase 6 — cross-platform

After Linux correctness:

- Windows named file mappings + native wait/wake
- macOS POSIX shared memory / mapping + native synchronization
- C ABI tests across compilers

## Phase 7 — Live Lab

Only after the production ring is stable, connect `live_lab/` to actual runtime telemetry.

The UI should stay consumer-readable:

```text
TenzorPipe → TenzorBus → Detector / Embedder / Preview
```

Advanced details can show queue depth, transport latency, tensor shape, active consumers and backpressure.

The demo must clearly label whether a measurement is:

- TenzorPipe decode
- TenzorBus transport
- model inference
- end-to-end

Never blend them into one number.

## Release gate

Do not call v0.1 production-ready until:

- Rust implementation compiled/tested on Linux
- full current TenzorPipe integration passes
- 8-consumer stress passes
- dead-consumer recovery passes
- benchmark methodology reviewed
- licensing/notices reviewed
- GitHub CI reproduces all core tests from a clean clone
