# TenzorBus v0.1.0-alpha.1

TenzorBus is a Linux-focused shared-memory tensor transport for one producer and
multiple local consumers. This maintenance alpha incorporates independent
tester feedback without changing the transport protocol or benchmark evidence.

## Maintenance changes

- Corrected Python direct-writer destruction order. A reserved writer now
  releases its slot before its producer and mapping can be destroyed, including
  cleanup after an expected `BufferError` caused by a still-exported NumPy view.
- Added subprocess regression coverage for that cleanup path.
- Made `tenzorbus_rs.create(name, ...)` fail when `name` already exists unless
  callers explicitly request replacement with `force=True`.
- Added coverage for both collision refusal and forced-replacement mapping
  behavior.
- Corrected the README close/unlink example to match the real public API and
  documented delayed unlink, immediate unlink, and replacement semantics.
- Hardened release verification so source-archive checksums are independent of
  checkout line endings and TSan suppression files remain LF-only.

## Compatibility and scope

- The shared-memory protocol and its version are unchanged.
- Existing attached handles continue using their current mapping after a name is
  unlinked or explicitly replaced. New attachments resolve the replacement.
- Forced replacement requires coordination: a live old creator must select
  `keep_on_close()` before replacement so its later shutdown cannot unlink the
  replacement's name.
- No benchmark evidence or benchmark claims changed in this maintenance release.
- This is still an alpha prerelease and the production target remains Linux
  x86-64.
- Nothing has been published by this preparation step.

## Assets

- `tenzorbus-0.1.0a1-py3-none-any.whl`
- `tenzorbus_py-0.1.0a1-cp311-abi3-manylinux_2_34_x86_64.whl`
- `tenzorbus-0.1.0a1.tar.gz`
- `TenzorBus-v0.1.0-alpha.1-source.zip`
- `TenzorBus-v0.1.0-alpha.1-source.tar.gz`
- `epyc-kvm-final-benchmark-2026-09-21-complete.json`
- `TenzorBus-Final-49-Case-Benchmark.csv`
- `TenzorBus-Final-49-Case-Benchmark.md`
- `SHA256SUMS`
