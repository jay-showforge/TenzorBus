# Changelog

## 0.1.0-alpha.0 — 2026-09-21

First standalone TenzorBus engineering prerelease.

### Transport

- Added the production Linux single-producer/multi-consumer Rust ring using
  named shared memory, atomics, futex wait/wake, bounded slots and crash
  recovery.
- Added block and drop-newest backpressure policies.
- Added sequence, timestamp, tensor metadata and 64-consumer registration.
- Fixed torn reads, mid-scan out-of-order delivery, duplicate delivery, zombie
  consumer slot pinning and PID-reuse liveness errors.

### Python

- Added abi3 Python bindings with zero-copy NumPy/PyTorch views.
- Enforced exported-view lifetime so a lease cannot release a live slot.
- Retained the Python reference implementation as an executable protocol
  contract.

### TenzorPipe v0.3.2

- Added a scoped direct-write integration patch and TenzorBus integration bridge.
- Preserved the normal `.tenzor` path; 58 cases were byte-identical and two
  unsupported cases produced matching refusals against a pristine v0.3.2 build.
- Added slot/sequence tracing, producer-copy accounting, corruption negative
  controls, backpressure propagation and killed-consumer recovery.

### Verification

- Pinned release verification to Rust 1.98.1.
- Added protocol parity, golden-byte, kill, stress, soak and TSan gates.
- Kept the TSan positive control so a silently nonfunctional sanitizer run
  cannot pass.
- Final accepted Linux results: 47 Rust tests and 48 Python tests, zero skips.

### Benchmarks

- Completed 49/49 cases with zero errors on an AMD EPYC 9V74, 9-vCPU Linux KVM
  host.
- Kept all seven Unix socket rows by using real inherited
  `AF_UNIX/SOCK_STREAM` socketpairs when seccomp blocked pathname sockets.
- Made the matrix fail closed on exceptions, child failures, missing/invalid
  reports, incomplete delivery, wrong case counts or unidentified Unix
  transport.
- Added four regression tests for the hardened harness.
- Documented `tenzorbus_direct_nofill` as an in-place-generation transport floor
  that is not directly comparable to copy paths.

### Packaging

- Prepared standalone source, wheel and source-distribution assets.
- Added release notes, final verification summary, complete benchmark table,
  evidence provenance and checksums.
- Retained the approved future Tenzor Live Lab visual reference without
  starting Live Lab implementation.
