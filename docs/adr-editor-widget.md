# ADR: Editor Widget Foundation

## Status

Accepted for the editor spike: use `edtui` as the widget foundation and keep
document history, Ex commands, named registers, and unsupported Vim operations
in a `tg`-owned adapter. Do not integrate `vimltui` 0.2.11.

This decision selects a foundation, not a claim that `edtui` is a complete
drop-in implementation of the required Vim surface. The adapter gaps below are
release work and must have behavioral tests before the editor gate closes.

## Context

[`editor-design.md`](editor-design.md) prefers a `vimltui` spike and names
`edtui` as the fallback when correctness or extension problems block the first
choice. The important constraints are exact UTF-8 text preservation,
character-based reference ranges, Normal/Insert/Visual Line/Visual Block
behavior, structured-reference styling, application-owned commands, and undo
history that restores text and reference metadata together.

The repository currently uses Crossterm 0.29 and Ratatui 0.29. `vimltui`
0.2.11 uses Crossterm 0.29 and Ratatui 0.30. `edtui` 0.11.7 uses Crossterm
0.29 and the Ratatui 0.30 split crates (`ratatui-core` 0.1 and
`ratatui-widgets` 0.3). Either integration therefore requires one coordinated
Ratatui upgrade; allowing both Ratatui type families in application code is
not acceptable.

## Spike evidence

An isolated scratch crate under `/tmp` exercised both published crates without
changing this repository's dependencies.

### `vimltui` 0.2.11

Useful API surface:

- `VimMode` distinguishes Normal, Insert, Replace, and character/line/block
  Visual modes.
- `EditorAction` reports `Save`, `Close`, `ForceClose`, and `SaveAndClose`.
- Operator-motion composition, text objects, search, dot repeat, indentation,
  case operations, and relative line numbers are implemented.

Blocking behavior:

1. `VimEditor::new` and `set_content` replace every tab with four spaces.
2. Input is split with `str::lines()` and reconstructed with `join("\n")`, so
   a terminal newline and original line-ending form are lost.
3. Cursor columns are inconsistent. `current_line_len()` returns UTF-8 byte
   length, basic motions increment that value as a column, while several
   operations interpret it as a character index and others pass it directly
   to `String::insert`, `remove`, or slicing.
4. The scratch sequence `l`, `l`, `i`, `X` on `a🦀b` panics because insertion
   is attempted at a byte offset inside the crab emoji.
5. `Snapshot` contains only lines and cursor coordinates. Reference metadata
   cannot participate in native undo/redo.
6. The command parser recognizes `w`, `q`, `q!`, and `wq`, but consumes unknown
   commands as `Handled`; `:copy` cannot be intercepted after dispatch.
7. The `SyntaxHighlighter` callback receives line text without a row identity,
   so it cannot reliably style arbitrary document reference ranges.
8. The apparent named-register prefix consumes the register character but the
   editor stores only `unnamed_register`; it does not implement the required
   named text registers.
9. `Ctrl-P` is consumed as `Handled`, so application routing must occur before
   the widget even for the completion-preview binding.

The Unicode defect is systemic across motion, insertion, search, and visual
code, rather than an isolated conversion at the API boundary. A local patch
would amount to auditing the editor implementation and is too risky for the
editor gate.

### `edtui` 0.11.7

Useful API surface:

- `Lines` stores characters and the scratch Unicode edit completed without a
  panic or corruption.
- Tabs and a terminal newline round-trip through `Lines::from` and
  `Lines::to_string`.
- `EditorView::line_numbers(LineNumbers::Relative)` renders the current row as
  its absolute one-based number and other rows as cursor-relative distances.
- `EditorState::set_highlights(Vec<Highlight>)` accepts explicit character-row
  and character-column ranges, which can style structured references.
- `EditorEventHandler::on_event` handles Crossterm bracketed-paste events and
  inserts their text through the character-based buffer.
- The default Vim map includes `/`, `n`, `N`, `u`, redo, dot repeat,
  operator-motion commands, common inner text objects, and `V` line
  selection. Selection state records whether it is linewise.
- Buffer, cursor, mode, selection, and highlights are publicly observable or
  replaceable, allowing a `tg` document adapter to own snapshots.

Missing or unsuitable surface:

- There is no Visual Block representation or behavior.
- There is no Ex command-line mode or save/close action protocol.
- Named text registers are not implemented.
- Indent/dedent and case operators are not part of the default Vim map.
- Native undo/redo stacks are private and contain text/cursor state, not
  reference metadata.
- The widget has one `EditorMode::Visual` value; linewise behavior is stored in
  the selection rather than represented as a distinct mode.

These gaps are substantial, but they can be isolated above a character-safe
buffer instead of repairing pervasive UTF-8 invariants inside the widget.

## Decision

Use `edtui` 0.11.7 with default features disabled initially. Enable only the
features proven necessary; system clipboard integration is not needed because
`:copy` uses the existing OSC 52 implementation.

Create a `tg`-owned `EditorSession` adapter with these boundaries:

1. Route completion keys and `Ctrl-P` before the widget.
2. Own `:` command entry and dispatch `:w`, `:q`, `:q!`, `:wq`, and `:copy`.
   Do not send command-line keystrokes into `edtui`.
3. Own compound document snapshots containing `Lines`, cursor, selection,
   resolved references, revision, and saved revision. Intercept `u` and redo
   and restore these snapshots rather than invoking `edtui`'s private history.
4. Compare the before/after public buffer around dispatched widget events and
   commit a document transaction only when text changed. Insert-mode edits
   must be grouped into one transaction from entry through Escape.
5. Rebuild `EditorState` highlights from resolved character ranges after every
   transaction, undo, and redo.
6. Add an adapter-owned Visual Block state and operations. Character and
   linewise Visual behavior may delegate to `edtui`; block selection, render,
   yank/delete/change, insert/append, and paste require project tests.
7. Add adapter-owned named/unnamed register storage and intercept register
   prefixes plus yank/delete/paste operations that must use it.
8. Add missing indent, dedent, uppercase, lowercase, and toggle-case operators
   as adapter transactions over character ranges.
9. Pass ordinary keys to `edtui` only when none of the application-owned states
   above is active.

## Integration gate

Before adding `edtui` to production dependencies, a tracked adapter prototype
must prove:

- tabs, terminal newlines, multiline Unicode, and bracketed paste round-trip;
- current-absolute plus other-relative gutter rendering;
- reference highlights use character ranges and occupy no extra columns;
- all five required Ex commands are intercepted without buffer changes;
- `Ctrl-P` toggles preview only during completion and otherwise reaches the
  widget;
- character, line, and block Visual edits work;
- named and unnamed registers work for yank, delete, and paste;
- undo/redo restore text and reference metadata together; and
- a new edit after undo clears the project-owned redo history.

If the adapter prototype cannot implement Visual Block and compound history
without duplicating most of an editor, stop and reassess a small project-owned
editor core. Do not fall back to `vimltui` 0.2.11 without an upstream release
that fixes exact text preservation and character-safe cursor invariants.

## Consequences

The project loses `vimltui`'s broader ready-made Vim surface and must implement
several operations. In exchange, the foundational buffer is character-safe,
reference highlighting is expressible, paste events are supported, and the
application can own the history and command semantics required for durable
structured references.
