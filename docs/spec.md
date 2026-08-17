# `tg` Prompt Editor Product Specification

## 1. Status

This document replaces the original symbol-aware selector MVP specification.
It defines the next version of `tg`: a small terminal editor for composing
coding-agent prompts with structured references.

The supporting designs are:

- [`editor-design.md`](editor-design.md) — buffer, Vim behavior, saving, and OSC 52;
- [`reference-providers.md`](reference-providers.md) — files, symbols, skills,
  GitHub, Jira, lowering, and token accounting;
- [`configuration-ui.md`](configuration-ui.md) — TOML configuration and the
  minimal interface; and
- [`impl-plan.md`](impl-plan.md) — migration and delivery sequence.

The existing Tree-sitter extraction and search behavior is the semantic-search
scope for this project. Vector search, embeddings, and natural-language code
retrieval are not part of this specification.

## 2. Product Definition

`tg` is a focused text editor for writing prompts that will be consumed by a
coding agent. It is deliberately not a general-purpose programming editor, but
it edits ordinary files rather than requiring a special prompt format or a
temporary-file location.

The following are equally valid:

```sh
VISUAL=tg EDITOR=tg codex
tg README.md
tg ~/prompts/refactor-auth.md
```

When an agent invokes `tg` through `VISUAL` or `EDITOR`, `tg` edits the supplied
file in place and exits synchronously. When a user invokes it directly, the
same editor and save behavior apply.

## 3. Goals

The product must:

- edit an existing or new UTF-8 text file in a terminal;
- provide familiar Vim modes, motions, operators, line numbers, search, undo,
  redo, registers, and command-line actions;
- use relative line numbers by default;
- retain the existing Git-aware, broad-file, and Tree-sitter symbol searches;
- provide full syntax-aware symbol support for the core language tier and a
  documented path to the extended language tier;
- discover coding-agent skills locally and insert the agent-native skill
  mention rather than the skill body;
- search GitHub issues and pull requests through `gh`;
- search Jira issues through `jira`;
- work with GitHub Enterprise and Jira Cloud or on-premises installations by
  inheriting the CLIs' existing configuration and authentication;
- show whole-file and symbol token sizes in thousands, rounded to one decimal;
- maintain a live total of the context represented by accepted references;
- lower structured references into durable agent-readable text when saving;
- copy the lowered prompt through OSC 52 with `:copy`;
- load typed TOML configuration from `~/.config/tg/config.toml` by default;
- allow reference leaders and Jira key prefixes to be configured;
- preserve the single-command installer and self-update experience; and
- support macOS and Linux.

## 4. Non-Goals

`tg` does not aim to:

- replace Vim, Neovim, Helix, or another general-purpose editor;
- provide LSP, DAP, compilation, refactoring, or project-file editing features;
- implement vector or embedding-based semantic search;
- invoke a model or submit a prompt to an agent itself;
- inline skill instructions into a prompt;
- store, prompt for, or manage GitHub or Jira credentials;
- call the Codex app server for skill discovery;
- load dynamic libraries or third-party executable plugins;
- support Windows; or
- guarantee compiler-accurate symbol resolution.

## 5. Invocation and Repository Context

The primary CLI is:

```text
tg [OPTIONS] [FILE]
```

`FILE` may exist or may name a new file whose parent directory exists. Version
one edits at most one file. With no file, `tg` opens an unnamed buffer that can
be copied but cannot be written until a filename-capable command is added.

The repository search root is resolved in this order:

1. an explicit `--root <DIRECTORY>` argument;
2. the Git worktree containing the process current working directory;
3. the Git worktree containing the edited file's parent directory; or
4. the process current working directory when no Git worktree is found.

Current-working-directory precedence is required for external-editor use.
Codex may store its prompt tempfile outside the project while retaining the
project as the editor process's current working directory.

Directories are not editable files. The old `tg .` selector invocation is
replaced by `tg --root .` with an unnamed buffer or by `tg --root . FILE`.

## 6. Editing Contract

The default editing mode is Normal mode. The minimum Vim-compatible surface is:

- Normal, Insert, and character/line/block Visual modes;
- `h`, `j`, `k`, `l`, word, line, and document motions with counts;
- delete, change, yank, paste, indent, case, and motion composition;
- insert, append, open-line, replace, and join operations;
- common text objects;
- `/` and `?` search with `n` and `N`;
- `u`, redo, and dot repeat;
- named and unnamed registers; and
- `:w`, `:q`, `:q!`, `:wq`, and `:copy`.

Relative line numbers are enabled by default. The current line shows its
absolute number so the gutter behaves like Vim's combined `number` and
`relativenumber` settings.

Detailed mode and command behavior is normative in
[`editor-design.md`](editor-design.md).

## 7. Reference Model

A reference begins only when a configured leader is typed at a token boundary.
The defaults are:

| Reference kind | Leader | Example |
| --- | --- | --- |
| Git-aware file | `@` | `@src/app.rs` |
| Broad/ignored file | `%` | `%fixtures/generated.json` |
| Repository symbol | `::` | `::App.submit` |
| Coding-agent skill | `$` | `$gh-fix-ci` |
| GitHub issue | `#` | `#312` |
| GitHub pull request | `!` | `!87` |
| Jira issue | `&` | `&123` or `&G5-123` |

A file reference may be extended with `::` to search symbols within the
selected file. A standalone `::` searches the repository-wide Tree-sitter
index. Existing language support and symbol ranking are preserved.

Accepting a completion creates a structured reference span in the in-memory
document. Its friendly form remains visible until the session ends, while
saving or copying renders the lowered form.

Examples:

| Friendly form | Lowered form |
| --- | --- |
| `@src/app.rs` | `src/app.rs` |
| `@src/app.rs::submit` | `src/app.rs::347:5 submit` |
| `$gh-fix-ci` | `$gh-fix-ci` |
| `#312` | `https://github.example.com/acme/widget/issues/312` |
| `!87` | `https://github.example.com/acme/widget/pull/87` |
| `&123` with Jira prefix `G5` | `https://jira.example.com/browse/G5-123` |

Reference discovery and validation are specified in
[`reference-providers.md`](reference-providers.md).

## 8. Save and Copy Semantics

`:w` validates every structured reference, renders the complete buffer with
lowered references, and atomically writes that rendered text to the edited
file. The visible in-memory buffer retains friendly references so editing can
continue.

`:wq` performs the same write and exits successfully. This is the normal way
to return a prompt to an agent that invoked `tg` as its external editor.

`:copy` validates and lowers the buffer, emits an OSC 52 clipboard sequence,
and remains in the editor. It does not write the file or clear the dirty flag.

`:q` exits only when there are no unsaved changes. `:q!` exits without writing.
An error resolving a stale or ambiguous reference blocks `:w`, `:wq`, and
`:copy` and returns focus to that reference.

### 8.1 Language Coverage

Language support is divided into two explicit tiers. A language counts as
supported only when `tg` has a maintained Tree-sitter grammar, extension and
filename detection, useful symbol extraction, preview behavior, lowering, and
fixture-based tests. Merely parsing or syntax-highlighting a file does not
qualify as support.

#### Core release tier

The first prompt-editor release should fully support this compact set of common
application and systems languages:

- Bash and POSIX shell;
- C;
- C++;
- C#;
- Go;
- Java;
- JavaScript and JSX;
- Markdown;
- Python;
- Ruby;
- Rust; and
- TypeScript and TSX.

This tier includes the currently implemented C, C++, Rust, Python, and Markdown
adapters. The remaining adapters are release work, not silently implied by the
presence of a grammar crate.

#### Extended target tier

The larger target set is intentionally broad and should be delivered in
independent adapter increments:

- **Additional application languages:** Kotlin, Swift, Objective-C, PHP,
  Scala, Dart, Lua, Perl, R, Julia, Elixir, Erlang, Gleam, Haskell, OCaml, F#,
  Clojure, Zig, Groovy, and Solidity.
- **Infrastructure and build languages:** Nix, HCL/Terraform, Dockerfile,
  Make, CMake, Bazel/Starlark, and GitHub Actions workflow YAML.
- **Data and interface languages:** JSON, JSONC, YAML, TOML, SQL, GraphQL, and
  Protocol Buffers.
- **Web and component formats:** HTML, CSS, SCSS, Vue, and Svelte.

The extended tier has a hard dependency gate: a maintained, usable
`tree-sitter-<language>` grammar package must already exist and be consumable
from Rust. If no such package exists, that language is out of scope regardless
of its position in this list. `tg` will not create, fork, vendor, or become the
maintainer of a language grammar merely to add support. Package names may use
the ecosystem's established spelling, such as a combined package that supplies
multiple closely related dialects, but an upstream Tree-sitter package is
always required.

For languages without conventional declarations, the adapter exposes useful
named structural units rather than pretending they are classes or functions.
Examples include Markdown headings, HCL blocks, Nix attributes and functions,
SQL statements and named objects, GraphQL operations and types, Protocol
Buffer messages and services, build targets, and top-level configuration keys.
Large JSON/YAML/TOML documents must avoid indexing every scalar value.

All other UTF-8 files remain editable, searchable, previewable, selectable as
whole files, and token-counted even when symbol completion is unavailable.

## 9. Token Accounting

The default tokenizer is GPT-4o's `o200k_base` encoding. Token counts operate
on UTF-8 text and are cached by canonical path, size, and modification time.

File candidates display the whole-file count. Symbol candidates display both
the symbol range and whole-file count:

```text
submit                    symbol 1.2k · file 18.7k
```

Counts use thousands as the unit and one decimal place. Values below 1,000 may
be shown as an integer. The status line shows a live `refs` total.

The total represents the unique source context selected by structured
references:

- a whole-file reference contributes the entire file once;
- a symbol reference contributes its selected syntax range once;
- duplicate references to the same range are counted once; and
- a whole-file reference subsumes symbol references to that file.

The tokenizer design must permit additional named encodings later without
changing the reference or UI models.

## 10. Skills

The default Codex-compatible skill source is local filesystem discovery. It
must cover repository `.agents/skills` roots from the working directory to the
repository root, user and legacy Codex roots, admin/system roots, and installed
plugin skill roots that can be determined from local Codex manifests.

`SKILL.md` frontmatter supplies the name and description. Optional agent
metadata may improve display, but accepting a skill inserts only its invocation
name, such as `$skill-name`. The coding agent remains responsible for loading
and applying the skill.

Generic skill roots and emitted mention syntax are configurable in TOML so the
editor can work with agents other than Codex. No Codex app-server dependency is
permitted.

## 11. GitHub and Jira Providers

GitHub support invokes `gh` without a shell. The repository and host are
inferred by `gh` from the current worktree and its normal environment and
configuration. Issue and pull-request searches are separate. Accepted results
lower to their full web URLs.

Jira support invokes `jira` without a shell and consumes raw JSON. It inherits
the CLI's selected server, project, authentication type, token, config file,
and mTLS settings. A configured key prefix such as `G5` makes `&123` resolve as
`G5-123`; explicitly typed keys such as `&OTHER-42` are never rewritten.
Accepted results lower to their full `/browse/KEY-NUMBER` URLs.

Neither provider prompts for or persists credentials. A missing CLI disables
its provider and produces an actionable, nonfatal diagnostic.

## 12. Minimal Interface

The editor occupies the full terminal. Persistent UI is limited to:

- a line-number gutter;
- the text buffer; and
- one subdued status line containing mode, file, dirty state, reference total,
  and transient messages.

Completion results appear as an overlay only while a reference query is
active. Source preview is hidden by default and toggled with `Ctrl-P` while the
overlay is open. Toggling preview must not insert text or interfere with normal
Vim commands.

There is no persistent transcript, application banner, matches pane, preview
pane, or help legend. Help is available on demand.

## 13. Configuration

The default user configuration path is:

```text
~/.config/tg/config.toml
```

`$XDG_CONFIG_HOME/tg/config.toml` replaces `~/.config/tg/config.toml` when
`XDG_CONFIG_HOME` is set. Configuration layers and the complete schema are
specified in [`configuration-ui.md`](configuration-ui.md).

At minimum, configuration covers leaders, line-number style, preview behavior,
tokenizer name, search limits, Jira key prefix, provider executable paths,
skill roots, and emitted skill syntax.

## 14. Platform and Distribution

`tg` supports macOS and Linux on the architectures already shipped by the
release workflow. Windows-specific code paths, packaging, terminal behavior,
and authentication are out of scope.

The existing checksum-verifying curl installer, release binaries, `tg update`,
and `TG_INSTALL_DIR`/`TG_REPOSITORY` overrides remain supported. The default
installation remains `~/.local/bin/tg`.

After installing, the script may offer to configure `tg` as `EDITOR`, `VISUAL`,
or both when those variables are unset. This setup is strictly opt-in:

- the default and empty response is to make no shell-configuration changes;
- the prompt shows the exact `.zshrc` or `.bashrc` path and export lines before
  asking;
- choices are limited to doing nothing, setting only an unset `EDITOR`, setting
  only an unset `VISUAL`, or setting both when both are unset;
- existing environment values and existing rc-file assignments are never
  overwritten or duplicated;
- selected exports use the absolute installed `tg` path;
- an unrecognized shell or missing controlling terminal skips configuration
  and prints manual instructions when possible; and
- `TG_NO_EDITOR_PROMPT=1` disables the prompt for automation.

The download, verification, and binary installation complete before this
optional prompt, so declining it never makes installation fail.

## 15. Acceptance Criteria

The replacement is complete when:

1. `VISUAL=tg codex` can open, edit, save, and return a multiline prompt.
2. `tg README.md` edits and atomically saves an ordinary file.
3. `tg ~/prompts/task.md` uses the current repository for references when run
   from that repository.
4. Relative line numbers and the specified Vim command surface work.
5. Search UI has no visual weight before a leader is activated.
6. `@`, `%`, file-symbol, and repository-symbol behavior passes the existing
   extraction and lowering corpus.
7. Candidates show GPT-4o token counts and symbols show both range and file
   counts.
8. The status line updates a deduplicated reference total after every accepted,
   edited, or removed reference.
9. `Ctrl-P` toggles preview only while completion is open.
10. `$` discovers local Codex-compatible skills without an app server and
    inserts the native skill mention.
11. `#` and `!` find current-repository GitHub issues and pull requests through
    an already-authenticated `gh`, including an enterprise host.
12. `&123` applies a configured Jira prefix, Jira search works through an
    already-authenticated Cloud or on-premises `jira`, and the saved value is a
    full URL.
13. `:w` and `:wq` write lowered references, while `:copy` copies the same
    lowered text through OSC 52 without writing.
14. Missing provider CLIs, invalid TOML, stale references, and provider timeouts
    do not lose buffer contents or corrupt the edited file.
15. The installer and updater continue to work on supported macOS and Linux
    targets.
16. With unset editor variables, the interactive installer clearly offers
    `EDITOR`, `VISUAL`, both, or no change; Enter makes no change, existing
    assignments are preserved, and noninteractive installation never waits.
