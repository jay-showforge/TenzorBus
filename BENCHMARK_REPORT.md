# TenzorBus v0.1.0-alpha.0 benchmark report

The final accepted matrix completed **49/49 cases with zero errors** on an
**AMD EPYC 9V74, 9-vCPU Linux KVM** host.

See [`BENCHMARKS.md`](BENCHMARKS.md) for the complete table and methodology, and
[`evidence/epyc-benchmark/README.md`](evidence/epyc-benchmark/README.md) for
evidence provenance and the recorded source JSON SHA-256.

Two caveats are release-critical:

1. The EPYC VM's seccomp policy blocked pathname Unix sockets at
   `socket(AF_UNIX, ...)`. All seven Unix cases still ran using real
   `AF_UNIX/SOCK_STREAM` socketpairs inherited by independent consumer
   processes across `exec`.
2. `tenzorbus_direct_nofill` is an in-place-generation transport floor. Its
   fill/generation is excluded, so it is not directly comparable to transports
   that copy an already-existing payload.

The benchmark harness is fail-closed and rejects any exception, nonzero child
status, missing/invalid report, incomplete delivery, wrong matrix size or
unidentified Unix transport with a nonzero exit status.
