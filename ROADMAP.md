# Roadmap

## Gate A — production Rust bus

- Compile `rust/` on Rust 1.98.1+.
- Implement named shared mappings and native wait/wake on Linux first.
- Replace reference file lock/polling with atomics.
- Add consumer heartbeat/death reclamation.
- Fuzz header parsing and slot state transitions.
- Run TSAN/Miri-equivalent ownership checks where applicable.

## Gate B — real TenzorPipe direct writer

- Integrate against current TenzorPipe v0.3.2 source.
- Extract only genuinely shared ABI/schema pieces into `tenzor-core`.
- Preserve TenzorPipe byte-identical `.tenzor` output and existing tests.
- Add an opt-in producer that writes selected tensor frames directly into reserved TenzorBus slots.

## Gate C — language / runtime adapters

- Python/PyTorch (PyO3)
- C ABI / C++
- Windows named mapping implementation
- macOS mapping/wakeup implementation

## Gate D — Tenzor Live Lab

- live pipeline event stream
- add/remove consumer interaction
- actual measured comparison mode
- video -> TenzorPipe -> TenzorBus -> detector/embedder/preview demonstration

## Later, only after evidence

- Unity / Unreal / Godot adapters
- robotics/edge integrations
- remote networking/protocol experiments
