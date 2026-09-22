# Changelog

## 0.1.0-alpha.1 — 2026-09-21

Maintenance update based on independent testing of the first alpha.

### Reliability

- Fixed Python direct-writer destruction order so a writer always releases its
  slot before the producer and shared-memory mapping that it borrows are
  destroyed, including after an expected `BufferError` from committing while a
  NumPy view is still exported.
- Added a subprocess regression that exercises final cleanup after that expected
  API error.

### API and documentation

- Changed `tenzorbus_rs.create()` to fail when the ring name already exists by
  default; replacement now requires an explicit `force=True`.
- Added regression coverage proving existing handles remain on the original
  mapping while new attachments use an explicitly forced replacement, including
  the required `keep_on_close()` coordination for a live old creator.
- Corrected the README cleanup example to use the public
  `unlink_on_close()`/object-lifetime API and documented immediate `unlink()`
  behavior and name-collision semantics.

### Packaging

- Bumped the Python and Rust package versions to `0.1.0a1` and
  `0.1.0-alpha.1` and updated the release-asset checks for the next alpha patch.
- Made source-archive checksums use the exact normalized Git bytes placed in
  release archives, independent of checkout line endings.
- Pinned text suppression files to LF so the TSan gate also works from a
  Windows-hosted checkout.
- Carried forward the alpha.0 benchmark evidence byte-for-byte; no benchmark
  results or claims were changed.

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
