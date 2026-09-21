# Local revalidation evidence

These files were generated during the 2026-09-21 packaging pass on an Intel
Core i5-14400F under WSL2. They validate the harness and sanitizer setup; they
do not replace or relabel the authoritative AMD EPYC benchmark evidence.

- `benchmark_matrix_local_intel.json`: fail-closed 49-case matrix, 49 complete,
  zero errors.
- `tsan_local.json`: zero warnings in the clean run and a successful injected-
  race positive control.

See `LOCAL_REVALIDATION.md` for the complete pass summary.
