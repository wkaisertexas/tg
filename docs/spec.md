# Symbol-Aware File Selector TUI Specification

## 1. Status

This document specifies an MVP for a standalone Rust terminal application that
provides coding-agent-style text composition, fuzzy file selection, and
Tree-sitter-backed symbol selection.

The application is a proof of concept. It optimizes for a useful, concise way
to refer to source code, not for compiler-accurate name resolution. It does not
invoke an AI model, compile the selected project, run an LSP server, or attempt
semantic analysis across files.

An implementation plan derived from this specification lives in
[`impl-plan.md`](impl-plan.md).

## 2. Problem Statement

Coding-agent prompts often contain file references. Existing `@` completion
usually stops at the file boundary, which makes referring to a specific type,
function, or declaration cumbersome. Users must either describe the symbol in
prose or look up and type a line number manually.

This application adds a second completion stage. After choosing a file, the
user may choose a symbol extracted from that file's syntax tree. The friendly
symbol reference is converted into the line-and-column form that a coding
agent can consume directly.

The application also provides an explicit way to search files hidden by Git
ignore rules. This is important for local fixtures, generated test data,
scratch files, and other intentionally untracked content.

## 3. Goals

The MVP must:

- Present a persistent, coding-agent-like text composer in a terminal.
- Accept one root-directory argument and discover its containing Git
  repository automatically.
- Fuzzy-search ordinary project files with `@`.
- Fuzzy-search a broader filesystem view, including Git-ignored files, with
  `%`.
- Parse a resolved C, C++, Rust, or Python file with Tree-sitter.
- Complete useful non-local symbols after the file has been resolved.
- Permit concise leaf-name references to nested symbols without requiring the
  full namespace or enclosing-type path.
- Convert selected symbols to one-based `path::line:column SymbolName` locations when the
  user submits the prompt.
- Show a scrollable source preview so the user can verify the selected
  location.
- Print the lowered prompt as ordinary, unescaped text without JSON framing.

## 4. Non-Goals

The MVP does not:

- Contact or embed an AI model.
- Compile or type-check the project.
- Start or emulate an LSP server.
- Resolve imports, includes, aliases, inheritance, macros, or references
  between files.
- Guarantee that a symbol can be referenced according to the source
  language's name-resolution rules.
- Index symbol names across the whole repository.
- Search deleted files or older revisions from Git history.
- Extract local variables, function parameters, or anonymous syntax nodes.
- Promise a latency, memory, or repository-size target.
- Persist an index between application runs.

## 5. Terminology

- **Invocation root**: the directory passed as the application's sole
  positional argument.
- **repository root**: the top-level directory of the Git worktree containing
  the invocation root.
- **search root**: the repository root when Git discovery succeeds, otherwise
  the invocation root.
- **file token**: a resolved reference to a file, originally introduced by `@`
  or `%`.
- **symbol token**: a file token extended with `::` and a selected syntax
  symbol.
- **display reference**: the friendly reference shown in the composer, such as
  `@src/model.rs::User`.
- **lowered reference**: the plain path or source location emitted on prompt
  submission.

## 6. Command-Line Interface and Lifecycle

The initial interface is:

```text
tscodeselection <root-folder>
```

The program must canonicalize the invocation root and reject a missing,
unreadable, or non-directory argument with a useful error.

Starting at the invocation root, it must walk upward to find the nearest Git
worktree root. The search root is that repository root. If no Git worktree is
found, the invocation root becomes the search root and the UI must indicate
that Git-aware filtering is unavailable.

The program operates as a REPL:

1. The user edits a prompt in the composer.
2. Completion popups and previews appear as references are entered.
3. Submitting a prompt lowers all resolved references.
4. The lowered prompt is written to standard output followed by a newline,
   with behavior equivalent to printing the resulting string with `printf`.
5. The composer is cleared and remains ready for another prompt.

Output must not be JSON-formatted, shell-escaped, quoted, or otherwise encoded.
The application must not print diagnostic or UI control text to the same
logical output stream as submitted prompts. The implementation may use stderr
or an internal UI region for diagnostics.

`Ctrl-C` cancels the active completion or clears the composer before exiting on
a subsequent `Ctrl-C`. `Ctrl-D` on an empty composer exits successfully.

## 7. Composer and Completion Interaction

### 7.1 General Interaction

The main UI contains:

- a transcript or status region;
- a multiline-capable text composer;
- a completion-results list when completion is active; and
- a scrollable source-preview region when a file or symbol can be previewed.

The composer behaves like the input area of a terminal coding agent. Text not
belonging to a reference is preserved exactly, subject only to the terminal's
normal text-input behavior.

When no completion list is active, `Enter` submits the prompt. When a completion
list is active, `Enter` accepts the highlighted candidate and does not submit
the prompt. A separate, documented key such as `Ctrl-Enter` may submit while a
completion list is visible if the terminal backend can detect it reliably.

`Up` and `Down` change the highlighted candidate. `Esc` closes the active
completion without deleting already typed text. The final implementation may
add Vim-like navigation, but it is not required.

### 7.2 File Completion

Typing `@` at a token boundary begins Git-aware file completion. Typing `%` at
a token boundary begins broad file completion. Characters after the sigil form
the fuzzy path query until the token ends or symbol completion begins.

Matching is performed against search-root-relative paths, not just basenames.
Therefore a query may contain a filename fragment, one or more path fragments,
or both. Matching is case-insensitive by default, with ranking delegated to a
well-established fuzzy-matching library.

The results list displays at least:

- the search-root-relative path;
- whether the result came from ordinary or broad search; and
- enough highlighting to show which query characters matched.

Accepting a file result inserts a canonical display reference using `/` as the
display separator:

```text
@src/parser/ast.rs
%fixtures/generated/case.cpp
```

The sigil records how the user found the file but does not become part of the
lowered output.

### 7.3 Symbol Completion

Symbol completion is available only after a file path has resolved to one
specific current filesystem entry. It begins when the user types `::` after a
resolved file token or explicitly requests symbol completion for the
highlighted file.

Examples:

```text
@include/widget.h::Widget
@src/parser.rs::parse_item
%generated/test_case.cpp::Fixture
```

The text after `::` fuzzy-matches extracted symbol names. The full qualified
path is not required. For example, either of the following may identify a
nested structure:

```text
@include/widget.h::outer::inner::Record
@include/widget.h::Record
```

The shorter form searches symbol leaf names anywhere in the selected file.
This behavior is deliberately looser than compiler name resolution.

If a query matches more than one symbol, the completion list must show each
candidate's:

- leaf name;
- best-effort qualified name;
- symbol kind; and
- one-based line and column.

The user resolves ambiguity by selecting a candidate. The internal token must
retain the chosen symbol's identity and source range even when the visible
display reference uses only the leaf name. Merely typing an ambiguous name is
not sufficient to create a resolved symbol token.

Nested completion may expose both direct children and deeper descendants. The
ranking should prefer exact leaf-name matches, then prefix matches, then fuzzy
matches. A fully or partially qualified query should rank matching ancestor
segments above unrelated leaf-only matches.

## 8. File Search Semantics

### 8.1 `@`: Git-Aware Search

`@` searches the current working tree under the search root. It includes:

- tracked files;
- untracked files not excluded by applicable ignore rules; and
- hidden files not otherwise ignored.

It respects repository `.gitignore` files, `.git/info/exclude`, applicable
global Git ignore rules, and conventional `.ignore` files when supported by the
chosen walker.

It does not search historical commits. “Git-aware” describes filtering and
repository-root discovery, not revision-history search.

### 8.2 `%`: Broad Filesystem Search

`%` searches current filesystem entries under the search root without applying
Git ignore rules. It is intentionally allowed to be slower than `@`.

The MVP's mandatory exclusion list is deliberately small:

```text
.git/
```

Directories such as `target`, `node_modules`, generated trees, caches, and
ignored fixture directories are not excluded by default. This permits users to
find files in inconvenient but known paths. Future configuration may add
user-defined exclusions, but it is not required for the MVP.

The UI must visibly distinguish `%` mode because it can expose ignored files,
local configuration, generated artifacts, and other content that ordinary
search hides.

### 8.3 Common Walking Rules

Both modes:

- return regular files, not directories, as selectable final results;
- use paths relative to the search root;
- skip `.git` metadata;
- do not traverse a symlink whose canonical target is outside the search root;
- avoid duplicate entries and symlink cycles;
- tolerate unreadable entries by reporting a nonfatal diagnostic; and
- may list unsupported, binary, or very large files for whole-file selection.

Binary files and files without a supported grammar do not offer symbol
completion.

## 9. Language and Symbol Model

### 9.1 Supported Languages

The MVP supports:

- C;
- C++;
- Rust; and
- Python.

Language selection is based primarily on file extension, with conventional
extensions for these languages. Header files that are ambiguous between C and
C++ should default to C++ parsing for the MVP, because C++ grammar generally
provides the more useful declaration surface for the intended use case.

Unsupported extensions remain available as whole-file references.

### 9.2 Extracted Symbols

Each language adapter maps its grammar-specific syntax nodes into a small
common model. Where the language supports an analogous construct, the adapter
should extract:

- namespace or module declarations;
- classes, structs, unions, enums, traits, and interfaces;
- functions and methods;
- named type aliases;
- named top-level constants and static variables;
- enum members; and
- named fields or members of a type.

The MVP excludes:

- local variables;
- parameters;
- imports, includes, and use declarations;
- anonymous declarations;
- arbitrary expressions and statements;
- macro invocations and macro-generated declarations; and
- semantic aliases or declarations that exist only after compilation.

Language adapters may intentionally omit difficult edge cases. A partially
correct, predictable declaration index is preferable to compiler emulation.

### 9.3 Common Symbol Record

A parsed symbol has at least:

```text
Symbol {
  leaf_name
  qualified_name_segments
  kind
  declaration_start_byte
  declaration_end_byte
  start_line
  start_column
  end_line
  end_column
  parent_symbol_id?
}
```

Lines and columns stored internally may use Tree-sitter's zero-based points,
but all user-visible and emitted locations are one-based. Columns count bytes,
matching Tree-sitter's point representation, and the UI must document this
choice. Supporting Unicode scalar-value or display-cell columns is a possible
future improvement.

The location of a symbol is the beginning of its best declaration identifier,
not merely the beginning of the enclosing syntax node. For example, a C++
struct should point at the struct's name when the grammar exposes that range.
If the identifier range cannot be recovered, the adapter may fall back to the
declaration node's start.

### 9.4 Error-Tolerant Parsing

Tree-sitter trees containing error or missing nodes are not fatal. The adapter
must extract recognizable symbols from valid portions of the tree. A complete
parse failure disables symbol completion for that file while leaving whole-file
selection available.

Parsing occurs against the current on-disk contents. No compiler flags,
preprocessor configuration, Python environment, or Rust feature selection is
consulted.

## 10. Reference Representation and Lowering

### 10.1 Internal Representation

Resolved references in the composer must be structured tokens, not plain text
that is reparsed only at submission time. A resolved token records:

- its span in the composer;
- search mode (`@` or `%`);
- canonical absolute path;
- search-root-relative display path;
- file metadata sufficient to detect a change; and
- optional selected symbol metadata and source range.

Editing inside a resolved token invalidates or re-resolves it. The UI must make
unresolved and stale references visibly different from resolved references.

### 10.2 Lowering Rules

On submission, ordinary prompt text is preserved and each resolved reference is
replaced independently:

| Displayed token | Lowered text |
| --- | --- |
| `@src/model.rs` | `src/model.rs` |
| `%fixtures/output.py` | `fixtures/output.py` |
| `@src/model.rs::User` | `src/model.rs::12:8 User` |
| `%gen/case.cpp::Fixture` | `gen/case.cpp::41:3 Fixture` |

All paths are relative to the search root and use `/` separators. Lines and
columns are one-based decimal integers. A symbol reference emits the selected
declaration identifier's start line and column.

No source excerpt, symbol name, sigil, quoting, escaping, JSON, or metadata is
added to the lowered prompt.

Example:

```text
Composer:
Compare @src/old.rs::Parser with %generated/new_parser.cpp::Parser.

Standard output:
Compare src/old.rs::18:12 Parser with generated/new_parser.cpp::73:7 Parser.
```

### 10.3 Validation at Submission

Before lowering, the application checks that each referenced file still exists
and has not changed since its symbol was parsed. A file-only token can be
lowered if the file still exists. A stale symbol token must be reparsed and
matched to its selected symbol.

If a unique re-resolution succeeds, its new location is used. If it fails or
becomes ambiguous, submission is blocked, the token is marked stale, and the
user is returned to symbol selection. The program must not silently emit a
known-invalid location.

Unresolved sigil text is ordinary text only if the user explicitly dismisses
completion or escapes the sigil. Otherwise submission should warn that a
reference-like token remains unresolved and require confirmation or resolution.

## 11. Source Preview

When a file result is highlighted, the preview shows the file path as a header
and a useful initial portion of the file. When a symbol result is highlighted,
the preview centers on the selected declaration identifier.

The default symbol preview contains:

- up to five source lines before the symbol's start line;
- the symbol's start line;
- up to five source lines after the symbol's start line;
- one-based line numbers; and
- visual highlighting for the declaration identifier and, where useful, the
  selected AST node's extent.

Thus the initial symbol preview is at most eleven lines. The preview is
scrollable beyond this window so the user can inspect surrounding code. The
path header remains visible while scrolling. Horizontal scrolling or a clear
truncation treatment must be provided for long lines.

Preview content exists only to confirm resolution. It is not included in the
lowered standard-output prompt.

If the source cannot be decoded as UTF-8, the preview reports that it is
unavailable and symbol parsing is disabled for the MVP.

## 12. Runtime Architecture

The program should be divided into components with explicit boundaries:

1. **Application state and event loop** owns REPL state, terminal events, and
   rendering.
2. **Composer model** stores text plus structured reference spans and performs
   lowering.
3. **Repository discovery** resolves the invocation root, Git worktree, and
   search root.
4. **File search service** walks paths and incrementally updates fuzzy matches
   for `@` and `%` modes.
5. **Language registry** maps files to supported Tree-sitter parsers and symbol
   adapters.
6. **Symbol extractor** converts language-specific trees into the common symbol
   model.
7. **Parse cache** retains recently parsed files in memory.
8. **Preview provider** reads source windows and maps byte ranges to displayed
   lines.
9. **Submission renderer** validates references and produces plain output.

Filesystem walking and parsing must not block terminal input or rendering.
Results should be delivered through channels or asynchronous tasks and tagged
with a query generation so stale results can be discarded.

## 13. Caching Strategy

The MVP parses symbols only after a path has resolved or a highlighted file
needs a symbol preview. It does not eagerly create a repository-wide symbol
index. Consequently, queries of the form `%<file-fragment>::<symbol-fragment>`
must first resolve the file portion before symbol matching begins.

Parsed file data should be cached in memory using a key containing at least:

- canonical path;
- file size; and
- modification timestamp.

The cache value contains decoded source text, the Tree-sitter tree or extracted
symbol records, and line-start offsets used for previews. A bounded least
recently used cache is recommended, but exact capacity and eviction behavior
are implementation choices rather than product requirements.

The architecture should leave room for a later opt-in eager mode that indexes
all supported files and permits combined path-and-symbol search. That mode is
outside the MVP and must not complicate the initial user syntax.

## 14. Error Handling

The UI must handle these cases without crashing:

- the root is not a Git repository;
- entries disappear during a walk;
- files or directories are unreadable;
- a file changes while selected;
- a file is unsupported, binary, or invalid UTF-8;
- Tree-sitter returns an error-containing tree;
- no symbols are found;
- two symbols have the same leaf and qualified name;
- a broad search traverses a large ignored tree; and
- terminal dimensions are too small for the normal layout.

Fatal initialization errors go to stderr and cause a nonzero exit. Nonfatal
search and parse errors appear in the TUI status area and preserve the user's
composer contents.

## 15. Security and Privacy Considerations

Broad `%` search is an explicit opt-in to enumerate ignored content. The UI
must communicate the active mode and must not automatically preview a file
until it becomes the highlighted result. `.git` is never traversed.

The application performs only local reads. It does not transmit file names,
source code, prompts, or telemetry. Submitted text is written locally to
standard output.

Symlinks must not be used to escape the search root. Paths in output must remain
search-root-relative and must not contain unresolved `..` traversal.

## 16. Recommended Rust Dependencies

The implementation may use established libraries freely. The expected stack is:

- `ratatui` for layout and rendering;
- `crossterm` for terminal input and terminal-state management;
- `ignore` for parallel, Git-aware and ignore-disabled filesystem walking;
- `nucleo` for fuzzy matching;
- `clap` for argument parsing;
- `tree-sitter` plus C, C++, Rust, and Python grammar crates;
- `git2` or a small Git command wrapper for worktree discovery;
- `crossbeam-channel`, Tokio channels, or equivalent for background work; and
- a small cache library or a local bounded-cache implementation.

OpenAI Codex is a design reference for using an `ignore::WalkBuilder`, a
long-lived fuzzy-search session, query updates on each keystroke, and `nucleo`
ranking. This project need not copy Codex internals or match its UI exactly.

Relevant upstream references:

- <https://github.com/openai/codex/blob/main/codex-rs/file-search/src/lib.rs>
- <https://github.com/openai/codex/blob/main/codex-rs/tui/src/file_search.rs>

## 17. MVP Acceptance Criteria

The MVP is complete when all of the following can be demonstrated:

1. Starting the program with a directory inside a Git worktree discovers and
   displays the correct repository root.
2. Typing `@` returns tracked and non-ignored untracked files while omitting an
   ignored fixture.
3. Typing `%` returns that ignored fixture while never returning `.git`
   contents.
4. Path-fragment fuzzy matching can find a deeply nested file without typing
   its complete path.
5. A resolved C, C++, Rust, or Python file offers the expected top-level and
   nested non-local symbols.
6. Local variables and function parameters do not appear as symbols.
7. A nested symbol can be found by its leaf name without typing its enclosing
   namespaces or types.
8. Duplicate leaf names are displayed with enough qualification and source
   position to select the intended declaration.
9. Selecting a symbol shows its file path and a scrollable preview initially
   containing five lines before and five lines after its declaration line.
10. Submitting a prompt converts symbol tokens to one-based
    `path::line:column SymbolName`, converts file-only tokens to paths, preserves surrounding
    prompt text, and writes one unescaped line to standard output.
11. Editing a referenced file before submission triggers re-resolution or
    blocks submission rather than emitting a stale location.
12. A malformed but partially parseable source file still exposes symbols from
    recognizable portions of its syntax tree.
13. An unsupported or non-UTF-8 file remains selectable as a whole file without
    crashing the application.
14. After submission, the application clears the composer and accepts another
    prompt until explicitly exited.

## 18. Deferred Extensions

Potential follow-up work includes:

- eager in-memory parsing of every supported file;
- combined fuzzy path-and-symbol queries before a file uniquely resolves;
- incremental Tree-sitter edits for files changing during a session;
- configurable broad-search exclusions;
- additional language adapters;
- field, macro, and language-specific symbol filters;
- range output in addition to point output;
- persistent indexes;
- historical Git revision search; and
- integration as a library or subprocess for an actual coding agent.
