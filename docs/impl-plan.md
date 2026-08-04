# Symbol-Aware File Selector MVP Implementation Plan

## 1. Purpose

This plan derives from [`spec.md`](spec.md). It breaks the MVP into increments
that remain runnable and testable while preserving the specification's key
boundary: file search is repository-wide, but symbol parsing begins only after
a file path resolves.

The repository is initially empty, so the first phase establishes both the Rust
workspace and its testing conventions.

## 2. Proposed Project Layout

```text
.
├── Cargo.toml
├── docs/
│   ├── spec.md
│   └── impl-plan.md
├── src/
│   ├── main.rs
│   ├── app.rs
│   ├── cli.rs
│   ├── event.rs
│   ├── repository.rs
│   ├── search/
│   │   ├── mod.rs
│   │   ├── walker.rs
│   │   └── matcher.rs
│   ├── composer/
│   │   ├── mod.rs
│   │   ├── reference.rs
│   │   └── lowering.rs
│   ├── language/
│   │   ├── mod.rs
│   │   ├── symbol.rs
│   │   ├── c.rs
│   │   ├── cpp.rs
│   │   ├── rust.rs
│   │   └── python.rs
│   ├── parse_cache.rs
│   ├── preview.rs
│   └── ui/
│       ├── mod.rs
│       ├── composer.rs
│       ├── results.rs
│       └── preview.rs
└── tests/
    ├── fixtures/
    │   ├── c/
    │   ├── cpp/
    │   ├── rust/
    │   ├── python/
    │   └── search_repo/
    ├── repository.rs
    ├── search.rs
    ├── extraction.rs
    └── lowering.rs
```

The exact module split may change as code emerges. The important boundaries are
that UI code does not contain language queries, language adapters do not own
terminal state, and lowering can be tested without running a terminal.

## 3. Phase 1: Bootstrap and Core Types

### Work

- Create a binary Cargo package using Rust 2024 edition.
- Add `clap`, error-handling, logging, and initial terminal dependencies.
- Define the CLI with one required root-directory argument.
- Define core types:
  - `SearchMode::{GitAware, Broad}`;
  - `ResolvedFile`;
  - `SymbolKind`;
  - `Symbol`;
  - `ResolvedReference`;
  - `SourcePoint`; and
  - query-generation identifiers for background work.
- Decide on one-based wrapper types or explicit conversion methods so
  zero-based Tree-sitter points cannot be emitted accidentally.
- Establish `cargo fmt`, `cargo clippy --all-targets --all-features`, and
  `cargo test` as the baseline validation commands.

### Verification

- CLI parsing accepts a directory argument and rejects omission.
- Unit tests cover point conversion and path-display normalization.
- The binary starts, reports the canonical invocation root, and exits cleanly.

## 4. Phase 2: Repository Discovery

### Work

- Canonicalize and validate the invocation root.
- Discover the nearest containing Git worktree root.
- Represent “no repository found” explicitly rather than as an error.
- Normalize emitted paths relative to the selected search root.
- Reject canonical paths that escape the search root.
- Handle Git worktrees where `.git` is a file rather than a directory.

Prefer a small, testable abstraction. `git2` avoids spawning a process, while
invoking `git rev-parse --show-toplevel` may better match installed Git
semantics. Choose one and record the reason in code documentation.

### Verification

- Integration tests cover invocation at the repository root and in a nested
  directory.
- Tests cover a non-Git directory.
- Tests cover a worktree-style `.git` file if the selected discovery library
  does not already guarantee it.
- Tests ensure relative output paths contain no `..` components.

## 5. Phase 3: Independent File Search Service

### Work

- Build a long-lived search session around `ignore::WalkBuilder` and `nucleo`.
- Run walking and matching outside the UI thread.
- Tag result snapshots with a generation number and discard stale generations.
- Include regular files and hidden files.
- Match against full search-root-relative paths.
- Return matched-character indices for UI highlighting.
- Implement the two walker policies:
  - `@`: Git and ignore processing enabled;
  - `%`: ignore processing disabled, with `.git` excluded explicitly.
- Do not follow symlinks outside the canonical search root.
- Surface unreadable-entry errors as nonfatal status events.
- Avoid default exclusions for `target`, `node_modules`, or generated folders.

The initial implementation can create one session per search mode and reuse its
walked candidates for subsequent queries. It must be possible to cancel or drop
a session without leaking worker threads.

### Verification

- A fixture repository proves that `@` omits ignored files.
- The same fixture proves that `%` includes ignored files and excludes `.git`.
- Tests cover hidden files, path-fragment queries, disappearing entries,
  internal symlinks, external symlinks, and cycles.
- A unit test proves an old query's late snapshot cannot replace newer results.

## 6. Phase 4: Language Registry and Parse Pipeline

### Work

- Add Tree-sitter and pinned grammar crates for C, C++, Rust, and Python.
- Map conventional extensions to a language adapter.
- Treat ambiguous C/C++ headers as C++ for the MVP.
- Decode supported source as UTF-8.
- Parse error-tolerantly and retain trees containing error nodes.
- Build line-start offsets once per parsed source.
- Return an explicit unsupported/binary/decode-failed state rather than a
  generic error.

### Verification

- Each supported extension selects the expected grammar.
- Unsupported extensions produce a whole-file-only result.
- Invalid UTF-8 disables parsing without panicking.
- A malformed fixture with valid declarations around an error produces a tree
  and reaches extraction.

## 7. Phase 5: Common Symbol Extraction

### Work

- Define grammar-specific Tree-sitter queries or visitors for each supported
  language.
- Extract the common declaration categories from the specification.
- Prefer the declaration identifier's range over its enclosing node's start.
- Build best-effort parent relationships and qualified-name segments.
- Exclude locals and parameters by construction, using ancestor context rather
  than name-based filtering.
- Preserve duplicate declarations as separate candidates.
- Provide symbol ranking that prefers exact leaf matches, prefixes, fuzzy leaf
  matches, and finally qualified-path matches as appropriate.

Implement and test one language fully before copying the adapter pattern to the
others. Rust is a good first adapter because its declarations and module/type
nesting provide broad coverage of the common model. C++ should be implemented
early enough to validate namespaces, nested records, and ambiguous names.

### Language Fixture Minimums

Each language fixture should contain:

- a top-level function;
- a named record-like type;
- a method or analogous nested callable;
- a field;
- an enum and enum member;
- a type alias;
- nested declarations where supported;
- duplicate leaf names under different parents;
- a local variable and parameter that must not be extracted; and
- at least one syntax error outside another valid declaration.

### Verification

- Snapshot tests list extracted symbols with kind, qualified name, and
  one-based identifier position.
- Tests prove leaf-only search finds a nested symbol.
- Tests prove qualified queries improve ranking.
- Tests prove duplicate leaf names remain independently selectable.
- Tests prove local variables and parameters are absent.

## 8. Phase 6: Parse Cache and Preview Provider

### Work

- Add a cache keyed by canonical path, size, and modification time.
- Cache decoded source, line starts, extracted symbols, and optionally the
  Tree-sitter tree.
- Bound the cache by entry count or approximate bytes.
- Invalidate cache entries when metadata changes.
- Build preview windows with five lines before and after a declaration start.
- Add identifier highlighting derived from byte ranges.
- Support vertical scrolling beyond the initial window and horizontal
  scrolling or deterministic truncation.
- Preserve the path as a sticky preview header.

### Verification

- Repeated symbol completion for an unchanged file hits the cache.
- Changing a file invalidates its cache entry.
- Beginning-of-file and end-of-file previews clamp correctly.
- Preview line numbers and highlight columns agree with emitted locations.
- Long lines and multiline syntax nodes do not corrupt layout state.

## 9. Phase 7: Composer and Structured References

### Work

- Implement an editable composer model independently of Ratatui widgets.
- Detect `@` and `%` only at valid token boundaries.
- Track completion state as a state machine:
  - inactive;
  - searching for a file;
  - file resolved;
  - searching for a symbol;
  - symbol resolved; and
  - stale or invalid.
- Store accepted references as structured spans associated with composer text.
- Update spans as text is inserted or deleted outside them.
- Invalidate a token when editing changes its display reference.
- Preserve a selected symbol ID/range even when the display text uses only its
  leaf name.
- Define a way to enter literal `@` and `%` text or dismiss unintended
  completion.

### Verification

- Table-driven tests cover token boundaries, punctuation, multiple references,
  Unicode surrounding text, edits before and after a reference, edits inside a
  reference, and dismissal.
- A duplicate leaf-name selection remains bound to the chosen candidate.
- `%path::symbol` cannot begin symbol search until `path` uniquely resolves.

## 10. Phase 8: Lowering and Stale-Reference Validation

### Work

- Implement lowering as a pure operation over composer text and structured
  reference spans.
- Convert file-only references to relative paths.
- Convert symbol references to one-based `path::line:column SymbolName`.
- Preserve all non-reference text.
- Validate file existence and metadata immediately before submission.
- Reparse stale symbol files and attempt to identify the selected declaration
  by stable descriptive fields such as kind, qualified name, leaf name, and
  nearby source identity.
- Block submission if re-resolution is missing or ambiguous.
- Write successful lowered text plus a newline directly to stdout, without
  quoting or JSON serialization.

The lowering module must not own terminal rendering. It should return either a
plain string or a structured validation error that lets the application focus
the affected token.

### Verification

- Golden tests cover prompts containing zero, one, and multiple references.
- Tests cover both search sigils and file-only versus symbol references.
- Tests prove punctuation adjacent to references is preserved.
- Tests prove output lines and columns are one-based.
- Tests prove stale references are relocated when unique and rejected when
  ambiguous or missing.
- A byte-for-byte test proves the output is unescaped plain text.

## 11. Phase 9: Ratatui REPL Integration

### Work

- Implement terminal setup and guaranteed restoration on normal exit, error,
  and panic where practical.
- Decide how to keep submitted stdout clean while rendering the TUI. Prefer a
  design that does not use stdout for both persistent output and control
  sequences, or explicitly suspend/restore rendering around submission.
- Render the composer, result list, status/transcript area, and preview.
- Connect file search to `@` and `%` composer states.
- Connect accepted files to on-demand parsing and symbol completion.
- Implement candidate navigation, acceptance, dismissal, submission, preview
  scrolling, and exit keys.
- Visually distinguish:
  - Git-aware versus broad search;
  - file versus symbol candidates;
  - resolved versus unresolved references; and
  - current versus stale references.
- Degrade to a compact layout in small terminals.
- Clear the composer after a successful submission and keep the REPL running.

### Verification

- Reducer/state tests cover key events without requiring a real terminal.
- Manual testing confirms terminal restoration after `Ctrl-C`, `Ctrl-D`, and a
  forced parse error.
- Manual testing confirms submitted stdout contains no ANSI control sequences.
- Manual testing covers an ignored C++ fixture and an unqualified nested symbol.

## 12. Phase 10: End-to-End MVP Validation

### Automated Checks

Run:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

Add an end-to-end harness around the application state and services where
possible. Full pseudo-terminal snapshot testing is optional for the MVP if it
would dominate implementation effort.

### Manual Acceptance Script

1. Create a Git fixture with tracked Rust and C++ files, an ignored generated
   directory, and duplicate nested symbol names.
2. Start the application from a subdirectory of that fixture.
3. Confirm the displayed search root is the Git worktree root.
4. Confirm `@` omits the ignored fixture.
5. Confirm `%` finds it using incomplete path segments.
6. Resolve the ignored file and enter `::` symbol completion.
7. Find a nested symbol by leaf name only.
8. Confirm the path header, qualified candidate label, kind, location, and
   five-before/five-after preview.
9. Scroll beyond the initial preview window.
10. Compose a sentence containing both a file-only and a symbol reference.
11. Submit and confirm stdout contains the expected plain paths and
    `path::line:column SymbolName` value with no control sequences or escaping.
12. Submit a second prompt without restarting the application.
13. Modify a selected source file before submission and confirm re-resolution
    or an actionable stale-reference state.

## 13. Implementation Risks and Mitigations

### Terminal output versus TUI rendering

Both typically use stdout. Mixing them could contaminate machine-consumable
prompt output with ANSI sequences. Isolate UI rendering from submitted output
and add a byte-level manual or pseudo-terminal check.

### Grammar-version drift

Tree-sitter node names and query compatibility can change. Pin grammar versions
in `Cargo.lock`, keep queries local to each adapter, and use extraction snapshot
tests as the compatibility boundary.

### C and C++ declaration complexity

Declarators can place identifiers deeply inside pointer, function, template,
and qualified nodes. Centralize “find declaration identifier” helpers per
language and accept documented omissions rather than approximating a compiler.

### Incorrect local-symbol inclusion

Broad queries can accidentally capture locals. Make extraction context-aware
and include negative fixtures for locals and parameters in every language.

### Broad-search resource use

`%` intentionally traverses ignored dependency and build trees. Keep it
cancellable, stream partial results, avoid parsing files during the walk, and
make the active mode obvious. Do not add speculative default exclusions beyond
`.git` without changing the specification.

### File mutation and stale positions

Modification timestamps can have coarse resolution. Include file size in cache
keys and consider a lightweight content hash when validating a selected symbol
at submission.

## 14. Completion Order

The recommended delivery order is:

1. bootstrap and discovery;
2. headless dual-mode file search;
3. headless parsing and extraction for all four languages;
4. preview and cache;
5. composer token model and lowering;
6. integrated TUI; and
7. end-to-end hardening.

Do not start with the full-screen interface. The search, extraction, reference,
and lowering layers should be demonstrably correct through headless tests before
terminal event handling is added.
