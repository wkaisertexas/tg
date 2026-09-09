# tg

A focused Vim-like terminal editor for composing coding-agent prompts with
structured references to files, symbols, local skills, GitHub issues and pull
requests, and Jira issues. Accepted references remain readable while editing
and lower to durable agent-readable text on save.

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

The installer supports macOS and GNU/Linux on arm64 and x86_64. It requires a
POSIX shell, `curl`, `uname`, `awk`, `mktemp`, `chmod`, `mv`, `mkdir`, `rm`, `sed`,
and either `sha256sum` or `shasum` (SHA-256). Optional Bash/Zsh editor setup also
uses `grep`, `tail`, `od`, and `tr`; if these are missing, installation still
succeeds and automatic editor setup is skipped. GitHub's `gh` and the `jira` CLI
are optional provider dependencies, not installer requirements.

```sh
nix profile install github:wkaisertexas/tg
# Or install the latest release binary without Nix:
curl -fsSL https://raw.githubusercontent.com/wkaisertexas/tg/main/install.sh | sh
```

By default, this installs to `~/.local/bin/tg`. `TG_INSTALL_DIR` overrides the
destination and `TG_REPOSITORY` overrides the release repository (default:
`wkaisertexas/tg`); pass installer overrides to the `sh` side of the pipeline.
The installer checks dependencies and destination permissions before downloading,
verifies the release's SHA-256 checksum, and runs the downloaded candidate's
`--version` before replacing an existing `tg`. Failed downloads, checksums, or
candidate smoke checks leave the installed binary untouched.

The summary prints quoted absolute commands for `setup` and `doctor`, so you
can start even if the install directory is not on `PATH`. It also prints PATH
guidance for Bash, Zsh, POSIX shells, and fish, with a manual fallback for other
shells. It does not change PATH or add PATH entries to shell startup files.

Editor setup is separately opt-in: when run interactively, the installer shows
the exact Bash/Zsh startup file and proposed `EDITOR`/`VISUAL` assignments before
asking. Enter means no change, and existing environment values or assignments in
that startup file are not overwritten. Other shells get manual guidance. A
piped `curl | sh` install never prompts or changes editor settings, but still
prints next steps. To suppress the editor offer and its manual guidance entirely:

```sh
curl -fsSL https://raw.githubusercontent.com/wkaisertexas/tg/main/install.sh | TG_NO_EDITOR_PROMPT=1 sh
```

Once `tg` is on PATH (or using the absolute path from the installer):

```sh
tg setup
tg doctor
tg README.md
tg --root . prompts/task.md
VISUAL=tg EDITOR=tg codex
cargo run --release --bin tg -- [FILE]
```

`tg setup` shows onboarding and the effective reference leaders for the current
configuration. The seven default leaders are:

| Leader | Reference |
| --- | --- |
| `@` | Git-aware files |
| `%` | Broad files, including ignored files |
| `::` | Repository symbols; after a file reference, symbols in that file |
| `$` | Local skills |
| `#` | GitHub issues |
| `!` | GitHub pull requests |
| `&` | Jira issues |

Leaders are configurable; use `tg setup` rather than assuming the defaults.
In the editor, `:providers` or Normal-mode `Space` then `p` shows provider status.

`tg doctor` reports local readiness without network requests. Remote checks are
explicitly opt-in: `tg doctor --check jira` checks Jira, and
`tg doctor --check all` checks the remote providers. Run these from the project
whose provider context you want to check. Authentication stays in the provider
CLIs: use `gh auth login` for GitHub and `jira init` for Jira, following those
CLIs' authentication instructions. `tg` does not collect or store their tokens;
provider commands inherit their CLIs' existing environment and configuration.
Missing remote CLIs or authentication do not block local file/symbol editing.

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
