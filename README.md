# tg

A Rust terminal composer for fuzzy file references and syntax-aware symbol selection.
`@` respects Git ignores; `%` searches ignored files too. C, C++, Rust, Python, and Markdown
code lowers to `path::line:column Name`; Markdown lowers to compact `path.md#heading` anchors.

The next version is being designed as a focused Vim-like prompt editor that can
be used through `VISUAL`/`EDITOR` or as `tg FILE`. See
[`docs/spec.md`](docs/spec.md), [`docs/editor-design.md`](docs/editor-design.md),
[`docs/reference-providers.md`](docs/reference-providers.md), and
[`docs/configuration-ui.md`](docs/configuration-ui.md).

## Scaffold

- `src/app.rs` — Ratatui REPL, input handling, results, and source preview
- `src/search/` — Git-aware/broad walking and fuzzy path ranking
- `src/language/` — Tree-sitter parsing, extraction, and symbol ranking
- `src/composer/`, `repository.rs`, `preview.rs` — lowering, roots, and previews
- `tests/` — extraction, search, and YAML-driven end-to-end coverage

## Build and verify
```sh
cargo build --release
just check
```
## Install and run
```sh
curl -fsSL https://raw.githubusercontent.com/wkaisertexas/tg/main/install.sh | sh
tg README.md
tg --root .
cargo run --release --bin tg -- [file]
```

Type `@`/`%` for files or standalone `::` for repository symbols; append `.` for members.
`Tab` or Enter. Enter submits when completion is closed; `Ctrl-J` always submits. `Esc`
dismisses; `Ctrl-C` cancels/clears/exits; `Ctrl-D` exits empty. For automation, add
`--resolve 'Inspect @src/lib.rs::Symbol'` with `--root` when needed. Upgrade with `tg update`.
