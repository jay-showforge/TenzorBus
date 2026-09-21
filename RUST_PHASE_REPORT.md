# TenzorBus — Phases 1–3 executed report

> Historical phase report. The later phases and Rust 1.98.1 parity gate are now
> complete; use `FINAL_VERIFICATION.md` for current status.

Date: 2026-09-20
Scope: `HANDOFF_TO_WORK.md` Phase 1 (compile and freeze protocol), Phase 2
(Linux production ring + stress), Phase 3 (Python bindings).
Phases 4–7 are **not** done; see "What is still open".

Everything below was executed on the host named in "Environment". No number in
this document is estimated, extrapolated or carried over from the earlier
sandbox report.

## Environment

| | |
|---|---|
| host | Linux 6.18.44-fc-v37 x86_64, 2 vCPU |
| rustc | 1.95.0 (59807616e 2026-04-14) |
| Python | 3.11.15 |
| numpy / pyarrow / torch | 2.4.4 / 25.0.1 / 2.14.0+cu130 (CPU use only) |
| TenzorPipe binary | **not available in this package** — see below |

The handoff asks for Rust 1.98.1+ for toolchain parity with the TenzorPipe
release. 1.95.0 is what this host has. Nothing in the crates needs a newer
compiler (edition 2024, `offset_of!`, `div_ceil` are all well below that floor),
but **the parity build on 1.98.1 has not been done and remains a release-gate
item.**

## Baseline first: the shipped tests, unchanged

Run before a line of Rust was touched, exactly as shipped:

```
scripts/run_tests.sh   → Ran 7 tests, OK (skipped=2)
```

The two skips were `torch not installed` and `TENZORPIPE_BIN not set`. After
installing `pyarrow` and CPU PyTorch on this host, the previously-skipped
`pyarrow` gate and the PyTorch zero-copy test both **pass**. The shipped
benchmarks also reproduce (`benchmark_transport.py`, `benchmark_cross_process.py`).

No shipped test was modified, relaxed or replaced.

## Phase 1 — compile and freeze protocol: **done**

* `rust/tenzor-core` now carries the full offset table, dtype table, slot and
  registry structs, and layout arithmetic, with `const` assertions tying the
  `#[repr(C)]` field offsets to the named constants at compile time.
* `rust/tenzor-core/tests/layout_parity.rs` shells out to the **actual**
  `src/tenzorbus/protocol.py` and compares every constant, every dtype code,
  both magics and the layout arithmetic. Neither side keeps a hand-copied table
  of the other's, so they cannot drift silently.
* `rust/tenzorbus/tests/golden_bytes.rs` does it in both directions with real
  producers: Rust publishes a tensor and the Python reference parser reads every
  field and the payload back out of the image; the Python reference publishes a
  tensor and Rust parses it with the `tenzor-core` offsets. It also asserts that
  every byte the Rust transport claimed is still zero in a reference-written
  image.

### Protocol extensions, and why they are safe

The Rust transport needs coordination state the reference never had. All of it
lives in bytes the reference zero-fills and never reads:

* global header offsets 56–4096 → futex words, registry lock and owner pid,
  producer pid, registry epoch, reaped count, and a 64-entry consumer
  registration table (32 bytes each);
* slot header offsets 112–120 → a 64-bit reader bitmask, one bit per registry
  entry.

`readers_remaining` is kept as the bitmask's popcount, so the reference's own
reading of that field stays correct. `docs/PROTOCOL.md` now documents the full
table. **The layout is frozen as of this gate.**

## Phase 2 — Linux production ring: **done**

`rust/tenzorbus` is a working transport, not a scaffold:

* named `shm_open`/`mmap` mappings, using the same object names as the Python
  reference (`/tzbus_<name>`);
* atomic slot-state transitions with release/acquire publication;
* consumer registration table with pid, generation, heartbeat and last sequence;
* crash recovery — a consumer killed holding a lease has its bit cleared from
  every slot and its registry entry freed; a producer killed between reserve and
  commit leaves a `WRITING` slot that the next sweep returns to `FREE`; a
  producer killed while holding the registry lock has the lock stolen by a
  waiter once its pid is gone;
* futex wait/wake on both directions, so neither side polls;
* `block` and `drop_newest` policies;
* single-producer enforcement across processes, with the role released on clean
  exit and reclaimable after a crash.

### Two real bugs this phase found

Both were found by stress, not by review, and both are now covered by
regression tests.

**1. Torn payloads — roughly 1 in 2,000,000 publications.**
The first 10M-publication soak reported 6 corrupt tensors. Cause: a consumer's
reads of `state`, `sequence` and `readers_mask` are not a consistent snapshot. A
stale `COMMITTED` can pair with the *next* publication's sequence while the
producer is still copying, so the consumer reads a half-written slot. Fixed by
re-reading `state` and `sequence` after the header and discarding the snapshot
if either moved — the successful re-read is also what orders the payload reads
against the producer's copy. A related ordering defect was found alongside it: a
single slot-scan pass could see a commit that landed mid-scan while missing an
older one, delivering out of order; consumers now rescan until two passes agree.

*This is the reason the handoff's "verify no stale or torn payload is ever
observed" is a real requirement and not a formality. A 50,000-publication test
never saw it.*

**2. A publication could be delivered twice to the same consumer.**
The reader bit stays set until release, so a scan driven by the bit alone handed
the same slot back while the first lease was still outstanding; the second
release then reported a double-release on a slot that had never been
re-published. The reference's `last_sequence` filter is what makes delivery
once-only, and restoring it fixed this.

### Test suite

`cargo test --release` — **28 tests, all passing**, including:

| | |
|---|---|
| backpressure never overwrites an active reader | ✅ |
| every registered consumer sees the same publication | ✅ |
| consumer view aliases the slab at the slot's payload offset, 64-byte aligned | ✅ |
| a publication is never delivered twice to one consumer | ✅ |
| `drop_newest` returns `None` and counts the drop | ✅ |
| sequence counter wraps through `u64::MAX` → `0` without loss or reorder | ✅ |
| 400 randomised shapes / ranks / dtypes round-trip byte-exact | ✅ |
| oversized tensor rejected, not truncated | ✅ |
| consumer registry bounded at 64 and entries reusable | ✅ |
| **`SIGKILL` a consumer holding a lease** → slot is not pinned, ring keeps running, reap counted | ✅ |
| **`SIGKILL` a producer between reserve and commit** → slot recovered | ✅ |
| a second live producer *process* is refused; the role is reclaimable after it dies | ✅ |
| 8 consumer processes × 2,000 publications, random sleeps — no loss, no gaps, no corruption | ✅ |
| permanently-full ring under `block` makes progress | ✅ |
| saturated `drop_newest` never corrupts a delivered tensor | ✅ |
| 50,000-publication stream byte-exact | ✅ |

Every fault involving a dying process is driven through real OS processes
(`tenzorbus-lab`) and a real `SIGKILL`. Nothing simulates a crash by calling a
cleanup function.

### Stress soaks (`scripts/run_rust_stress.sh`)

Measured on the host above, release build. `bad` counts publications where any
element mismatched its deterministic pattern.

| scenario | published | dropped | rate | producer RSS | producer CPU | bad | gaps | lease violations |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 producer / 1 consumer, 10M × 256 B | 10,000,000 | 0 | 337,603/s | 2,256 KiB | 28.3 s | 0 | 0 | 0 |
| 1 producer / 8 consumers, 250k × 4 KiB, random sleeps | 250,000 | 0 | 11,063/s | 3,276 KiB | 3.3 s | 0 | 0 | 0 |
| full ring, `block`, 2 slots, slow consumer, 180 s | 756,568 | 0 | 4,203/s | 2,520 KiB | 7.1 s | 0 | 0 | 0 |
| full ring, `drop_newest`, 2 slots, slow consumer, 60 s | 261,963 | 62,432,492 | 4,366/s | 2,520 KiB | 60.0 s | 0 | 0 | 0 |
| randomised shapes, 200k, 4 consumers | 200,000 | 0 | 65,174/s | 3,316 KiB | 1.8 s | 0 | 0 | 0 |

Two things worth reading carefully rather than quoting:

* These rates are **not** the transport's headline throughput. They are
  whole-loop rates on a 2-vCPU box where the producer, every consumer and the
  verification all compete for the same two cores. The 8-consumer and saturated
  rows are consumer-paced by design. Phase 5 is where fair numbers come from.
* `drop_newest` burned a full CPU second per second because the harness retries
  in a tight loop with no pacing. That is the harness, not the ring.

One protocol property surfaced here and is now documented: **a dropped
publication does not consume a sequence number**, so a consumer cannot infer
drops from sequence gaps. The Live Lab must read `dropped` from ring stats.

## Phase 3 — Python bindings: **done**

`rust/tenzorbus-py` is a PyO3 extension module (`tenzorbus_rs`, abi3-py311)
matching the API the handoff sketched:

```python
import tenzorbus_rs as tenzorbus

bus      = tenzorbus.create("frames", slots=16, slot_bytes=1 << 20)
producer = bus.producer()
consumer = tenzorbus.attach("frames").consumer()

with consumer.next() as lease:
    t = lease.torch()      # direct view, no consumer copy
```

* `lease.numpy()` and `lease.torch()` are verified zero-copy: the tests assert
  `view.__array_interface__["data"][0] == lease.data_address` and
  `tensor.data_ptr() == lease.data_address`.
* **Lifetime is enforced, not just documented.** A view holds a buffer export;
  `release()` raises `BufferError` while one is outstanding. The v0.1 lifetime
  bug — a NumPy view outliving its lease and then aliasing a recycled slot —
  cannot be reproduced through these bindings. `lease.copy()` is the escape
  hatch.
* `consumer.next()` releases the GIL while waiting.

`tests/test_rust_bindings.py` mirrors the reference's `test_ring.py` invariants
against the Rust ring, adds the lifetime tests, a 3-process fan-out via
`multiprocessing`, a dtype sweep over all nine supported dtypes, and a
**cross-language** test where a Rust process publishes and Python consumes in
place.

Full Python suite, reference and Rust bindings together:

```
scripts/run_tests.sh   → Ran 16 tests, OK (skipped=1)
```

The one skip is `TENZORPIPE_BIN`, below.

## What is still open

**Phase 4 — TenzorPipe integration is not started, and it is the next gate.**
The handoff is explicit that v0.3.2 is the integration target and that reading a
`.tenzor` file does not count as integration. This package contains no
TenzorPipe source or binary, so nothing could be run against it here:
`test_tenzorpipe_release.py` skips for want of `TENZORPIPE_BIN`, and
`demos/tenzorpipe_to_bus.py` has not been exercised end to end. Point both at a
v0.3.2 build and the MP4 → TenzorPipe → TenzorBus → three-consumer gate can be
closed.

Also open:

* **Rust 1.98.1 parity build** — built and tested on 1.95.0 only.
* **Phase 5 fair benchmarks** — the matrix (Unix socket / HTTP binary / HTTP
  JSON / producer-copy / direct-write × 1,2,4,8 consumers × 64 KiB…16 MiB,
  median/p95/p99, CPU, RSS, throughput, copies per path) has not been run. The
  numbers in this report are stress-soak rates, not that matrix. The shipped
  `benchmark_results.json` / `benchmark_cross_process.json` remain the *Python
  reference's* numbers and should not be published as the Rust transport's.
* **Phase 4 stage 2 direct writer** — the remaining producer copy is still there.
* **Phase 6 cross-platform** — Linux only. The futex and `shm_open` paths are
  Linux-specific by construction; Windows named mappings + `WaitOnAddress` and
  macOS POSIX shm + native waits are untouched. No stable C ABI yet.
* **Phase 7 Live Lab** — still static; not wired to the transport.
* **CI** — no workflow file; the release gate asks for a clean-clone reproduction.
* **Sanitizers** — no ASan/TSan/Miri run yet. Given that stress found two real
  ordering bugs, a TSan pass on the multi-process harness is worth doing before
  the release gate.

## How to run any of this

```bash
scripts/build_rust.sh        # builds the workspace, installs tenzorbus_rs into src/
cd rust && cargo test --release
scripts/run_tests.sh         # Python reference + Rust bindings
scripts/run_rust_stress.sh   # ~5 minutes of soaks → stress_rust_results.json
scripts/run_benchmarks.sh    # the Python reference's own benchmarks

# hand-driven stress
rust/target/release/tenzorbus-lab create  --ring frames --slots 16 --capacity 1048576
rust/target/release/tenzorbus-lab consume --ring frames --expect 1000 --sleep-max-us 200 &
rust/target/release/tenzorbus-lab produce --ring frames --count 1000 --elements 1024
```

A longer soak: `TENZORBUS_STRESS_PUBLICATIONS=10000000 cargo test --release`.
