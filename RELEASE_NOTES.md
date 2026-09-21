# TenzorBus v0.1.0-alpha.0

TenzorBus is a Linux-focused shared-memory tensor transport for one producer and
multiple local consumers. This first standalone engineering prerelease includes
the Rust transport, Python bindings and reference implementation, the
TenzorPipe v0.3.2 direct-write integration patch, verification harnesses, and
the final EPYC benchmark evidence.

## Release highlights

- Publish one tensor into a bounded shared-memory ring and let multiple
  processes read the same slot through zero-copy NumPy or PyTorch views.
- Reserve a writable slot so a cooperating producer can generate directly into
  the consumer-visible payload.
- Recover from dead consumers, abandoned writes and stale registrations without
  silently corrupting sequence or slot ownership.
- Preserve media timestamps, including a timestamp of exactly zero.
- Refuse unsupported direct-write modes rather than silently falling back to a
  copied path.

## Accepted verification

- 18 release-candidate gates passed, zero failed and zero skipped.
- 47 Rust tests passed.
- 48 Python tests passed after four fail-closed benchmark regressions were
  added; zero skipped.
- Pristine-versus-patched TenzorPipe comparison: 58 byte-identical outputs and
  two matching refusals, zero mismatches.
- Direct path: zero producer copies; copy-path control: 120 producer copies.
- Stress and soak gates reported zero corruption, gaps or ordering failures.
- ThreadSanitizer was clean and its injected-race positive control was detected.

## Final benchmark

The accepted benchmark ran on an AMD EPYC 9V74, 9-vCPU Linux KVM host and
completed 49/49 cases with zero errors. The complete table and methodology are
in [`BENCHMARKS.md`](BENCHMARKS.md).

TenzorBus does not win every single-consumer metric. It is primarily designed
for shared-memory fan-out and copy scaling: one publication and one
producer-side copy for an existing payload, followed by zero-copy consumer
views as the fan-out grows.

The host's seccomp policy blocked pathname Unix sockets at socket creation. The
seven Unix rows use real `AF_UNIX/SOCK_STREAM` socketpairs inherited by separate
consumer processes across `exec`, with connection setup outside the timed
region.

`tenzorbus_direct_nofill` is the transport floor for in-place generation. It
excludes the fill/generation step and must not be compared directly with paths
that copy an existing payload.

## Important limitations

- This is an alpha prerelease, not a stable API promise.
- The production transport target is Linux. Windows/macOS support and the C ABI
  remain future work.
- The release assets include both the portable reference-package wheel and the
  production Linux x86-64 `tenzorbus_rs` wheel.
- TenzorPipe itself is not bundled. Its scoped v0.3.2 integration patches and
  bridge are included.
- Tenzor Live Lab has not been started; only its approved future visual
  reference is retained.

## Assets

- `tenzorbus-0.1.0a0-py3-none-any.whl`
- `tenzorbus_py-0.1.0a0-cp311-abi3-manylinux_2_34_x86_64.whl`
- `tenzorbus-0.1.0a0.tar.gz`
- `TenzorBus-v0.1.0-alpha.0-source.zip`
- `TenzorBus-v0.1.0-alpha.0-source.tar.gz`
- `epyc-kvm-final-benchmark-2026-09-21-complete.json`
- `TenzorBus-Final-49-Case-Benchmark.csv`
- `TenzorBus-Final-49-Case-Benchmark.md`
- `SHA256SUMS`

This handoff intentionally stops before repository creation, push, tag, GitHub
release creation or asset upload. Those actions require the explicit command
`publish`.
