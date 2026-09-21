# Final release verification

Release: `v0.1.0-alpha.0`

## Authoritative Linux verification

The completed Linux release pass recorded:

- host: AMD EPYC 9V74, 9-vCPU Linux KVM
- release gates: 18 passed, 0 failed, 0 skipped
- Rust tests: 47 passed
- strict Python integration: 48 passed after adding four benchmark-harness
  regressions, 0 skipped
- pristine-versus-patched TenzorPipe v0.3.2: 58 byte-identical outputs, two
  matching refusals, zero mismatches
- direct path: zero producer copies
- copy-path control: 120 producer copies
- stress/soaks: zero corruption, gaps or ordering problems
- benchmark: 49/49 complete cases, zero errors

The authoritative benchmark evidence and its methodology are preserved in
`BENCHMARKS.md` and `evidence/epyc-benchmark/`.

## Packaging-day revalidation

The assembled repository was independently rechecked on 2026-09-21 under
Ubuntu/WSL2 with the pinned Rust 1.98.1 toolchain. All core tests, integration
tests, strict linting, formatting, license policy, ThreadSanitizer, TenzorPipe
byte identity, and the hardened 49-case benchmark harness passed. The local
timings are not substituted for the authoritative EPYC measurements. See
`LOCAL_REVALIDATION.md` and `evidence/local-revalidation/`.

## Benchmark harness hardening

The repository version of `benchmarks/benchmark_matrix.py` now:

- exits nonzero immediately on a case failure;
- treats a nonzero child status, timeout, missing/invalid JSON, incomplete
  delivery or invalid metric as a failure;
- requires exactly seven complete cases per selected transport and 49 for the
  full matrix;
- records whether Unix tests used pathname sockets or inherited real
  `AF_UNIX/SOCK_STREAM` socketpairs;
- verifies all seven Unix rows used the recorded transport;
- writes partial results with `benchmark_status: failed`, preventing an errored
  matrix from being presented as successful.

Four targeted regression tests cover the seccomp/socketpair fallback, complete
seven-case validation, incomplete-delivery rejection and unidentified Unix
transport rejection.

## Packaging verification

The final handoff regenerates the Python wheel, source distribution, standalone
source ZIP and `SHA256SUMS` from the repository contents. Package checks and the
local clean-tree result are recorded in `LOCAL_REVALIDATION.md`.

## Publication boundary

No GitHub repository was created. Nothing was pushed, tagged, released or
published, and Tenzor Live Lab was not started. The remaining external action is
the user's explicit `publish` command.
