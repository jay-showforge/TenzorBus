# TenzorBus v0.1.0-alpha.2

TenzorBus is a Linux shared-memory tensor transport for one producer and
multiple local consumers. This maintenance alpha adds native Linux ARM64
support while preserving the protocol, public API, benchmark claims, and
existing Linux x86-64 behavior.

## Native ARM64 support

- Added `aarch64-unknown-linux-gnu` as a production target alongside
  `x86_64-unknown-linux-gnu`.
- Added a native GitHub-hosted `ubuntu-24.04-arm` gate. It runs the full Rust
  workspace tests and the Python API/lifetime tests on ARM64 hardware.
- Added clean-environment installation and a real POSIX shared-memory
  roundtrip for the ARM64 production wheel, including create/attach,
  zero-copy NumPy aliasing, reserve/commit, unlink, and continued use of live
  mappings after unlink.
- Added an explicit build-time platform contract: Linux, little-endian,
  64-bit pointers, and native 16/32/64-bit atomics.
- Added release checks that require distinct x86-64 and ARM64 `cp311-abi3`
  manylinux wheels in the final asset set.

## Compatibility and protocol

- The shared-memory protocol remains version 1. Magic values, offsets, field
  widths, alignment, registry capacity, and golden bytes are unchanged.
- The public Rust and Python APIs are unchanged.
- POSIX shared memory, futex synchronization, `/proc` start-time liveness
  tokens, process cleanup, and max-consumer enforcement run through the same
  implementation on x86-64 and ARM64.
- The alpha.1 expected-error cleanup, existing-name, forced-replacement, and
  unlink semantics remain unchanged and are included in native ARM64 tests.

## Validation scope and limitations

- Native ARM64 validation covers the Rust transport and NumPy/PyO3 API.
  PyTorch-specific ARM64 view tests are skipped because PyTorch is not installed
  in that job; no ARM64 PyTorch claim is made by this release.
- ThreadSanitizer remains an x86-64 gate. ARM64 synchronization is covered by
  native functional, multi-process, fan-out, wraparound, crash-recovery, and
  lifecycle tests.
- The manylinux wheels require glibc 2.34 or newer.
- Windows and macOS remain unsupported.
- The authoritative EPYC benchmark remains explicitly AMD EPYC Linux x86-64
  evidence. It was not modified or rerun, and no benchmark claim changed.
- Nothing has been published by this preparation step.

## Assets

- `tenzorbus-0.1.0a2-py3-none-any.whl`
- `tenzorbus_py-0.1.0a2-cp311-abi3-manylinux_2_34_x86_64.whl`
- `tenzorbus_py-0.1.0a2-cp311-abi3-manylinux_2_34_aarch64.whl`
- `tenzorbus-0.1.0a2.tar.gz`
- `TenzorBus-v0.1.0-alpha.2-source.zip`
- `TenzorBus-v0.1.0-alpha.2-source.tar.gz`
- `epyc-kvm-final-benchmark-2026-09-21-complete.json`
- `TenzorBus-Final-49-Case-Benchmark.csv`
- `TenzorBus-Final-49-Case-Benchmark.md`
- `SHA256SUMS`
