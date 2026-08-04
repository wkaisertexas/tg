# tscodeselection

A Rust terminal composer for fuzzy file references and Tree-sitter symbol selection.
`@` respects Git ignores; `%` searches ignored files too. C, C++, Rust, and Python
symbols lower to natural `path::line:byte-column SymbolName` references on submission.

## Scaffold

- `src/app.rs` — Ratatui REPL, input handling, results, and source preview
- `src/search/` — Git-aware/broad walking and fuzzy path ranking
- `src/language/` — Tree-sitter parsing, extraction, and symbol ranking
- `src/composer/`, `repository.rs`, `preview.rs` — lowering, roots, and previews
- `tests/` — extraction, search, and YAML-driven end-to-end coverage

## Build and verify
```sh
cargo build --release
cargo test --all-targets --all-features
```

## Run
```sh
cargo run --release -- <root-folder>
```

Type `@`/`%` for files or standalone `::` for repository symbols. Complete with
`Tab` or Enter. Enter submits when completion is closed; `Ctrl-J` always submits. `Esc`
dismisses; `Ctrl-C` cancels/clears/exits; `Ctrl-D` exits empty. For automation, add
`--resolve 'Inspect @src/lib.rs::Symbol'` after the root folder.
