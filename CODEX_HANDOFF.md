# Codex handoff: GitHub publication

TenzorBus `v0.1.0-alpha.0` is assembled, verified, and packaged. This handoff
stops deliberately before any external publication.

## Current state

- Standalone TenzorBus source, documentation, CI, fixtures, integration patches,
  evidence, and release notes are present.
- The final release pass is recorded in `FINAL_VERIFICATION.md`.
- The packaging-day independent pass is recorded in `LOCAL_REVALIDATION.md`.
- Release assets are listed in `RELEASE_NOTES.md` and accompanied by
  `SHA256SUMS` outside the source tree.
- No GitHub repository, remote, tag, release, or uploaded asset has been created.
- Tenzor Live Lab has not been implemented or started.

## Publication boundary

Do not create a repository, push, tag, publish a release, upload an asset, or
start Tenzor Live Lab unless the user explicitly says `publish`.

After that explicit command, confirm the intended GitHub owner, repository name,
and visibility. Then:

1. Create or select the standalone TenzorBus repository.
2. Import the contents of `TenzorBus-v0.1.0-alpha.0-source.zip` on `main`.
3. Run the included GitHub Actions workflows and resolve only reproducible
   release-blocking failures; do not rewrite accepted benchmark evidence.
4. Create the annotated tag `v0.1.0-alpha.0` only after CI is green.
5. Create a GitHub **prerelease** using `RELEASE_NOTES.md` and upload both
   wheels, both source archives, the authoritative EPYC evidence, and
   `SHA256SUMS`.
6. Verify every uploaded asset against `SHA256SUMS`.

## Evidence rules

- The authoritative benchmark is the 49/49, zero-error AMD EPYC 9V74 run in
  `BENCHMARKS.md` and `evidence/epyc-benchmark/`.
- The authoritative raw EPYC JSON, original CSV, and report are tracked under
  `evidence/epyc-benchmark/`. The raw JSON must continue to match SHA-256
  `ba86271180b80da10a1c542312b14b2a8c643713eefb728c2aa12db45771debe`.
- The local Intel/WSL2 matrix is verification evidence only and must never be
  relabeled as the release benchmark.
- `tenzorbus_direct_nofill` excludes generation/fill work. Never compare it as
  though it were an existing-payload copy path.
- The EPYC Unix rows used inherited real `AF_UNIX/SOCK_STREAM` socketpairs
  because seccomp blocked pathname Unix sockets.

## Historical documents

Earlier phase reports are retained for provenance and marked historical. For
current status, use `README.md`, `FINAL_VERIFICATION.md`, `BENCHMARKS.md`,
`RELEASE_NOTES.md`, and `LOCAL_REVALIDATION.md`.
