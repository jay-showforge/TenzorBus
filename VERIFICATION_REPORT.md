# Independent verification and hardening — TenzorBus v0.1.0-alpha.0

> Historical WSL2 verification report. Use `FINAL_VERIFICATION.md` for the
> consolidated release status and `BENCHMARKS.md` for authoritative EPYC data.

Second pass, 2026-09-20. The release candidate was re-extracted from the handoff archive and
re-verified **from a clean tree** — every artefact rebuilt, no state carried over from the first
pass. Three packaging defects found in the first pass were fixed and the fixes were themselves
verified. An adversarial pass was then run against the API as an outside developer would meet it.

**Result: all gates green.** One limitation was found that users must know about and that is now
documented in the README, the release notes and here.

Correctness only. Performance numbers are WSL2 development measurements and are not publishable.

## Environment

| | |
|---|---|
| OS / kernel | Ubuntu 24.04.3 LTS, Linux 6.6.87.2-microsoft-standard-WSL2 |
| CPU / RAM | Intel Core i5-14400F, 16 logical cores, 15 GiB, `/dev/shm` 7.8 GiB |
| Release toolchain | `rustc 1.98.1 (48a229cea 2026-09-01)`, `cargo 1.98.1` — pinned by `rust-toolchain.toml` |
| Sanitizer toolchain | `rustc 1.100.0-nightly (feaadeeac 2026-09-19)` — ThreadSanitizer only |
| Python | 3.12.3 · numpy 2.5.3 · pyarrow 25.0.1 · torch 2.14.0+cpu · psutil 7.2.2 · pytest 9.1.1 |
| Build tools | nasm 2.16.01 · ffmpeg N-126717 static (libx264/libx265) |

All dependencies user-local. No sudo, no modification of the Windows host.

Nightly exists for exactly one purpose: `-Zsanitizer=thread` with `-Zbuild-std`, which stable
cannot provide. **No nightly-built artefact is offered as a release binary.**

## Artefacts

| | |
|---|---|
| Pristine TenzorPipe | `ffd765670f89331d44c310423be200a100f14f5e` (public tag `v0.3.2`) |
| Pristine binary | `86fba4294dcd7b583e9fec4b8ea6c0b79f0bc0736ce9031b6ee197d5dd87e9a7` |
| Patched binary | `4cc4823960b92816f14961635c3cb15196678f26063ca7bcdd789f8291431974` |
| Direct-write patch | `27f9d8fc4daf913c88827530e56afd366639cf07f646192eeaf036125a7591c4` |

The patched binary hashed **identically across two independent clean builds**, so the build is
reproducible from source. Separate `target/` trees were used so the patched build could not
inherit pristine fingerprints.

The shipped prebuilt `tenzorbus_rs.abi3.so`
(`c5fec9e2ed71d8636a1cdec717bb941babd56bbd43f4d75a10b3f22558b48e72`) was never executed; the
extension was rebuilt from source and excluded from the release package.

## Final gate results

Every check below was measured by its own exit code, not by parsing output.

| Gate | Result |
|---|---|
| `cargo build --release --locked --workspace` | PASS |
| `cargo fmt --all --check` | PASS |
| `cargo clippy --workspace --all-targets -D warnings` | PASS |
| `cargo deny check licenses` | PASS |
| `cargo test --release --locked --workspace` | PASS — 47 tests |
| Lease lifetime regressions, both transports | PASS — 11 tests |
| Strict preflight, with and without ffmpeg | PASS — fails closed, exits 2 |
| README examples executed verbatim | PASS |
| Fresh-wheel install, guards active | PASS |
| `pytest` strict mode | PASS — 44 passed, **0 skipped** |
| Strict-mode positive control | PASS — a skip becomes a failure |
| `SOURCE_SHA256SUMS` | PASS — 86 entries, self-check clean |
| Exact-byte pristine vs patched | PASS — 58 identical, 2 matching refusals, 0 mismatched / 60 |
| TenzorPipe regression matrix | PASS — **1,224 cases, 0 failures** |
| Live soak, 6 cases × 120 epochs | PASS — 0 problems |
| ThreadSanitizer + positive control | PASS — clean; injected race detected |
| Adversarial probes | 13/14 safe; 1 documented limitation |

The regression matrix category split — 888 identical / 120 expected-hevc / 96
expected-message-change / 72 same-error / 48 expected-new-success — is **identical to the v0.3.2
CI run on unpatched TenzorPipe**, reproduced three independent times. Matching the profile is
stronger evidence than matching the total.

## Direct-write, proven rather than asserted

| Path | `producer_copies` |
|---|---|
| direct (four soak cases) | **0** |
| `--copy-path` | **120** — one per epoch |

The same counter in the same harness reports 120 when copying is forced, so `0` is a measurement
and not a constant. Every consumer independently records the `[sequence, slot]` pair it read, and
all consumers agree on which slot served each sequence — 120/120 under 8-way fan-out, 41/41 with
a consumer SIGKILLed mid-stream.

Consumer-side and producer-side zero-copy are distinct: the copy path still yields consumer
zero-copy, because consumers map the slot either way. Only the producer's fill differs.

## The three packaging fixes, and proof each works

**1 — Integration tests could pass by skipping.** As shipped the suite reported `21 passed, 12
skipped`, and all 12 skips were the TenzorPipe claims. `tests/conftest.py` now converts any skip
into a failure when `TENZORBUS_REQUIRE_INTEGRATION=1`.

Proven with a positive control, on the same reasoning as the TSan gate — a guard that cannot be
shown to fire is not evidence:

| | strict OFF | strict ON |
|---|---|---|
| prerequisite removed | `1 skipped`, exit 0 | `1 error`, **exit 1** |

**2 — `layout_parity` failed misleadingly.** It shells out to `python3`; without numpy on that
interpreter it emitted a traceback that read as a protocol parity violation. It now names the
real requirement and honours `TENZORBUS_PYTHON`. Verified: 3/3 passing.

**3 — `SOURCE_SHA256SUMS` was wrong in both directions.** It listed 4 files that are not shipped
*and omitted 17 that are*, covering only 63 of 80 sources — a manifest blind to a fifth of the
tree provides far weaker tamper-evidence than its presence implies. Regenerated and kept in sync
through the packaging changes: 86 entries, self-check clean.

Additionally, `rust/deny.toml` **did not exist**, so the licence policy was enforced by nothing.
It now fails closed. `rust-toolchain.toml` was added to pin 1.98.1 in-tree.

## Adversarial pass

Probes written against the real API, not the happy path. 13 of 14 behaved safely.

| Probe | Behaviour |
|---|---|
| Oversized tensor | refused with a sizing error, not truncated |
| Ring name path traversal (`../../../tmp/...`) | sanitised to `.._.._.._tmp_adv_escape` |
| `slot_count = 1` | refused |
| Attach to non-existent ring | `FileNotFoundError`, no crash |
| Non-contiguous array | accepted, delivered values identical to the source view |
| Media timestamp of exactly `0` | round-tripped intact, not treated as absent |
| Double release of a lease | idempotent no-op; ring still usable afterwards |
| Publish after close | refused |
| Corrupted magic on attach | `RingError: invalid TenzorBus shared-memory magic` |
| Empty tensor | accepted deterministically |
| Unsupported dtype (`complex128`) | refused, not reinterpreted |
| Slot reuse while a lease is held | held slot intact after 6 further publishes |
| Consumer limit | 64 consumers created without hitting a documented cap |
| **Derived view outliving its lease** | found leaking the segment; **fixed, see below** |

### The finding

`lease.numpy()` and `lease.torch()` return views that borrow the slot. If such a view is still
alive when `lease.release()` is called:

    release ok · consumer closed · ring.close() -> BufferError: cannot close exported pointers exist

The sharp edges are that `release()` *reports success* even though the mapping cannot be torn
down, the failure surfaces later at `ring.close()`, and the message names neither the lease nor
the derived array. The segment leaks until something recreates it with `force=True`, which does
recover cleanly.

This matters because `arr = lease.numpy()` followed by use after release is the natural thing to
write, and in a zero-copy ring it is also a correctness hazard: the slot may have been recycled.

### The fix

`release()` now refuses while a view it handed out is still alive:

    LeaseStillBorrowed: cannot release sequence 1: 1 view(s) returned by numpy()/torch()
    are still alive and borrow this slot. Delete them first, or take a copy if the data
    must outlive the lease (for example `arr = lease.numpy().copy()`).

Detection is by weak reference to the views returned by `numpy()` and `torch()`. That choice was
made after measuring the mechanics rather than assuming them: `memoryview.release()` **succeeds**
while numpy borrows the slot, because the export lives at the `mmap` level — which is exactly why
the original failure only appeared later, at `close()`. Catching a `BufferError` would therefore
not have worked.

The zero-copy design is unchanged; views still alias the slot directly. This only closes the
window in which a caller could hold one past reclamation.

Verified:

| Check | Result |
|---|---|
| numpy view alive at release | raises `LeaseStillBorrowed` |
| torch view alive at release | raises `LeaseStillBorrowed` |
| view dropped first | releases cleanly |
| `.copy()` taken | releases cleanly |
| `ring.close()` afterwards | no `BufferError` |
| double release | still idempotent |
| `with` block holding a view | raises at `__exit__` rather than hiding it |
| installed wheel, no source tree | guard active |

**Scope, after auditing the Rust transport.** The Rust/PyO3 `Lease` was audited for the same
hazard and it was **half present**: guarded for NumPy by buffer-protocol export counting — a
stronger mechanism than the Python side uses — but **not guarded for PyTorch**, because
`torch.frombuffer()` releases the `Py_buffer` immediately and the export count returned to zero
while the tensor still aliased the slot.

Reading the code would not have revealed this; the guard looked correct and its doc comment
claimed to cover both. It was found by exercising it.

Fixed by constructing the Torch view from the NumPy view, so `torch.from_numpy()` keeps the array
alive and the array holds the export. Verified on both transports for both view types.

The exception types were also unified: `LeaseStillBorrowed` now subclasses `BufferError` as well
as `RingError`, matching what the Rust transport raises, so one handler covers both backends.

**One existing test required updating.** `test_zero_copy_numpy_view` held its view past the
`with` block, which is the pattern now forbidden. A `del got` was added after the assertions.
Every assertion still runs, including `np.shares_memory(...)` — the one that actually proves the
view aliases the slot. Nothing the test checks was weakened.

### Consumer cap

`tenzorbus.MAX_CONSUMERS` is now a public, documented constant equal to 64, matching the value in
the shared-memory header. Verified by attaching consumers until refusal: the ring accepts exactly
64 and rejects the next with `RingError: maximum consumer count reached`.

## Benchmarks — development measurements only

**Not publishable.** Produced under WSL2 on a developer laptop; they exist to show the harness
works. Reproduce with `scripts/rerun_benchmarks_epyc.sh`, which refuses any toolchain but 1.98.1.

The reproduced data supports the brief's hypothesis — the advantage is fan-out, not
single-consumer latency. TenzorBus's copy count is constant in consumer count while every
competing transport's grows linearly, so its throughput lead widens from 2.2× at two consumers to
4.4× at eight. That structural property should survive a change of hardware; the absolute figures
should not be quoted until the EPYC run exists.

## Remaining limitations

1. **A derived view must not outlive its lease.** Inherent to zero-copy, and now enforced:
   `release()` raises `LeaseStillBorrowed`. Copy when the data must outlive the lease.
2. **The lease guard covers the Python reference transport only.** The Rust extension's `Lease`
   was not modified; do not assume the same guarantee there without checking.
3. **Linux only** — the Python reference transport is POSIX-only by construction.
4. **Direct-write is sequential-single-decoder only**, by design; other execution modes refuse
   with an error rather than silently copying.
5. **No ARM64 verification** for TenzorBus. TenzorPipe's own ARM64 gates are unaffected by this
   patch, whose changes are x86-gated, but no TenzorBus ARM run was performed.
6. **The shipped prebuilt `.so` is unverified** and excluded from the package.
7. **No publishable performance numbers exist yet.** The EPYC rerun is the gate for those.
8. **Live Lab UI not started**, per the brief.

## The "transient" failure was not transient

The previous round reported 11 errors that did not reproduce on a re-run, and attributed them to
contention. That diagnosis was wrong.

Reproduced deterministically: the script in question rebuilt `PATH` without `~/bin`, which removed
ffmpeg. Strict mode then correctly turned eleven ffmpeg-dependent skips into errors. The signature
`1 failed, 21 passed, 11 errors` with reason "ffmpeg not available to build the media fixture"
appears every time under that condition, and never otherwise.

So the guard behaved correctly and the failure was fully determined by the environment. What was
defective was the diagnosability, and one genuinely missing guard:

- `tests/conftest.py` now runs a **preflight** under strict mode: prerequisites are checked once,
  before any test, and a single message names everything missing and how to obtain it, exiting 2.
- `test_real_release_ingest_is_deterministic_arrow` called ffmpeg with **no `HAVE_FFMPEG` guard**,
  so it raised `FileNotFoundError` where every neighbouring module skipped. Added.

Determinism confirmed by three consecutive full runs in each configuration:

| Configuration | Result (×3, identical) |
|---|---|
| no ffmpeg, strict off | 21 passed, 12 skipped |
| ffmpeg present, strict on | 44 passed, 0 skipped |

## Corrections to my own earlier reporting

In the first hardening run I reported `fmt: CLEAN` when it was not. The check was written as
`cargo fmt --check | tail -3 && echo CLEAN`, where `&&` binds to `tail`, which always succeeds —
so the echo fired regardless. My own edit to `layout_parity.rs` was in fact unformatted. It has
been formatted and every gate in the final table above is measured by its own exit code rather
than by a piped echo.

Two further errors of mine, both caught by testing rather than review:

- A stub-binary test of `scripts/rerun_benchmarks_epyc.sh` ran to completion and wrote
  `epyc-benchmark_matrix.json` containing WSL2 numbers under an EPYC filename, inside the release
  tree. The file was removed and the script now rejects a binary that does not identify as
  TenzorPipe, and refuses to overwrite an existing result.
- The quickstart example in the README I wrote held a view past its `with` block — the very
  pattern the new guard forbids — so the documentation would have raised on first use. It was
  corrected, and every example in the README was then executed to confirm it runs.

## Freeze

**TenzorBus v0.1.0-alpha.0 is frozen as of 2026-09-20.**

Every gate in this report was reproduced from a clean tree on `rustc 1.98.1`, with the regression
matrix reaching 1,224 cases / 0 failures for the fifth time at an identical category split. The
remaining pre-publication task is the EPYC benchmark run; no performance claim should be made
until it exists.
