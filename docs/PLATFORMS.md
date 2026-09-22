# Platform support

TenzorBus v0.1.0-alpha.2 supports native 64-bit little-endian Linux on x86-64
and ARM64. Windows, macOS, 32-bit targets, and big-endian targets are outside
this release.

| Rust target | Native CI | Python production wheel |
|---|---|---|
| `x86_64-unknown-linux-gnu` | GitHub-hosted `ubuntu-latest` | `cp311-abi3-manylinux_2_34_x86_64` |
| `aarch64-unknown-linux-gnu` | GitHub-hosted `ubuntu-24.04-arm` | `cp311-abi3-manylinux_2_34_aarch64` |

Both wheels use the Python 3.11 stable ABI and require glibc 2.34 or newer.

## Architecture audit

- Protocol fields have fixed widths and fixed offsets. Protocol version 1,
  global and slot header sizes, 64-byte slot alignment, magic values, and
  golden bytes are identical on x86-64 and ARM64.
- The production targets provide native 16-, 32-, and 64-bit atomics. Shared
  coordination words remain naturally aligned inside the page-aligned mapping.
  Rust release/acquire ordering supplies the required cross-core ordering on
  both architectures; no x86-only fences or instructions are used.
- Wait/wake uses Linux shared futex operations through `libc::SYS_futex` without
  `FUTEX_PRIVATE_FLAG`, so processes mapping the object at different virtual
  addresses coordinate on the same shared words.
- Storage uses POSIX `shm_open`, `ftruncate`, `mmap(MAP_SHARED)`, `fstat`, and
  `shm_unlink`; none uses architecture-sized data in the protocol.
- Process liveness reads architecture-independent fields 3 and 22 from
  `/proc/<pid>/stat`, with `kill(pid, 0)` only as a procfs fallback.
- Python buffer pointers and Rust `usize` values are process-local API values,
  not serialized protocol fields. Both production targets have 64-bit pointers.

Unsupported targets fail at compile time rather than producing a binary that
could silently interpret the frozen layout incorrectly.

## Native ARM64 gate

The ARM64 job runs on a real `aarch64` GitHub-hosted VM, not under emulation. It
runs the locked release Rust workspace tests, builds the `cp311-abi3` manylinux
wheel, installs the portable and native wheels into a clean virtual environment,
and runs the Python ring, Rust-binding, and lifetime suites.

Coverage includes create/attach, zero-copy NumPy views, reserve/commit,
multi-consumer fan-out, sequence wraparound, existing-name refusal and forced
replacement, cleanup after expected buffer errors, unlink behavior, producer
and consumer death recovery, and 64-consumer enforcement. A final installed-
wheel smoke test performs another real shared-memory roundtrip and verifies
that live mappings remain usable after unlink.

The same native job builds the supported TenzorPipe v0.3.2 release and the
scoped direct-write bridge on ARM64. It decodes a real H.264/AAC fixture into
ten `[3, 224, 224]` `float32` tensors, writes them directly into TenzorBus slots,
and verifies a separate NumPy consumer receives sequences 1 through 10 with
source timestamps from 0 through 4.5 seconds and byte-identical payloads. The
test also requires normal unlink cleanup and refuses a subsequent attach.

## Remaining ARM64 limitations

- The native ARM64 job does not install PyTorch, so its five PyTorch-specific
  view/lifetime tests skip. The release makes a native ARM64 NumPy/PyO3 claim,
  not an ARM64 PyTorch claim.
- ThreadSanitizer and its positive control remain on Linux x86-64. ARM64 uses
  the same synchronization implementation and receives native functional,
  multi-process, crash-recovery, and lifecycle coverage.
- The authoritative performance evidence remains the accepted AMD EPYC Linux
  x86-64 run. No ARM64 benchmark was run or inferred from it.
