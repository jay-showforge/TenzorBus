# Authoritative EPYC benchmark evidence

This directory records the final benchmark gate accepted for
TenzorBus `v0.1.0-alpha.0`.

- Result: **49/49 cases completed, zero benchmark errors**
- Host: **AMD EPYC 9V74, 9-vCPU Linux KVM**
- Matrix: seven transports across four single-consumer payload sizes, plus
  2/4/8-consumer fan-out at the real 602,112-byte TenzorPipe frame size
- Source raw JSON SHA-256:
  `ba86271180b80da10a1c542312b14b2a8c643713eefb728c2aa12db45771debe`
- Raw JSON: `epyc-kvm-final-benchmark-2026-09-21-complete.json`
- Original CSV: `TenzorBus-Final-49-Case-Benchmark.csv`
- Human-readable report: `TenzorBus-Final-49-Case-Benchmark.md`
- Repository benchmark documentation: `../../BENCHMARKS.md`

The JSON and CSV are the authoritative files recovered from the completed EPYC
benchmark task. `scripts/verify_authoritative_epyc.py` verifies the fixed raw
JSON digest, the 49/49 zero-error validation record, every CSV row against its
corresponding raw case, and the required report disclosures.

## Unix socket methodology

The VM seccomp policy rejected `socket(AF_UNIX, ...)` with `EPERM` before a
pathname was involved. The run did **not** omit Unix sockets. It used real
`AF_UNIX/SOCK_STREAM` socketpairs, one per consumer, inherited by independent
consumer processes across `exec`. Connection setup remained outside the timed
region. The fail-closed harness records the selected Unix transport in the
top-level report and in every Unix row.

## Direct no-fill caveat

`tenzorbus_direct_nofill` is the transport floor for a producer that generates
its output directly into a reserved TenzorBus slot. Payload generation is
outside its timed interval. It is **not directly comparable** to transports
that copy an already-existing payload and must never be used as a like-for-like
headline comparison.
