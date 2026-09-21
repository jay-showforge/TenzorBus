# Local release revalidation

Date: 2026-09-21

This pass rechecked the assembled repository immediately before packaging. It
was run under Ubuntu on WSL2 with an Intel Core i5-14400F, Python 3.12.3, and the
pinned Rust 1.98.1 toolchain. It is a correctness and packaging check, not the
source of the published performance figures.

## Results

- TenzorBus Rust: 47 passed, 0 failed.
- TenzorBus Python/integration: 48 passed, 0 failed, 0 skipped.
- Rust formatting, strict Clippy, and cargo-deny policy: passed.
- ThreadSanitizer: clean run had zero warnings; the injected-race positive
  control was detected on its first attempt.
- Hardened benchmark harness: 49/49 cases completed with zero errors.
- TenzorPipe v0.3.2 patched integration: 43 Rust tests passed; strict Clippy
  and formatting passed.
- TenzorPipe Python: 10 passed; one explicit `--run-matrix` evidence test was
  skipped because the recorded evidence is checked separately.
- Pristine-versus-patched TenzorPipe: 58 byte-identical outputs, two matching
  refusals, zero mismatches across 60 cases.

The local benchmark used pathname `AF_UNIX`/`SOCK_STREAM` sockets. This differs
from the authoritative EPYC environment, whose seccomp policy blocked pathname
Unix sockets and therefore used inherited real `AF_UNIX`/`SOCK_STREAM`
socketpairs. Both mechanisms are recorded by the fail-closed harness.

## Evidence boundary

- `evidence/local-revalidation/benchmark_matrix_local_intel.json` contains the
  local 49-case harness result. Its timings are not release benchmark numbers.
- `evidence/local-revalidation/tsan_local.json` contains the clean TSan run and
  positive-control result.
- `BENCHMARKS.md` and `evidence/epyc-benchmark/` remain the authoritative
  performance record.
