# tg

A focused Vim-like terminal editor for composing coding-agent prompts with
structured references. `@` searches Git-visible files, `%` includes ignored
files, `::` searches symbols, and `$` searches local skills. Accepted references
remain readable while editing and lower to durable agent-readable text on save.

`tg` can edit an ordinary UTF-8 file directly or act as `VISUAL`/`EDITOR`. See
[`docs/spec.md`](docs/spec.md), [`docs/editor-design.md`](docs/editor-design.md),
[`docs/reference-providers.md`](docs/reference-providers.md), and
[`docs/configuration-ui.md`](docs/configuration-ui.md).

## Structure

- `src/app.rs`, `src/editor/` — minimal full-screen editor and Vim adapter
- `src/references/` — file, symbol, skill, GitHub, and Jira providers
- `src/search/` — Git-aware/broad walking and fuzzy path ranking
- `src/language/` — Tree-sitter parsing, extraction, and symbol ranking
- `src/composer/`, `repository.rs`, `preview.rs` — headless lowering, roots, and previews
- `tests/` — extraction, search, and YAML-driven end-to-end coverage

## Build and verify
```sh
cargo build --release
just check
```
## Install and run
```sh
nix profile install github:wkaisertexas/tg
# Or install the latest release binary without Nix:
curl -fsSL https://raw.githubusercontent.com/wkaisertexas/tg/main/install.sh | sh
tg README.md
tg --root . prompts/task.md
VISUAL=tg EDITOR=tg codex
cargo run --release --bin tg -- [FILE]
```

The old `tg .` selector form is gone: directories are not editable files. Use
`tg FILE`, or combine an explicit search root and file with `tg --root DIR FILE`.

The editor supports normal, insert, visual, and visual-block modes; common Vim
motions/operators; search; undo/redo; registers; and exact `:w`, `:q`, `:q!`,
`:wq`, `:copy`, and `:r !command` commands. Accepted file references show their
token cost as subdued virtual text without changing the saved document. Use
`Tab` or Enter to accept an active completion, `Esc` to dismiss it, and `Ctrl-P`
to toggle its preview. In Normal mode, press
`Space` then `?` to inspect active bindings and configuration. For scripts, use
`tg --root DIR --resolve 'Inspect @src/lib.rs::Symbol'`. Upgrade with `tg update`.
