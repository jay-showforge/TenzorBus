# TenzorBus Phase 4 — real TenzorPipe v0.3.2 integration, and Phase 5 benchmarks

> Historical phase report. Direct write and the remaining release gates were
> completed later; use `FINAL_VERIFICATION.md` for current status.

Date: 2026-09-20
Continues `RUST_PHASE_REPORT.md` (Phases 1–3). Phases 1–3 were treated as an
established baseline and re-verified, not redone.

Everything in this document was executed on the host in "Environment". Nothing is
mocked, estimated, or carried over from an earlier report. Where something could
not be done here, it says so and says why.

## Summary

| | |
|---|---|
| Baseline (Phases 1–3) | re-verified green before any change |
| TenzorPipe v0.3.2 | built from source at tag `v0.3.2`, its own suite green |
| Phase 4A | **done** — real MP4 → real tensors → ring → 3–8 independent consumers, byte-exact |
| Phase 4B | **TenzorBus half done and proven**; the media path needs one scoped, backward-compatible TenzorPipe API (specified in `docs/TENZORPIPE_DIRECT_WRITE.md`) |
| ThreadSanitizer | **clean**, and self-verifying via a positive control |
| Phase 5 | 49-case matrix, zero failures |
| Bugs found and fixed | 4 (2 in TenzorBus, 1 in the v0.1 reference, 2 in TenzorPipe's repo — see below) |
| Tests | 43 Rust, 28 Python, 0 skips |

## Environment

| | |
|---|---|
| host | Linux 6.18.44-fc-v37 x86_64, **2 vCPU** |
| rustc | 1.95.0 (59807616e 2026-04-14) |
| Python / NumPy / PyArrow / PyTorch | 3.11.15 / 2.4.4 / 25.0.1 / 2.14.0+cu130 (CPU) |
| TenzorPipe | 0.3.2, built from source, `bin/tenzor-linux-x86_64` is 0.3.0 |
| ffmpeg | present, used only to generate fixtures |

**Two vCPUs matters for every number below.** With a producer plus eight consumer
processes on two cores, the fan-out figures measure CPU contention as much as
transport. Treat the shapes as real and the absolute values as this box's.

### The 1.98.1 toolchain item is still open, and now for a hard reason

TenzorPipe pins `1.98.1` in `rust-toolchain.toml`. `static.rust-lang.org` is blocked
by this environment's egress policy, so rustup cannot fetch it:

```
error: could not download file from 'https://static.rust-lang.org/dist/channel-rust-1.98.1.toml.sha256'
  ... tunnel error: unsuccessful
```

Both TenzorPipe and TenzorBus were therefore built with 1.95.0
(`RUSTUP_TOOLCHAIN=stable`). Everything compiles and TenzorPipe's own tests pass,
but **the pinned-toolchain build remains unverified** and stays a release-gate item.

## Baseline re-verification

Before touching anything:

```
scripts/build_rust.sh && cd rust && cargo test --release   → 28 tests, all pass
scripts/run_tests.sh                                       → 16 tests, OK (1 skip)
```

The single skip was `TENZORPIPE_BIN` being unset. It is now set and passing.

## Getting real TenzorPipe v0.3.2

Cloned `github.com/jay-showforge/tenzorpipe`, checked out `v0.3.2`, built with
`cargo build --release --locked`, and installed the Python package with its native
engine (`pip install .`, maturin/PyO3).

Two build prerequisites worth recording: `nasm` is required (the vendored OpenH264
refuses to fall back silently — a good failure), and the `openh264` C++ sources
build with the system compiler.

### TenzorPipe's own regression suite

| | |
|---|---|
| `cargo test --release --locked` | **40 tests pass** |
| `pytest tests/` | 10 pass, 1 skipped (`--run-matrix`, ~13 min) — **after the two fixes below** |

### Two real defects in the TenzorPipe v0.3.2 repo

Both block anyone running the suite at that tag. A patch is included as
`patches/tenzorpipe-v0.3.2-test-fixes.patch`.

**1. `bin/tenzor-linux-x86_64` is committed non-executable.**
`git ls-files -s` shows mode `100644`. A fresh clone therefore fails
`test_python_output_matches_the_engine_binary` with
`PermissionError: [Errno 13] Permission denied` before it compares anything.
Fix: `git update-index --chmod=+x`, plus the test now skips with a clear reason
rather than erroring if the binary will not run.

**2. That binary is 0.3.0, and the test compares bytes against it.**
`./bin/tenzor-linux-x86_64 --version` → `tenzor 0.3.0`, while the source tree is
0.3.2. The test asserts raw byte equality, and the engine version is embedded in
every artifact, so it cannot pass at the v0.3.2 tag.

I verified precisely what differs rather than assuming:

```
sizes: 12687682 12687682   equal length: True
differing byte count: 2
offsets: [700, 12686468]
  @700:       py=b'2' bin=b'0'   ctx: ...origin......0.3.2...
  @12686468:  py=b'2' bin=b'0'   ctx: ...origin......0.3.2...
occurrences of b'0.3.2' in py: 2 ; of b'0.3.0' in bin: 2
normalised equal: True
```

Two bytes out of 12,687,682, both the `origin` metadata version string, once per
footer copy. **The tensors are byte-identical** — the 0.3.2 engine and the 0.3.0
binary produce the same output. The defect is in the test's version fragility, not
in the engine.

Fix: the test now normalises the embedded version when the committed binary is a
different release, using the same substitution — and the same "exactly 2
occurrences" assertion — that the project's own
`scripts/test_skip_identity.py::substituted_sha256` already performs for the same
reason. That keeps the test comparing tensors, which is its purpose, while surviving
a version bump. Refreshing the committed binary would also fix it; normalising is
the option that does not require shipping a new binary.

## The real TenzorPipe v0.3.2 contract

Probed, not assumed:

```
ingest: {'epochs': 20, 'seconds': 2.315}
video_shape (3, 224, 224)   audio_shape (50, 64)   has_video True   has_audio True
schema: timestamp_ms, audio_valid_frames, video_timestamp_ms, video_tensor, audio_mel_tensor
  video               torch.float32  (20, 3, 224, 224)   values in [-1, 1]
  audio               torch.float32  (20, 50, 64)
  timestamp_ms        torch.int64    [0, 500, 1000, ...]
  video_timestamp_ms  torch.int64
numpy view: C-contiguous, 602,112 bytes per frame
```

602,112 bytes is the real video tensor size, and it is the pivot payload in the
benchmark matrix for that reason.

Public API surface: `tenzorpipe.ingest(input, output, ...)` and
`tenzorpipe.load(path)`; in Rust, `cli::convert(argv)` and nothing else —
`storage`, `video`, `media` are all private. That fact is what shapes Phase 4B.

## Phase 4A — done

`demos/tenzorpipe_to_bus.py` was rewritten against the real v0.3.2 API. It ingests
an MP4 with the actual engine, then publishes every epoch's video tensor into the
ring carrying that epoch's media timestamp, while N consumers run as separate OS
processes.

**No consumer trusts the producer.** Each one opens the same `.tenzor` file
independently and re-derives the expected tensor from TenzorPipe itself, then checks
the bytes it received against it. Verified per publication: shape, dtype, byte-exact
contents, media timestamp, sequence ordering, exactly-once delivery, and that the
NumPy or PyTorch view still aliases the slot.

```
MP4 → TenzorPipe → .tenzor → producer (1 copy, media timestamp) → ring
                                                                    ├── detector  (NumPy, zero-copy)
                                                                    ├── embedder  (PyTorch, zero-copy)
                                                                    └── preview   (NumPy, zero-copy)
```

### The gate can fail

A verification that always passes proves nothing, so `--inject` deliberately
mis-publishes and the tests require the consumers to catch it:

| injected fault | result |
|---|---|
| two frames published under each other's sequence | every consumer reported **exactly 2** content mismatches |
| two timestamps swapped | every consumer reported **exactly 2** timestamp mismatches, 0 content mismatches |

I also checked the fixture is not degenerate: all 20 frame digests distinct, all 20
timestamps distinct. A misrouted or stale slot cannot hide.

### Phase 4A acceptance suite (`tests/test_tenzorpipe_integration.py`)

All six pass:

* Rust ring delivers real tensors to three processes — 20/20 each, 0 mismatches, 60 zero-copy views
* the Python reference ring delivers the same tensors
* verification detects a mis-ordered publication (negative control)
* verification detects a swapped timestamp (negative control)
* backpressure: 3 consumers on a 2-slot ring with random sleeps
* **consumer SIGKILLed mid-stream while holding a lease on a real video tensor** — producer published every epoch, survivors received all of them uncorrupted, the dead consumer was reaped

### Integration soak (`scripts/run_integration_soak.sh`)

A 60 s 720p H.264/AAC clip, 120 epochs of real video, five configurations:

| case | epochs | published | consumers | slots | received | corruption / ts / gaps / reorder / non-zero-copy | zero-copy views | reaped |
|---|---:|---:|---:|---:|---|---|---:|---:|
| fanout8 | 120 | 120 | 8 | 16 | all 120 | 0 / 0 / 0 / 0 / 0 | 960 | 0 |
| saturated_2slot | 120 | 120 | 8 | 2 | all 120 | 0 / 0 / 0 / 0 / 0 | 960 | 0 |
| crash_midstream | 120 | 120 | 6 | 3 | 17 + 5×120 | 0 / 0 / 0 / 0 / 0 | 617 | 1 |
| single_consumer | 120 | 120 | 1 | 4 | 120 | 0 / 0 / 0 / 0 / 0 | 120 | 0 |
| tight_slot_fit | 120 | 120 | 4 | 2 | all 120 | 0 / 0 / 0 / 0 / 0 | 480 | 0 |

Raw: `integration_soak_results.json`.

## Three bugs Phase 4 exposed in TenzorBus

### 1. The Rust ring could not carry a media timestamp at all

`Producer::publish` hard-coded `shm::now_ns()` into the slot's timestamp field. The
v0.1 Python reference accepted `timestamp_ns=`; the Rust transport that replaced it
had silently dropped that, so a consumer could not align a frame with its clip.

Fixed: `TensorView::with_timestamp_ns`, `SlotWriter::set_timestamp_ns`, and
`timestamp_ns=` on the Python `publish`. Regression test:
`media_timestamps_survive_the_transport`.

### 2. A media timestamp of 0 was silently replaced — in the reference

```python
p.write_u64(self.buf, base + p.S_TIMESTAMP_NS, timestamp_ns or time.time_ns())
```

`0 or x` is `x`. **The first epoch of every clip carries timestamp 0**, so the first
frame of every integration got wall-clock time instead. Proven:

```
requested timestamp_ns=0            slot recorded=1789933128787140045  faithful=False
requested timestamp_ns=500000000    slot recorded=500000000            faithful=True
```

Fixed with `is None`, plus two regression tests in `tests/test_ring.py`. Worth
noting: the Rust implementation was already correct here, because `Option` cannot
confuse 0 with absent.

### 3. A zombie consumer pinned its slot forever — release-blocking

This one surfaced as the Phase 4A crash test hanging, and it is the most important
finding of this phase.

Liveness used `kill(pid, 0)`. When the dead consumer is the **producer's own child**
— a supervisor that spawns its workers, an entirely ordinary shape, and exactly what
the demo does — it stays in the process table as a zombie until someone waits on it,
and `kill` reports a zombie as alive. Proven:

```
child pid 4142 after SIGKILL (parent has not waited):
  /proc/<pid>/stat state = 'Z'   (Z = zombie)
  kill(pid, 0) = 0  -> pid_alive() reports ALIVE
  => a zombie consumer pins its TenzorBus slot forever
```

The Phase 2 test did not catch it because `wait_with_output()` reaps the child before
the producer's next sweep.

Fixed: liveness now reads `/proc/<pid>/stat` and treats state `Z`/`X` as dead — a
zombie has been reaped by the kernel and can never execute again, so it can never
release a lease. This is exact, not a heuristic, and it keeps the deliberate
decision not to reap on heartbeat age (so a slow consumer is never falsely reaped).
While there, the registry now records each pid's process start time so a recycled
pid is not mistaken for the original process; the same applies to the producer pid
and the registry-lock owner.

New regression test `a_zombie_consumer_does_not_pin_a_slot` deliberately never reaps
the child, so the zombie state is the thing under test.

## ThreadSanitizer — clean, and it has teeth

`scripts/run_tsan.sh`. Results in `tsan_results.json`.

**What TSan can see here, honestly.** TSan is single-process. It cannot observe
another process touching the same `MAP_SHARED` pages, so it can never cover
TenzorBus's cross-process traffic directly. What it does cover is the
synchronisation itself — the atomic slot state, the reader bitmask, and the
non-atomic payload `memcpy` they order — which is the same code a separate process
runs and is where both Phase 2 bugs lived. `rust/tenzorbus/tests/tsan_threads.rs`
therefore drives the producer and consumers as threads over one mapping, including a
test that churns registration while publishing. Cross-process behaviour stays
covered by the real-process tests.

**A second caveat.** The stable toolchain ships a precompiled, uninstrumented std
(`-Zbuild-std` needs nightly plus `rust-src`, and the download is blocked). So races
*inside* std are invisible, and std's own lock-free channels raise false positives.

The first run reported exactly one warning. I checked every frame rather than
suppressing it on sight: all of them were `library/std` or `library/core`, in
libtest's `std::sync::mpmc` result channel; **zero frames touched tenzorbus or
tenzor-core**. The suppression file is scoped to exactly those three constructs and
names nothing of ours.

**The positive control is what makes "clean" mean something.** A
`tsan-positive-control` cargo feature downgrades the commit release/acquire pair to
`Relaxed`, and the script fails if TSan does not then report a race in our code. It
does, and in precisely the right place:

```
WARNING: ThreadSanitizer: data race
  #0 tsan_threads::race_the_ring::{closure#0}  tests/tsan_threads.rs:85   <- consumer payload read
  #1 core::ptr::copy_nonoverlapping::<u8>
  #2 <tenzorbus::ring::Producer>::publish      src/ring.rs:833            <- producer payload copy
```

| | |
|---|---|
| clean run | 4 tests pass, **0 warnings**, 0 naming tenzorbus |
| positive control | race detected in tenzorbus, first attempt |

Detection of a relaxed-ordering race is timing-dependent, so the control retries up
to `TSAN_CONTROL_ATTEMPTS` (default 6) times and passes on the first detection.
Without that it was a coin flip on two cores — a flaky gate, which is worse than no
gate.

## Phase 4B — TenzorBus side done and proven; the media path needs TenzorPipe

### What is built

`Producer::reserve` returns a `SlotWriter` that holds the slot in `STATE_WRITING`
and hands the caller `&mut [u8]` (or a typed slice) pointing at the payload itself.
`commit()` publishes with the release store; `abort()` — and `Drop` — return it.
Exposed in Python as `producer.reserve(dtype, shape)`, with a writable NumPy array
over the slot.

Ownership semantics are unchanged, and each one is tested
(`rust/tenzorbus/tests/direct_write.rs`, 10 tests):

| property | test |
|---|---|
| producer writes and consumer reads the *same address*, 64-byte aligned | `the_producer_writes_into_the_same_bytes_the_consumer_reads` |
| a reserved slot is invisible to consumers until commit | `a_reserved_slot_is_invisible_until_commit` |
| abort returns the slot **and the sequence number**, so delivered sequences stay contiguous | `abort_returns_the_slot_and_the_sequence_number` |
| a dropped, uncommitted writer leaks nothing (10 iterations, 2 slots) | `dropping_a_writer_without_committing_does_not_leak_the_slot` |
| reserve never takes a slot with an active reader | `direct_write_never_takes_a_slot_with_an_active_reader` |
| `drop_newest` honoured | `direct_write_honours_drop_newest` |
| copy and direct paths interleave indistinguishably on one ring | `copy_path_and_direct_path_interleave_on_one_ring` |
| an unfilled payload is poisoned in debug builds | `an_unfilled_payload_is_poisoned_in_debug_builds` |

Python side: committing while a NumPy view of the slot is still alive raises
`BufferError`, because otherwise the producer could mutate a published slot.

### What is not built, and the honest reason

The media path cannot use it yet. TenzorPipe v0.3.2's entire public API is
`convert(argv)` — file in, file out. It cannot hand a tensor to anything but its own
writer, and it cannot be given a destination buffer.

And a point worth being blunt about: on the current file-based path the tensor
**already exists** in TenzorPipe's memory-mapped Arrow buffer, so copying it into a
slot is unavoidable. Direct-write cannot remove a copy of data that is already
somewhere else. Filling a reserved slot from an array you already hold is still one
copy. The payoff comes only when the producer *generates* into the slot.

The benchmark matrix reports that distinction explicitly rather than blurring it:
`tenzorbus_direct` (clock started before the fill, comparable to every other path)
and `tenzorbus_direct_nofill` (the transport floor for an in-place generator, marked
not comparable).

`docs/TENZORPIPE_DIRECT_WRITE.md` specifies the change, against v0.3.2 line numbers.
The encouraging part: the engine is already 90% shaped for it — `video.rs:432`
allocates the CHW output and returns it, and `video.rs:291` already announces each
finished tensor through an `on_epoch` callback threaded through both the H.264 and
HEVC decoders. Three scoped compatibility steps (split the resize to write into a caller's
buffer; expose an `EpochSink` that can supply the destination; implement it in
TenzorBus) with the `.tenzor` writer, schema, metadata and error texts untouched, so
the 1,056-case identity matrix still covers the default path.

One case will still cost a copy: at `video.rs:288-293` one resized picture can
satisfy several epochs, and each epoch needs its own slot. Direct-write eliminates
the copy for the one-picture-one-epoch case (ordinary at 30 fps / 0.5 s windows) and
costs one copy per repeated epoch.

## Phase 5 — fair benchmark matrix

`benchmarks/benchmark_matrix.py`, raw results in `benchmark_matrix.json`.
**49 cases, 0 failures, 0 cases where a consumer missed data.**

### Methodology

* **Measured:** one interval per publication — producer begins handing the tensor
  over → a consumer process has a usable array over those bytes. Nothing else.
* **Excluded:** media decode (the payload is generated once before timing and
  reused, so TenzorPipe's decoder never runs inside a measurement) and model
  inference (consumers run no model).
* **Identical consumer work in every path:** build a NumPy array over the bytes and
  sum a fixed 4096-element stride. Same in all paths so it cancels out, and it
  forces real reads so a zero-copy path cannot skip touching memory. Deliberately
  not a full-buffer sum, which would add the same large constant everywhere and
  flatter the slower paths.
* **Clock:** `time.monotonic_ns()`, system-wide on Linux. The send timestamp travels
  with the payload — in the slot's own timestamp field for TenzorBus, as an 8-byte
  prefix or header elsewhere.
* **Fan-out:** with N consumers a point-to-point transport delivers N times, because
  that is what it does. TenzorBus publishes once. That asymmetry is the measurement.
* **The JSON/base64 body is built per publication, inside the timed region.** The
  v0.1 reference benchmark prebuilt it, which excluded work a real sender cannot
  avoid.
* **Copies are counted analytically**, not measured, and reported per case.

### Axis 1 — payload size, one consumer, median ms

| path | 64 KiB | 602,112 B (real frame) | 4 MiB | 16 MiB |
|---|---:|---:|---:|---:|
| tenzorbus_direct_nofill \* | 0.0027 | 0.0046 | 0.0140 | 0.0199 |
| tenzorbus_direct | 0.0089 | 0.0452 | 0.4068 | 2.3252 |
| tenzorbus_copy | 0.0262 | 0.0619 | 0.4350 | 2.0330 |
| unix_socket | 0.0574 | 0.1474 | 1.4620 | 6.8429 |
| fifo | 0.0303 | 0.1858 | 2.0711 | 14.4035 |
| http_binary | 0.1014 | 0.2560 | 1.8217 | 6.0452 |
| http_json_b64 | 0.4788 | 4.2319 | 33.8726 | 131.0094 |

\* not comparable to the rows below it — see the note above.

### Axis 2 — fan-out at the real 602,112-byte frame

median ms / producer publications per second / total copies

| path | 1 | 2 | 4 | 8 |
|---|---|---|---|---|
| tenzorbus_direct_nofill \* | 0.0046 / 9352 / 0 | 0.0165 / 6463 / 0 | 0.0563 / 4975 / 0 | 0.1071 / 3978 / 0 |
| tenzorbus_direct | 0.0452 / 9028 / 1 | 0.0561 / 7077 / 1 | 0.1043 / 4598 / 1 | 0.1697 / 3523 / 1 |
| tenzorbus_copy | 0.0619 / 9518 / 1 | 0.0895 / 6034 / 1 | 0.1547 / 4964 / 1 | 0.2316 / 3934 / 1 |
| unix_socket | 0.1474 / 8696 / 2 | 0.1844 / 2304 / 3 | 0.1875 / 1372 / 5 | 0.1952 / 634 / 9 |
| fifo | 0.1858 / 5888 / 2 | 0.3925 / 1229 / 3 | 0.3750 / 651 / 5 | 0.3899 / 297 / 9 |
| http_binary | 0.2560 / 2417 / 3 | 0.1767 / 1664 / 5 | 0.1901 / 742 / 9 | 0.1988 / 350 / 17 |
| http_json_b64 | 4.2319 / 122 / 6 | 3.3167 / 71 / 9 | 3.3283 / 35 / 15 | 3.4180 / 17 / 27 |

### What the matrix actually shows

* **The fan-out asymmetry is the story, not per-message latency.** From 1 to 8
  consumers the producer's rate falls 13.7× for a Unix socket (8696 → 634 pub/s) and
  19.8× for a FIFO, because the producer sends the payload N times. TenzorBus falls
  2.4× (9518 → 3934), and on a 2-core box much of that is contention, not work.
  Delivered throughput at 8 consumers: 18,070 MiB/s for `tenzorbus_copy` against
  2,915 for a Unix socket and 79 for HTTP+JSON.
* **Point-to-point per-message latency stays flat with fan-out** (each connection is
  independent) while the producer serialises — which is why the socket's median
  barely moves while its throughput collapses. Reading only the latency column
  would miss the entire effect.
* **TenzorBus per-consumer latency rises with fan-out** (0.0046 → 0.107 ms at
  `direct_nofill`), because one slot must be read by 8 processes on 2 cores and the
  producer waits for the last reader. That is real and not hidden here.
* **HTTP request/response serialises the producer**, so `http_binary` shows low
  per-message latency with low throughput. Different shape, same conclusion.
* **JSON+base64 is 68× the real-frame latency of `tenzorbus_copy`** and 27 copies at
  8 consumers versus 1. Measured with the encode inside the timed region, as a real
  sender would pay.
* **`direct_nofill` is nearly flat in payload size** (0.0027 → 0.0199 ms from 64 KiB
  to 16 MiB) because nothing is copied at all. That flatness is the mechanism
  working, and it is the ceiling Phase 4B is chasing — but only reachable by a
  producer that generates in place.

These are this box's numbers, on 2 vCPU, and none of them should be published as
product claims without re-running on the target hardware.

## Test inventory

**Rust — 43 tests, all passing** (`cd rust && cargo test --release`):
tenzor-core 5, layout parity 3, tenzorbus lib 2, golden bytes 2,
production_ring 18, direct_write 9 (10 in debug), tsan_threads 4.
`cargo clippy --all-targets -- -D warnings` clean; `cargo fmt --check` clean.

**Python — 28 tests, 0 skips** (`scripts/run_tests.sh` with `TENZORPIPE_BIN` set):
reference ring 6, integration contract 2, Rust bindings 13, TenzorPipe integration
6, real TenzorPipe release determinism 1.

**Soaks and gates:**
`scripts/run_rust_stress.sh` (5 cases incl. 10M publications),
`scripts/run_integration_soak.sh` (5 cases, real 720p media),
`scripts/run_tsan.sh` (clean + positive control),
`benchmarks/benchmark_matrix.py` (49 cases).

## Remaining known issues

**Release-blocking**

1. **Pinned-toolchain build unverified.** Everything was built with Rust 1.95.0;
   TenzorPipe pins 1.98.1 and this environment cannot fetch it. Build both on
   1.98.1 before release.
2. **Phase 4B is not wired to the media path.** The TenzorBus half is proven; the
   TenzorPipe side needs the scoped compatibility API in `docs/TENZORPIPE_DIRECT_WRITE.md`.
   Until then, the media path pays one producer copy.

**Should be closed before release**

3. **TSan cannot see cross-process races at all**, and std is uninstrumented. A
   cross-process race remains possible in principle and is only covered by the
   real-process tests. A run against an instrumented std on nightly would tighten
   the single-process half.
4. **No ASan/UBSan/Miri run.** Miri cannot execute `shm_open`/`mmap`, so it is not
   applicable to the ring; ASan on the multi-process harness is worth doing.
5. **Windows and macOS untouched.** The futex and `shm_open` paths are Linux-only by
   construction. No stable C ABI yet.
6. **64-consumer ceiling** is structural (the reader bitmask is a u64). Fine for
   now; document it as a limit rather than a bug.
7. **Pid reuse is mitigated, not eliminated.** The start-time token would have to
   collide on both pid and start tick to fool the sweep. Narrow, but not zero.
8. **Benchmarks are from a 2-vCPU box.** Re-run on the AMD EPYC host before any
   number leaves the project.
9. **The `.tenzor` file path is inherently one copy.** Worth deciding whether the
   live path (TenzorPipe → ring, no file) is the actual product shape; if so, Phase
   4B matters much more than the file path does.

**Cosmetic / noted**

10. The Python reference ring emits `resource_tracker` "leaked shared_memory"
    warnings when consumers attach in other processes. Pre-existing, harmless, and
    absent from the Rust ring.
11. `benchmark_results.json` / `benchmark_cross_process.json` are still the *v0.1
    Python reference's* numbers from the original sandbox. `benchmark_matrix.json`
    supersedes them for anything about the Rust transport.
12. Live Lab is untouched, as instructed.

## Reproducing all of it

```bash
# TenzorPipe v0.3.2
git clone https://github.com/jay-showforge/tenzorpipe && cd tenzorpipe
git checkout v0.3.2
git apply /path/to/patches/tenzorpipe-v0.3.2-test-fixes.patch
sudo apt-get install -y nasm ffmpeg
cargo build --release --locked            # 1.98.1 per rust-toolchain.toml
pip install .
pytest tests/

# TenzorBus
scripts/build_rust.sh
cd rust && cargo test --release && cd ..
TENZORPIPE_BIN=/path/to/tenzorpipe/target/release/tenzor scripts/run_tests.sh
scripts/run_integration_soak.sh          # real 720p media, ~4 min
scripts/run_rust_stress.sh               # ~5 min, includes 10M publications
scripts/run_tsan.sh                      # clean run + positive control
PYTHONPATH=src benchmarks/benchmark_matrix.py run --output benchmark_matrix.json
```
