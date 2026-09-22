# Dependency and licence policy — TenzorBus

Enforced by `cargo deny check licenses` against `rust/deny.toml`. The gate fails closed: a
dependency whose licence is not listed stops the build rather than being waved through. That is
what keeps a copyleft crate from entering the graph unnoticed during a routine `cargo update`.

## Allowed licences

`MIT`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `Unicode-3.0`.

The list is deliberately narrow — it names what the graph contains today and nothing more. A new
dependency under BSD-2-Clause, BSD-3-Clause, ISC or Zlib will fail the gate. Those are perfectly
reasonable licences; the point is that widening the list should be a decision someone makes on
purpose, recorded in a commit, rather than something that happens silently.

GPL, LGPL, AGPL, SSPL, CDDL, EPL and MPL are absent deliberately and are not authorised.

`BUSL-1.1` is the project's own licence, not an allowlist entry. It is granted to the three
workspace crates through pinned exceptions (`=0.1.0-alpha.1`) so that a version bump has to be
made deliberately instead of being inherited by a wildcard. It never authorises a dependency.

## Current graph

17 crates: 14 external, 3 workspace. No copyleft, nothing undeclared.

| Crate | Version | Licence |
|---|---|---|
| heck | 0.5.0 | MIT OR Apache-2.0 |
| libc | 0.2.189 | MIT OR Apache-2.0 |
| once_cell | 1.21.4 | MIT OR Apache-2.0 |
| portable-atomic | 1.15.0 | Apache-2.0 OR MIT |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 |
| pyo3 | 0.29.2 | MIT OR Apache-2.0 |
| pyo3-build-config | 0.29.2 | MIT OR Apache-2.0 |
| pyo3-ffi | 0.29.2 | MIT OR Apache-2.0 |
| pyo3-macros | 0.29.2 | MIT OR Apache-2.0 |
| pyo3-macros-backend | 0.29.2 | MIT OR Apache-2.0 |
| quote | 1.0.47 | MIT OR Apache-2.0 |
| syn | 2.0.119 | MIT OR Apache-2.0 |
| target-lexicon | 0.13.5 | Apache-2.0 WITH LLVM-exception |
| unicode-ident | 1.0.26 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| tenzor-core | 0.1.0-alpha.1 | BUSL-1.1 (workspace) |
| tenzorbus | 0.1.0-alpha.1 | BUSL-1.1 (workspace) |
| tenzorbus-py | 0.1.0-alpha.1 | BUSL-1.1 (workspace) |

The runtime surface is deliberately small. `tenzorbus` depends on `libc` and the in-tree
`tenzor-core`; everything else in the graph arrives through `pyo3`, and only for the Python
extension. The pure-Rust transport pulls in no third-party crate beyond `libc`.

Python runtime dependency: `numpy>=1.22`. Nothing else.

## Other gates in `deny.toml`

- `wildcards = "deny"` — a dependency declared as `*` fails. Version ranges must be stated.
- `yanked = "deny"` — a yanked crate version fails rather than warning.
- `unknown-registry` / `unknown-git` = `"deny"` — only crates.io is an allowed source, so a
  dependency cannot be pulled from an arbitrary git URL without the policy being changed.
- `multiple-versions = "warn"` — duplicate versions of the same crate are surfaced but do not
  block. This is a warning rather than an error because `pyo3`'s own graph occasionally carries
  transitional duplicates that are outside this project's control.

## Third-party code carried in-tree

None. TenzorBus vendors no external source. TenzorPipe integration is by patch against its
public v0.3.2 tag, applied to a checkout the operator supplies — the patch is not a fork and
carries no third-party code of its own.

## Reproducing this audit

    cd rust
    cargo deny check licenses
    cargo tree --workspace --edges normal --prefix none | sort -u
