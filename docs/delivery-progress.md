# Delivery Progress

- [x] Inspect repository state and release metadata availability.
- [x] Rename the shipped binary to `tg` and preserve tested CLI behavior.
- [x] Copy successful submissions with OSC 52 without disturbing the TUI.
- [x] Add and test `tg update`, including permission-aware sudo guidance.
- [x] Add cross-platform GitHub release builds and artifact publication.
- [x] Add a checksum-verifying curl installer for macOS and Linux.
- [x] Add Criterion indexing/search benchmarks and corpus preparation tooling.
- [x] Shallow-clone Rust, LLVM, and Ansible benchmark corpora.
- [x] Parallelize indexing, reuse parsers, batch UI updates, and bound ranking sorts.
- [x] Run the complete benchmark suite and record results.
- [x] Run formatting, Clippy, tests, Nix build, and release-binary checks.

## Decisions

- `tg` is the canonical executable and update command (`tg update`).
- Release builds embed their GitHub `owner/repository`; local update builds may
  set `TG_REPOSITORY=owner/repository`.
- OSC 52 copies the lowered prompt, while the TUI transcript remains readable.
- Benchmark corpora live under ignored `benchmarks/corpus/` and use shallow clones.
- LLVM indexing improved from roughly 56 seconds to roughly 5.8 seconds on a
  14-core Apple workstation while indexing 71,158 source files.
- Criterion index means: Rust 1.38 s / 38,057 files, LLVM 5.74 s / 71,158
  files, and Ansible 63.4 ms / 1,812 files.
