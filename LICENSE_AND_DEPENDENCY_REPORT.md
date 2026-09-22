# Licence and dependency audit — TenzorBus v0.1.0-alpha.1

Audited 2026-09-20 against the release candidate tree, on the pinned toolchain
`rustc 1.98.1 (48a229cea 2026-09-01)`.

**Result: clean.** 17 crates in the graph, no copyleft, nothing undeclared, no wildcard version
requirements, no non-crates.io sources.

## Project licence

Business Source License 1.1.

| Parameter | Value |
|---|---|
| Licensor | Jonathan Tyler Montgomery |
| Licensed Work | TenzorBus v0.1.0 |
| Additional Use Grant | Non-commercial research, education and personal projects; commercial entities (including affiliates) under $100,000 USD gross annual revenue |
| Change Date | 20 September 2030 |
| Change License | Apache License, Version 2.0 |

Declared consistently in all four manifests — `rust/tenzor-core/Cargo.toml`,
`rust/tenzorbus/Cargo.toml`, `rust/tenzorbus-py/Cargo.toml` and `pyproject.toml`. No
disagreement between them.

**This is source-available, not OSI open source**, and should not be described as the latter.
Production use above the revenue threshold, and embedded or OEM distribution, require a
commercial licence.

## Dependency graph

14 external crates, 3 workspace crates.

| Crate | Version | Licence | Why it is present |
|---|---|---|---|
| libc | 0.2.189 | MIT OR Apache-2.0 | POSIX shared memory and locking |
| heck | 0.5.0 | MIT OR Apache-2.0 | via pyo3 |
| once_cell | 1.21.4 | MIT OR Apache-2.0 | via pyo3 |
| portable-atomic | 1.15.0 | Apache-2.0 OR MIT | via pyo3 |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 | via pyo3 macros |
| pyo3 | 0.29.2 | MIT OR Apache-2.0 | Python extension |
| pyo3-build-config | 0.29.2 | MIT OR Apache-2.0 | via pyo3 |
| pyo3-ffi | 0.29.2 | MIT OR Apache-2.0 | via pyo3 |
| pyo3-macros | 0.29.2 | MIT OR Apache-2.0 | via pyo3 |
| pyo3-macros-backend | 0.29.2 | MIT OR Apache-2.0 | via pyo3 |
| quote | 1.0.47 | MIT OR Apache-2.0 | via pyo3 macros |
| syn | 2.0.119 | MIT OR Apache-2.0 | via pyo3 macros |
| target-lexicon | 0.13.5 | Apache-2.0 WITH LLVM-exception | via pyo3 build config |
| unicode-ident | 1.0.26 | (MIT OR Apache-2.0) AND Unicode-3.0 | via syn |
| tenzor-core | 0.1.0-alpha.1 | BUSL-1.1 | workspace |
| tenzorbus | 0.1.0-alpha.1 | BUSL-1.1 | workspace |
| tenzorbus-py | 0.1.0-alpha.1 | BUSL-1.1 | workspace |

The runtime transport is nearly dependency-free: `tenzorbus` depends only on `libc` and the
in-tree `tenzor-core`. Every other crate arrives through `pyo3` and exists solely for the Python
extension. A pure-Rust consumer of this library pulls in `libc` and nothing else.

Python runtime dependency: `numpy>=1.22`. Nothing else.

## Vendored and third-party source

**None.** TenzorBus carries no vendored external source.

TenzorPipe integration is delivered as a patch against TenzorPipe's public v0.3.2 tag, applied to
a checkout the operator supplies. The patch is not a fork, contains no third-party code, and
TenzorPipe is not redistributed as part of this package.

## Enforcement

`rust/deny.toml` did not exist before this release; the policy was documented nowhere and
enforced by nothing. It now fails closed:

| Check | Setting |
|---|---|
| Allowed licences | `MIT`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `Unicode-3.0` |
| Workspace BUSL-1.1 | pinned exceptions at `=0.1.0-alpha.1` |
| Wildcard versions | `deny` |
| Yanked crates | `deny` |
| Unknown registry / git sources | `deny` |
| Duplicate versions | `warn` |

The allowlist names only what the graph contains today. A future dependency under BSD-2-Clause,
BSD-3-Clause, ISC or Zlib will fail the gate. Those are unobjectionable licences; the point is
that widening the list should be a recorded decision rather than something that happens quietly
during a `cargo update`.

GPL, LGPL, AGPL, SSPL, CDDL, EPL and MPL are absent deliberately and are not authorised.

`BUSL-1.1` is granted to the three workspace crates by pinned exception. It is never an allowlist
entry and never authorises a dependency.

## Verification

    cd rust
    cargo deny check licenses      # -> licenses ok
    cargo tree --workspace --edges normal --prefix none | sort -u

Reproduced clean on 2026-09-20.

## Note on the prebuilt artefact

The handoff shipped a prebuilt `src/tenzorbus_rs.abi3.so`
(`c5fec9e2ed71d8636a1cdec717bb941babd56bbd43f4d75a10b3f22558b48e72`). It was **not executed and
not audited**; the extension was rebuilt from source and that build is what these results
describe. The prebuilt binary is excluded from the release package.
