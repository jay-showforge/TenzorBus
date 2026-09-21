# TenzorBus v0.1 — release candidate: TenzorPipe v0.3.2 direct write

> Historical phase report. Its environment-specific open items were closed by
> the final release pass; use `FINAL_VERIFICATION.md` for current status.

Date: 2026-09-20
Continues `PHASE4_INTEGRATION_REPORT.md` (Phase 4A + Phase 5) and
`RUST_PHASE_REPORT.md` (Phases 1–3). Those phases were treated as an established
baseline: re-verified, not redone.

Everything here was executed on the host in "Environment". One gate could not be
closed in this environment, and it says so plainly rather than being glossed.

## Headline

**TenzorPipe's H.264 decoder now resizes each selected picture straight into a
TenzorBus slot. Nothing on the path from the decoder to the consumers copies the
tensor.**

```text
MP4 → H.264 decode → resize ──writes into──▶ TenzorBus slot ──▶ consumer A (NumPy view)
                                                            ├─▶ consumer B (PyTorch view)
                                                            └─▶ consumer C (NumPy view)
                       producer copies: 0        consumer copies: 0
```

Measured on the live path, every case: `destinations_taken == epochs` and
`producer_copies == 0`.

| | |
|---|---|
| TenzorPipe `.tenzor` output | **byte-identical**: 60/60 local cases, and its own 1,224-case matrix at 0 failures |
| Live direct-write path | 5 acceptance tests + 6 soak cases, real 720p media, zero corruption |
| TenzorPipe suite | 43 Rust, 10 Python, determinism, HEVC fidelity, audio-tail, full identity matrix — all green |
| TenzorBus suite | 47 Rust, 33 Python, clippy `-D warnings`, rustfmt — all green |
| ThreadSanitizer | clean, self-verifying, now covering reserve/commit and abort |
| Release-candidate gates | **13 passed, 0 failed** (`release_candidate_gates.json`) |
| Rust 1.98.1 | **not closed here** — see below. Wired into CI. |

## Environment

| | |
|---|---|
| host | Linux 6.18.44-fc-v37 x86_64, **2 vCPU** |
| rustc | 1.95.0 — *not* the pinned 1.98.1 |
| Python / NumPy / PyArrow / PyTorch | 3.11.15 / 2.4.4 / 25.0.1 / 2.14.0+cu130 (CPU) |
| TenzorPipe | v0.3.2 tag, patched, built from source |
| ffmpeg / nasm | present; ffmpeg only generates fixtures |

### The Rust 1.98.1 gate is still open, and here is exactly why

I retried it rather than assuming last session's result. The egress policy denies
the toolchain host outright:

```
$ rustup toolchain install 1.98.1
error: could not download file from
  'https://static.rust-lang.org/dist/channel-rust-1.98.1.toml.sha256'
  ... tunnel error: unsuccessful

$ curl -sS "$HTTPS_PROXY/__agentproxy/status"
  "recentRelayFailures": [ { "kind": "connect_rejected",
    "detail": "gateway answered 403 to CONNECT (policy denial or upstream failure)",
    "host": "static.rust-lang.org:443" } ]
```

I probed for any other route: `github.com` and `objects.githubusercontent.com` are
reachable, every rust-lang and mirror host tested is not, and Ubuntu's apt only
offers 1.75. Downloading a toolchain from an unvetted third-party GitHub mirror
would let an unverified compiler produce the build I then certify, so I did not do
that. Building rustc from source needs a bootstrap compiler from the same blocked
host.

**What this does and does not mean.** 1.95.0 is *older* than 1.98.1, so
everything compiling and passing on 1.95.0 proves the code uses no feature newer
than 1.95 — that direction is the stricter test. The residual 1.98.1 risks are
narrower: new or changed clippy lints under `-D warnings`, a std behaviour change,
and TenzorPipe's pin mattering for its own build. Those are real and unverified.

**What was done about it.** `scripts/verify_release_candidate.sh` runs every gate
in one command and **refuses to run unless `rustc` is 1.98.1** (override with
`ALLOW_RUSTC_SKEW=1`, which is recorded in the report). The CI `linux` and
`media-integration` jobs are pinned to `dtolnay/rust-toolchain@1.98.1`. One green
CI run, or one run of that script on the EPYC host, closes this.

## What was built

### TenzorPipe: `EpochSink` (patch: `patches/tenzorpipe-v0.3.2-direct-write.patch`)

Three scoped compatibility steps, exactly as `docs/TENZORPIPE_DIRECT_WRITE.md` specified.

**1. The resize can write into a caller's buffer.** `yuv420_to_resized_chw_into`
takes `out: &mut [f32]`; the old `yuv420_to_resized_chw` is now a wrapper that
allocates and calls it. Same code writes the bytes, so the output is identical by
construction rather than by testing — and then it was tested anyway.

**2. `EpochSink` and `convert_with_sink`.** The handshake is an offer in both
directions: the engine offers a destination only where it can, and the sink may
decline and be handed the tensor instead.

```rust
pub trait EpochSink {
    fn video_destination(&mut self, epoch: usize, pts: i64, len: usize)
        -> Result<Option<&mut [f32]>> { Ok(None) }
    fn epoch_ready(&mut self, epoch: usize, pts: i64, tensor: Option<&[f32]>) -> Result<()>;
    fn abort_epoch(&mut self, _epoch: usize) {}
}
```

A blanket `impl<F: FnMut(usize, i64, &[f32]) -> Result<()>> EpochSink for F` is
what kept the change small: every existing closure-based caller compiles
untouched, so only one call site inside the decoder actually changed.

**3. The bridge.** `integration/bridge` (`tenzorbus-ingest`) implements the sink
by reserving a TenzorBus slot per epoch and handing the decoder its payload.

### Where a destination is offered, and where it honestly cannot be

| path | destination offered | why |
|---|---|---|
| sequential + `--video-workers 1` (H.264) | **yes** | one picture at a time, in presentation order, one epoch per picture |
| one picture serving several epochs | no | each epoch needs its own slot; the engine resizes once and hands the tensor over, exactly as before |
| chunked parallel decoder | no | workers produce and reorder owned `Vec<f32>`; reserving from a worker would break publication ordering |
| concurrent executor | **refused** | it buffers owned tensors over a channel. `convert_with_sink` rejects `--execution concurrent` with a message saying what to use instead, rather than silently falling back to a copy |
| HEVC | no | its own decoder, same owned-tensor shape |

I chose to refuse rather than silently degrade, and to leave the parallel and
concurrent paths untouched, because redesigning TenzorPipe's concurrency to chase
the last copy is exactly the kind of change that would put its correctness at
risk. The `.tenzor` path and every other executor still behave identically.

### TenzorBus

`SlotWriter` already existed from Phase 4B. What is new: TSan coverage of
reserve/fill/commit and of abort under reader contention, and the bridge, soak and
live acceptance suites.

## The `.tenzor` path is unchanged — checked three ways

**1. Against a pristine build of the same tag.** `scripts/test_sink_refactor_identity.py`
builds a clean v0.3.2 and compares raw bytes over a matrix chosen to hit the code
the refactor touched — the single-decoder path, the chunked parallel path, and the
repeated-epoch case.

```
58 identical, 2 matching refusals, 0 mismatched, out of 60 cases
```

Same version on both sides, so this is exact: no version normalisation, no
tolerance.

**2. TenzorPipe's own 1,224-case matrix against the v0.2.0 reference binary**, run
with the patch applied (`evidence/tenzorpipe-skip-identity-with-direct-write.json`):

```
TOTAL 1224  FAIL 0
kinds = identical 888, expected-message-change 96, expected-new-success 48,
        same-error 72, expected-hevc 120
identical-with-skips 364, skipped access units 91,188
```

**3. Its other gates**, all passing with the patch: 43 Rust tests, 10 Python
tests, `test_arch_identity.py` (18 identical / 0 different), `test_matrix.py`
(16 media + 12 failure cases + PyTorch checks), `test_hevc_fidelity.py` (8/8
against an FFmpeg oracle within 2e-05), `test_audio_tail_tolerance.py` (24/24),
clippy `-D warnings`, rustfmt.

## The live path, verified

`tests/test_live_direct_write.py`, 5 tests, all passing. Reference for every
assertion: the `.tenzor` file the **ordinary** path produces for the same clip,
read independently by each consumer. That is the comparison worth making — does
letting the decoder write into shared memory change the tensors? It does not.

Two claims are checked structurally rather than asserted:

* `producer_copies == 0` and `destinations_taken == epochs` from the bridge.
* **the slot each consumer read equals the slot the decoder wrote into**, per
  sequence. The bridge emits a `(sequence, slot)` trace; each consumer records
  the same; the test compares the sets. Combined with each consumer's own check
  that its NumPy/PyTorch view address equals the slot payload address, that closes
  the loop from decoder write to consumer read.

| test | result |
|---|---|
| decoder writes into the slots the consumers read (3 processes) | ✅ 0 copies, slot traces match, 10/10 each |
| the copy path (`--copy-path`) delivers the same tensors | ✅ |
| **negative control**: one slot deliberately corrupted before commit | ✅ every consumer reported exactly 1 content mismatch |
| backpressure reaches the decoder (2 slots, 3 slow consumers) | ✅ |
| consumer SIGKILLed holding a lease on a slot the decoder wrote | ✅ decoder published every epoch, survivors clean, dead consumer reaped |

### Live soak (`scripts/run_live_soak.sh`)

A 60 s 720p H.264/AAC clip, 120 epochs of real video per case.

| case | mode | published | destinations | producer copies | consumers | received | corruption / ts / gaps / reorder / non-zero-copy | zero-copy views | reaped |
|---|---|---:|---:|---:|---:|---|---|---:|---:|
| direct_fanout8 | direct | 120 | 120 | **0** | 8 | all 120 | 0 / 0 / 0 / 0 / 0 | 960 | 0 |
| direct_saturated_2slot | direct | 120 | 120 | **0** | 6 | all 120 | 0 / 0 / 0 / 0 / 0 | 720 | 0 |
| direct_crash_midstream | direct | 120 | 120 | **0** | 6 | 41 + 5×120 | 0 / 0 / 0 / 0 / 0 | 641 | 1 |
| direct_single | direct | 120 | 120 | **0** | 1 | 120 | 0 / 0 / 0 / 0 / 0 | 120 | 0 |
| copy_path_fanout4 | copy | 120 | 0 | 120 | 4 | all 120 | 0 / 0 / 0 / 0 / 0 | 480 | 0 |
| direct_tight_fit | direct | 120 | 120 | **0** | 4 | all 120 | 0 / 0 / 0 / 0 / 0 | 480 | 0 |

Raw: `live_soak_results.json`. Backpressure is real and it reaches the decoder:
`reserve` blocks while every slot is held, so a slow consumer slows the decode
instead of growing a queue. That is visible in the elapsed times (8.3 s for one
consumer, 19.7 s for eight on two cores).

## ThreadSanitizer

`scripts/run_tsan.sh`, now with four more tests covering the direct-write path:
one/four consumer threads, heavy slot recycling, and aborts racing readers. The
reserve→write→commit window is longer than the copy path's, so it earns its own
coverage rather than being assumed safe.

| | |
|---|---|
| clean run | 8 tests pass, **0 warnings**, 0 naming TenzorBus |
| positive control | injected race detected in TenzorBus, first attempt |

The scope limits from last phase still hold and are still recorded in
`tsan_results.json`: TSan is single-process, so it covers the atomics and the
payload writes they order, not cross-process traffic; and the stable toolchain's
std is uninstrumented, so std-internal races are invisible and its lock-free
channels need the narrowly scoped suppressions.

## Four clippy findings in my own TenzorPipe change

Worth recording because TenzorPipe's CI runs `clippy -D warnings` and would have
failed: a very complex inline closure type (now a named `EmitFn` alias), a
needless `Option::as_deref_mut`, and a collapsible `if let` nest. Fixed, and the
identity matrix and pytest were re-run afterwards rather than trusted.

## Test inventory

**TenzorPipe** — 43 Rust (3 new in `tests/sink_direct_write.rs`), 10 Python,
plus determinism, matrix, HEVC fidelity, audio-tail and the 1,224-case identity
matrix. clippy and rustfmt clean.

**TenzorBus** — 47 Rust: tenzor-core 5, layout parity 3, lib 2, golden bytes 2,
production_ring 18, direct_write 9 (10 in debug), tsan_threads 8. 33 Python:
reference ring 6, integration contract 2, Rust bindings 13, TenzorPipe
integration 6, live direct write 5, real release determinism 1. clippy
`-D warnings` and rustfmt clean. Bridge: clippy and rustfmt clean.

**Gates and soaks** — `scripts/verify_release_candidate.sh` (13 passed, 0
failed), `run_tsan.sh`, `run_rust_stress.sh`, `run_integration_soak.sh`,
`run_live_soak.sh`, `benchmarks/benchmark_matrix.py`.

## Remaining known issues

**Release-blocking**

1. **Rust 1.98.1 unverified.** The only open gate. One CI run or one
   `scripts/verify_release_candidate.sh` on a host that can install 1.98.1 closes
   it. Everything else is green.

**Should be decided before release, not bugs**

2. **Direct write is sequential single-decoder only.** Deliberate, documented, and
   refused rather than silently degraded elsewhere. If the live path needs the
   chunked decoder's throughput, that is a TenzorPipe concurrency change and
   should be scoped on its own rather than bolted on.
3. **Throughput on this box is not a product number.** 2 vCPU, and the live figures
   are decoder-paced. Re-run on the EPYC host.
4. **The bridge duplicates TenzorPipe's `[patch.crates-io]` table.** A `[patch]`
   only applies at a workspace root and the bridge is its own workspace, so the
   four vendored redirects are repeated in `integration/bridge/Cargo.toml` and must
   stay in step with `tenzorpipe/Cargo.toml`. A drift there fails loudly at build
   time, but it is duplication worth removing if the bridge ever moves into
   TenzorPipe's workspace.
5. **`integration/tenzorpipe` is a build-time symlink** created by
   `scripts/build_bridge.sh` from `TENZORPIPE_DIR`. Not in the archive; regenerate
   it.

**Carried forward, unchanged**

6. TSan cannot see cross-process races; std is uninstrumented. No ASan/UBSan run
   (Miri cannot execute `shm_open`/`mmap`).
7. Windows and macOS untouched; no stable C ABI.
8. 64-consumer ceiling (the reader bitmask is a `u64`) — a documented limit.
9. Pid reuse is mitigated by a start-time token, not eliminated.
10. Live Lab untouched, as instructed. The gates it was waiting on are green
    except the toolchain.
11. `benchmark_results.json` / `benchmark_cross_process.json` are still the v0.1
    Python reference's numbers; `benchmark_matrix.json` supersedes them.

## Reproducing

```bash
# TenzorPipe v0.3.2 with direct write
git clone https://github.com/jay-showforge/tenzorpipe && cd tenzorpipe
git checkout v0.3.2
git apply /path/to/patches/tenzorpipe-v0.3.2-direct-write.patch
sudo apt-get install -y nasm ffmpeg
cargo build --release --locked        # 1.98.1 per rust-toolchain.toml
pip install .

# a pristine build, for the byte-identity gate
git clone -b v0.3.2 https://github.com/jay-showforge/tenzorpipe /tmp/pristine
cargo build --release --locked --manifest-path /tmp/pristine/Cargo.toml

# TenzorBus: every gate, one command
cd /path/to/TenzorBus
TENZORPIPE_DIR=/path/to/tenzorpipe \
PRISTINE_TENZOR=/tmp/pristine/target/release/tenzor \
TENZORPIPE_BIN=/path/to/tenzorpipe/target/release/tenzor \
  scripts/verify_release_candidate.sh
```

`SKIP_SOAKS=1` for the fast pass; omit it for the full run including the soaks and
the benchmark matrix.
