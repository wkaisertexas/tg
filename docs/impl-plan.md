# `tg` Prompt Editor Implementation Plan

## 1. Purpose

This plan migrates the shipped v0.1 selector into the prompt editor specified
by [`spec.md`](spec.md). Each phase should leave the repository buildable and
retain the existing extraction, search, lowering, installer, updater, and
benchmark coverage.

## 2. Migration Principles

- Extract reusable services before replacing the application shell.
- Keep existing tests passing unless a documented product behavior changed.
- Put editor state, provider state, and rendering in separate modules.
- Run filesystem work, parsing, tokenization, and subprocesses off the UI
  thread and reject stale generations.
- Introduce no dynamic plugin ABI.
- Never make `gh` or `jira` authentication a prerequisite for local features.
- Preserve release artifact names and the `tg` executable.

## 3. Target Module Shape

```text
src/
├── main.rs
├── cli.rs
├── app/
│   ├── mod.rs
│   ├── event.rs
│   ├── command.rs
│   └── render.rs
├── editor/
│   ├── mod.rs
│   ├── document.rs
│   ├── reference_ranges.rs
│   └── save.rs
├── config/
│   ├── mod.rs
│   ├── schema.rs
│   └── sources.rs
├── references/
│   ├── mod.rs
│   ├── model.rs
│   ├── session.rs
│   ├── file.rs
│   ├── symbol.rs
│   ├── skill.rs
│   ├── github.rs
│   └── jira.rs
├── tokens.rs
├── language/
├── search/
├── repository.rs
├── preview.rs
└── updater.rs
```

The exact filenames may change, but `app.rs` must no longer own indexing,
provider logic, document lowering, clipboard encoding, key interpretation, and
all rendering in one type.

## 4. Phase 0: Characterization

Before structural changes:

- add tests around current `@`, `%`, `::`, Markdown, ambiguity, stale-file,
  OSC 52, and repository-discovery behavior;
- capture representative rendered results independently of the current layout;
- record current benchmark baselines; and
- add a pseudo-terminal harness that can drive a terminal binary.

This phase establishes which existing behavior is intentionally preserved and
which behavior changes under the new specification.

## 5. Phase 1: Configuration Foundation

Add typed configuration with compiled defaults and support:

- XDG or `~/.config/tg/config.toml` user path;
- repository `.tg.toml`;
- restricted project overrides;
- the small `TG_*` environment surface;
- CLI overrides;
- strict unknown-key diagnostics; and
- leader validation.

Use `serde`, `toml`, and optionally `figment` for source layering. Do not permit
repository config to select provider executables or external skill roots.

Verification:

- unit tests for every precedence layer and safety restriction;
- fixture configs for the documented schema; and
- `tg --help` documents file, root, config, and update behavior.

## 6. Phase 2: Reference Engine Extraction

Move current file and symbol completion out of `App` behind provider-neutral
types:

- `ReferenceKind`;
- `QueryRequest` and generation ID;
- `ReferenceCandidate`;
- `ReferenceTarget`;
- `ResolvedReference`; and
- validation, lowering, preview, and context-cost results.

Adapt `search`, `language`, `composer`, and `preview` rather than rewriting
them. Repository-wide symbol indexing remains lazy and batched.

Verification:

- all current extraction and lowering fixtures pass through the new boundary;
- stale generations are discarded;
- file changes re-resolve or invalidate symbols; and
- no provider work occurs on a render-thread test executor.

## 7. Phase 3: GPT-4o Token Service

Add `tiktoken-rs` and initialize the `o200k_base` singleton lazily. Implement:

- whole-file counts;
- symbol-range counts using existing byte ranges;
- metadata and tokenizer-keyed caching;
- one-decimal `k` formatting;
- pending/unavailable states; and
- deduplicated live totals with whole-file subsumption.

Do not use a bytes-per-token estimate while exact GPT-4o counting is pending;
display an ellipsis.

Verification:

- known strings match checked-in token-count fixtures;
- Unicode symbol slices use valid byte boundaries;
- cache invalidation follows metadata changes;
- duplicated file/range identities total correctly; and
- counting large files does not stall input.

## 8. Phase 4: Core Language Adapter Expansion

Preserve the current C, C++, Rust, Python, and Markdown fixtures, then add the
remaining core-tier adapters in small groups:

1. JavaScript, JSX, TypeScript, and TSX;
2. Go and Java;
3. C# and Ruby; and
4. Bash and POSIX shell.

Each group adds pinned grammar crates, extension and conventional-filename
detection, symbol queries, malformed-source coverage, duplicate-name coverage,
and representative benchmarks. A grammar dependency without extraction
fixtures does not mark the language supported.

Extended-tier adapters follow after the editor gate and ship independently.
Prioritize them from demonstrated user demand, grammar maintenance quality, and
the usefulness of their named structural units. Start with Nix, HCL/Terraform,
Kotlin, Swift/Objective-C, PHP, SQL, YAML, TOML, JSON/JSONC, GraphQL, Protocol
Buffers, Vue, and Svelte before the remaining extended list.

Before planning any adapter, verify that a maintained
`tree-sitter-<language>` package exists, is consumable from Rust, and has a
compatible license and release history. If that gate fails, remove or defer the
language; do not implement, fork, or vendor a grammar in this repository.

## 9. Phase 5: Editor Widget Spike

Build a narrow prototype branch or example for `vimltui` that proves:

- required Normal/Insert/Visual behavior;
- absolute-current plus relative-other line numbers;
- multiline Unicode and bracketed paste;
- custom rendering for structured reference spans;
- interception of `:w`, `:q`, `:q!`, `:wq`, and `:copy`;
- completion overlay event routing;
- `Ctrl-P` preview toggling; and
- undo/redo integration with external reference metadata.

If any of the last four require invasive patching or cannot be made reliable,
repeat the spike with `edtui`. Record the selection and limitations in an
architecture decision note before integrating it.

The prompt buffers in scope do not justify introducing Ropey independently of
the chosen widget.

## 10. Phase 6: Document, Save, and Clipboard

Implement `Document` around the selected widget:

- open existing or new UTF-8 files;
- track revision and saved revision;
- map or invalidate structured spans through transactions;
- include metadata in undo/redo history;
- detect external changes;
- render immutable lowered snapshots;
- atomically write snapshots; and
- implement `:copy` with the existing OSC 52 encoder.

Update the CLI to `tg [OPTIONS] [FILE]`. Repository discovery follows
`--root`, process CWD Git root, file-parent Git root, then CWD.

Verification:

- unit tests cover range mapping and lowering;
- save-failure injection never corrupts the destination;
- a parent-process test confirms synchronous external-editor behavior; and
- `:copy` bytes equal `:w` bytes for the same document revision.

## 11. Phase 7: Minimal Application Shell

Replace the current persistent header, transcript, result pane, preview pane,
and five-line prompt box with:

- full-screen editor;
- line-number gutter;
- one status line; and
- completion overlay created only while a provider query is active.

Add manual preview toggling with `Ctrl-P`. Keep result navigation distinct from
that binding. Add narrow-terminal degradation and no-color behavior.

Verification:

- snapshots prove idle mode has no completion or preview chrome;
- relative numbering follows cursor movement;
- overlays remain within terminal bounds;
- status totals update as spans change; and
- terminal state restores after normal and injected-error exits.

## 12. Phase 8: Local Skill Discovery

Implement the built-in `codex-local` profile without app-server calls:

- repository ancestor `.agents/skills` roots;
- `~/.agents/skills`;
- legacy and bundled `$CODEX_HOME/skills` roots;
- `/etc/codex/skills`;
- locally installed plugin manifest skill roots;
- symlink and cycle handling;
- `SKILL.md` frontmatter;
- optional `agents/openai.yaml` interface metadata; and
- applicable Codex `[[skills.config]]` disable rules.

Then implement generic TOML skill roots and mention templates. Accepting a
skill inserts only the agent-native mention.

Verification:

- fixtures mirror repository, user, admin, bundled, and plugin layouts;
- disabled and malformed skills do not appear;
- duplicates show scope/path disambiguation; and
- no Codex executable or server is needed.

## 13. Phase 9: GitHub Provider

Add separate issue and pull-request providers using `gh` JSON output. Commands
run without a shell, infer repository and host through `gh`, inherit its normal
authentication, and return full URLs.

Add timeouts, output caps, cancellation, error normalization, and missing-CLI
diagnostics.

Verification uses a fake `gh` executable to assert arguments, environment
noninterference, JSON parsing, exact-number ranking, enterprise URLs, timeouts,
and failures. An opt-in manual test covers a real authenticated repository.

## 14. Phase 10: Jira Provider

Add Jira search using `jira --raw` JSON and inherited CLI configuration.
Implement:

- configured project-key prefix;
- digits-only expansion such as `123` to `G5-123`;
- preservation of complete explicit keys;
- text search through JQL in the configured project;
- Jira base-URL discovery; and
- full `/browse/KEY` lowering.

Verification uses a fake `jira` executable and fixtures for Cloud and
on-premises response shapes, basic/bearer/mTLS-compatible inherited settings,
prefix normalization, malformed JSON, timeouts, and authentication errors.

## 15. Phase 11: Headless and Compatibility Cleanup

Update `--resolve` to use the new lowering engine where operations are
deterministic without an interactive candidate selection. Keep exact file,
symbol, skill, GitHub URL, and Jira URL resolution scriptable where possible.

Remove the old REPL transcript and multi-submission behavior. Update examples,
README instructions, shell snippets for setting `VISUAL`/`EDITOR`, and Nix
packaging. Mark old v0.1 UI documentation as historical.

Run:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
nix flake check
cargo bench --bench repository
```

## 16. Phase 12: Release and Installer Verification

Preserve existing release targets, checksum publication, installer environment
overrides, and `tg update`. Add smoke tests that install the built artifact and
drive:

- `tg --version`;
- `tg --help`;
- a headless resolution;
- direct editing in a pseudo-terminal; and
- external-editor invocation from a parent process.

Add pseudo-terminal installer tests for the optional shell setup. The tests
must cover both variables already set, each variable independently unset, both
unset, Enter/default refusal, explicit `EDITOR`/`VISUAL`/both choices, existing
rc-file assignments, Bash and Zsh selection, an unknown shell, no controlling
terminal, and `TG_NO_EDITOR_PROMPT=1`. Every prompt shows the destination and
exact lines before appending; no refusal or skipped case changes an rc file.

Release notes must call out that `tg .` no longer opens the selector REPL and
show the new `tg FILE` and `tg --root DIR FILE` forms.

## 17. Suggested Dependencies

Retain:

- `ratatui`, `crossterm`, `ignore`, `tree-sitter` and grammar crates;
- `rayon`, `clap`, `serde`, `reqwest`, `sha2`, `base64`, and `anyhow`.

Evaluate or add:

- `vimltui`, with `edtui` as the fallback editor widget;
- `tiktoken-rs` for GPT-4o `o200k_base` counting;
- `figment` plus `toml` for layered typed configuration;
- `nucleo` to replace `fuzzy-matcher` after provider extraction;
- `serde_json` for `gh` and `jira` output; and
- a small cancellation/channel abstraction only if standard channels become
  unwieldy.

Do not add an HTTP GitHub/Jira client, credential store, dynamic-loading crate,
LSP stack, embedding model, or general editor framework.

## 18. Delivery Gates

The work should be released in coherent gates:

1. **Core gate:** config, provider-neutral references, token counts, and the
   complete core language tier behind the current UI.
2. **Editor gate:** file lifecycle, Vim widget, minimal UI, save lowering, and
   `:copy`.
3. **Context gate:** local skills, GitHub, and Jira providers.
4. **Release gate:** docs, compatibility cleanup, benchmarks, packaging, and
   installer smoke tests.

Each gate must be usable without the later providers and must not regress
local file or symbol search performance materially from the recorded baseline.
