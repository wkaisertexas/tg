# Editor and File Lifecycle Design

## 1. Purpose

This document defines how `tg` behaves as a focused terminal editor. It covers
the file lifecycle, in-memory document model, Vim interaction, structured
reference spans, save rendering, clipboard output, and terminal cleanup.

## 2. Architectural Boundary

The editor shell owns text editing and delegates reference work. It must not
contain filesystem walking, Tree-sitter queries, GitHub parsing, Jira parsing,
or skill discovery.

The major interfaces are:

```text
Application
├── EditorSession       buffer, cursor, mode, history, registers
├── ReferenceSession    structured spans and active completion
├── CommandDispatcher   :w, :q, :wq, :copy
├── RenderState         viewport, gutter, overlay, status line
└── BackgroundRuntime   search, parsing, token counts, CLI providers
```

The application event loop remains responsible for restoring the terminal on
every success or error path.

## 3. File Lifecycle

### 3.1 Opening

An existing file is read as UTF-8. Invalid UTF-8 is rejected without entering
raw mode. New files begin with an empty buffer if their parent exists and is a
directory.

The session records:

- the logical path supplied by the caller;
- the canonical path when the file exists;
- original bytes or an original content hash;
- original permissions where available;
- last known metadata; and
- whether the file existed at startup.

Only one editable file is supported in version one.

### 3.2 External changes

Before a write, `tg` compares current file identity and content metadata with
the opening snapshot or most recent successful write. If another process has
changed the file, `tg` blocks the write and reports a conflict. It must not
silently overwrite external changes.

Version one may offer `:w!` to override a detected external change only if the
target path is unchanged and is still a regular file. This command is not
required for the first implementation increment.

### 3.3 Atomic write

Saving writes the lowered snapshot to a temporary file in the destination
directory, flushes it, preserves reasonable existing permissions, and renames
it over the destination. A failure before rename leaves the original intact.

After a successful write, the on-disk snapshot becomes the new clean baseline.
The visible friendly buffer may differ from the lowered on-disk text, so dirty
state is based on the last successfully rendered friendly document state, not
on a direct comparison between buffer text and disk text.

## 4. Document and Reference State

The editor widget provides text, cursor, selection, mode, undo/redo, search,
and registers. `tg` adds a document layer that associates accepted references
with stable ranges.

```rust
struct Document {
    editor: EditorState,
    references: Vec<ResolvedReference>,
    revision: u64,
    saved_revision: u64,
}

struct ResolvedReference {
    id: ReferenceId,
    range: TextRange,
    friendly_text: String,
    target: ReferenceTarget,
    source_version: Option<FileVersion>,
}
```

Ranges are character-based at the editor boundary. Tree-sitter and file I/O
may use byte offsets internally, but conversions must occur in a dedicated
UTF-8-safe layer.

Every text transaction maps reference ranges through insertions and deletions:

- an edit strictly before a reference shifts it;
- replacing the entire friendly text may reparse and retain it;
- editing inside a reference invalidates its structured identity and turns it
  into ordinary text; and
- deleting any part of a reference invalidates that reference unless the
  operation deletes the entire span.

Undo and redo restore both text and reference metadata. A plain text snapshot
alone is insufficient for history entries containing references.

## 5. Vim Surface

An embeddable Ratatui editor library should provide the first implementation.
`vimltui` is the preferred spike because it already reports explicit Save,
Close, ForceClose, and SaveAndClose actions and implements operator-motion
composition, visual modes, text objects, search, registers, and relative line
numbers. `edtui` is the fallback if the spike finds blocking correctness or
extension problems.

The required v1 surface is:

- Normal, Insert, Visual, Visual Line, and Visual Block modes;
- `h j k l`, `w W b B e E`, `0 ^ $`, `gg G`, and counted motions;
- `d c y`, doubled line forms, and composition with supported motions;
- `i I a A o O`, replace, join, indent, and case operations;
- `iw`, `aw`, and common quote/bracket text objects;
- unnamed and named registers, yank, paste, and delete registers;
- `/`, `?`, `n`, and `N`;
- `u`, redo, and `.`; and
- `:` command entry.

Compatibility is behavioral rather than exhaustive. Unsupported Vim commands
must be ignored or reported without modifying the document.

## 6. Commands

### `:w`

1. Finish command entry without modifying the document.
2. Validate and refresh structured references.
3. Render a lowered snapshot.
4. Atomically write it to the current file.
5. Mark the current friendly-document revision saved.
6. Remain in Normal mode.

`:w` fails for an unnamed buffer.

### `:wq`

Run `:w`; exit with status zero only if it succeeds.

### `:q`

Exit with status zero if no unsaved friendly-document changes exist. Otherwise
show `No write since last change` and remain open.

### `:q!`

Exit successfully without saving. The caller sees the last on-disk contents.

### `:copy`

Validate references, render the same lowered snapshot used by `:w`, and emit
it with OSC 52. Stay open, retain friendly references, and do not alter dirty
state. The status line briefly reports the copied byte and token counts.

### `:r !command`

Run the explicitly typed command through Bash off the UI thread and insert its
UTF-8 stdout below the current line as one undoable edit. Output is bounded,
execution times out, failures do not modify the document, and project
configuration cannot supply commands.

### Future commands

`:set`, filename-changing `:w path`, multiple buffers, and Vim configuration
sourcing are deferred. Product settings belong in TOML rather than a
Vimscript-compatible command language.

## 7. Lowered Snapshot Rendering

Rendering is a pure operation over an immutable document snapshot:

```text
friendly buffer + resolved reference records -> lowered UTF-8 string
```

References are validated in parallel where safe, then replacements are applied
from the end of the buffer toward the beginning. Ordinary text, whitespace,
and line endings are otherwise preserved.

Friendly text remains in memory after `:w`. This permits a user to continue
editing and to see which text is structured. If the same file is reopened,
only the previously lowered text is available; reference metadata is not stored
in a sidecar file.

## 8. Completion Interaction

Reference completion is active only in Insert mode after a configured leader
at a token boundary. While it is active:

- typing edits the query;
- `Up`/`Down` and `Ctrl-N`/`Ctrl-P` would normally conflict with preview, so
  result movement uses `Up`/`Down`, `Ctrl-J`/`Ctrl-K`, or configurable bindings;
- `Tab` or `Enter` accepts the selected candidate;
- `Esc` closes completion and returns to Normal mode according to normal Vim
  Insert-mode behavior;
- `Ctrl-P` toggles the preview pane as a `tg`-level binding; and
- stale background generations never replace newer results.

Because `Ctrl-P` is reserved for preview while completion is active, it does
not perform Vim keyword completion in that context. Outside completion it is
passed to the editor widget.

Accepting a candidate replaces the active query token with its canonical
friendly form and creates a structured reference record. The overlay closes.
Typing `::` after an accepted file explicitly starts the symbol stage.

## 9. Rendering

The normal screen contains only:

```text
  3  previous line
  2  previous line
  1  previous line
 42  current line
  1  next line
  2  next line
────────────────────────────────────────────────────────
 NORMAL  task.md [+]                         refs 18.7k
```

The current line number is absolute; other lines are relative. The gutter and
status line use low-contrast styles. Structured references may receive a
subtle style distinct from Markdown text, but styling must not change their
characters or occupy extra columns.

Completion is a bounded overlay near the cursor. If preview is enabled, the
overlay divides into a result list and preview area. On small terminals,
preview is suppressed before the editor viewport is reduced below a usable
minimum.

## 10. Terminal and Process Behavior

The UI renders to stderr or the controlling terminal so stdout remains usable
for diagnostics or future scripting. External-editor operation does not print
the prompt to stdout; the calling agent reads the edited file.

Terminal setup and teardown use a guard so raw mode, alternate screen, bracketed
paste, and cursor visibility are restored on normal exit, error, and panic.
Signals should request a clean shutdown where possible.

The process exits nonzero only for initialization failures, unrecoverable I/O,
or terminal failures. A user `:q!` is a successful editor exit.

## 11. Verification

Tests must cover:

- opening existing and new files;
- atomic saves and permission preservation;
- external-change conflicts;
- dirty state when friendly and lowered snapshots differ;
- reference range mapping through edits, undo, and redo;
- every required command action;
- `:copy` equality with the `:w` rendered snapshot;
- multibyte Unicode around reference spans;
- terminal restoration after injected failures; and
- a pseudo-terminal integration in which a parent process invokes `tg FILE`,
  waits, and reads the written contents.
