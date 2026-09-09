//! Character-safe adapter around `edtui`.
//!
//! The adapter deliberately owns application key routing and public snapshots.
//! Project history, Visual Block operations, registers, and missing text
//! operators live here instead of depending on edtui's private state.

mod block;
mod display;
mod input;
mod operators;
mod registers;
#[cfg(test)]
mod tests;

use crate::config::{EditorConfig, LineNumbers as ConfigLineNumbers};
use crate::references::model::TextRange;
use anyhow::{Context, Result, ensure};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use edtui::{EditorEventHandler, EditorMode, EditorState, Index2, LineNumbers, Lines};
use input::parse_key_binding;
use ratatui::style::{Color, Style};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterMode {
    Normal,
    Insert,
    Replace,
    VisualCharacter,
    VisualLine,
    VisualBlock,
    Search,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextOperator {
    Indent,
    Dedent,
    Uppercase,
    Lowercase,
    ToggleCase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorInput {
    Delegated {
        text_changed: bool,
    },
    TogglePreview,
    CommandStarted,
    CommandUpdated,
    /// Raw command text for the application-owned Ex dispatcher.
    CommandSubmitted(String),
    CommandCancelled,
    Undo,
    Redo,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RegisterValue {
    Character(String),
    Line(String),
    Block(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockSelection {
    anchor: Index2,
    cursor: Index2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockInsert {
    rows: Vec<usize>,
    column: usize,
    inserted_chars: usize,
}

/// Public widget state used by the project-owned compound history.
#[derive(Clone)]
pub struct EditorSnapshot {
    text: String,
    state: EditorState,
    viewport: (usize, usize),
    reference_ranges: Vec<TextRange>,
    block: Option<BlockSelection>,
}

impl EditorSnapshot {
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn cursor(&self) -> Index2 {
        self.state.cursor
    }
    pub fn reference_ranges(&self) -> &[TextRange] {
        &self.reference_ranges
    }
}

pub struct EditorSession {
    state: EditorState,
    events: EditorEventHandler,
    command_line: Option<String>,
    reference_ranges: Vec<TextRange>,
    reference_style: Style,
    focused: bool,
    block: Option<BlockSelection>,
    block_insert: Option<BlockInsert>,
    registers: HashMap<char, RegisterValue>,
    register_prefix: bool,
    selected_register: char,
    line_numbers: LineNumbers,
    current_line_absolute: bool,
    tab_width: usize,
    wrap: bool,
    preview_toggle: KeyBinding,
    count: usize,
    operator_count: usize,
    pending_operator: Option<char>,
    pending_text_object: Option<char>,
    pending_replace: bool,
    replace_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyBinding {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl EditorSession {
    pub fn new(text: impl AsRef<str>) -> Self {
        Self {
            state: EditorState::new(Lines::from(text.as_ref())),
            events: EditorEventHandler::vim_mode(),
            command_line: None,
            reference_ranges: Vec::new(),
            reference_style: Style::default().fg(Color::Cyan),
            focused: true,
            block: None,
            block_insert: None,
            registers: HashMap::new(),
            register_prefix: false,
            selected_register: '"',
            line_numbers: LineNumbers::Relative,
            current_line_absolute: true,
            tab_width: 4,
            wrap: true,
            preview_toggle: KeyBinding {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
            },
            count: 0,
            operator_count: 0,
            pending_operator: None,
            pending_text_object: None,
            pending_replace: false,
            replace_mode: false,
        }
    }

    pub fn configure(&mut self, editor: &EditorConfig, preview_toggle: &str) -> Result<()> {
        self.line_numbers = match editor.line_numbers {
            ConfigLineNumbers::Relative => LineNumbers::Relative,
            ConfigLineNumbers::Absolute => LineNumbers::Absolute,
            ConfigLineNumbers::None => LineNumbers::None,
        };
        self.current_line_absolute = editor.current_line_absolute;
        self.tab_width = usize::from(editor.tab_width);
        self.wrap = editor.wrap;
        self.preview_toggle = parse_key_binding(preview_toggle)?;
        Ok(())
    }

    pub fn text(&self) -> String {
        self.state.lines.to_string()
    }
    pub fn state(&self) -> &EditorState {
        &self.state
    }
    pub fn command_line(&self) -> Option<&str> {
        self.command_line.as_deref()
    }

    pub fn mode(&self) -> AdapterMode {
        if self.command_line.is_some() {
            return AdapterMode::Command;
        }
        if self.block.is_some() {
            return AdapterMode::VisualBlock;
        }
        if self.replace_mode {
            return AdapterMode::Replace;
        }
        match self.state.mode {
            EditorMode::Normal => AdapterMode::Normal,
            EditorMode::Insert => AdapterMode::Insert,
            EditorMode::Visual => {
                if self
                    .state
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.line_mode)
                {
                    AdapterMode::VisualLine
                } else {
                    AdapterMode::VisualCharacter
                }
            }
            EditorMode::Search => AdapterMode::Search,
        }
    }

    pub fn cursor_char_offset(&self) -> usize {
        position_to_char_index(&self.text(), self.state.cursor)
            .expect("editor cursor must remain inside the buffer")
    }

    pub fn set_cursor_char_offset(&mut self, offset: usize) -> Result<()> {
        let text = self.text();
        let position = char_index_to_position(&text, offset)
            .ok_or_else(|| anyhow::anyhow!("cursor offset is outside the editor buffer"))?;
        validate_position(&text, position)?;
        self.state.cursor = position;
        if let Some(block) = &mut self.block {
            block.cursor = position;
        }
        self.rebuild_highlights()
    }

    pub fn focus(&mut self) {
        self.focused = true;
    }
    pub fn blur(&mut self) {
        self.focused = false;
    }
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            text: self.text(),
            state: self.state.clone(),
            viewport: self.state.viewport_offset(),
            reference_ranges: self.reference_ranges.clone(),
            block: self.block,
        }
    }

    /// Restores public widget state while resetting edtui's private history.
    /// The project history owns undo/redo and calls this hook with its compound
    /// text/reference snapshot.
    pub fn restore(&mut self, snapshot: &EditorSnapshot) -> Result<()> {
        validate_position(&snapshot.text, snapshot.state.cursor)?;
        let mut state = EditorState::new(Lines::from(snapshot.text.as_str()));
        state.cursor = snapshot.state.cursor;
        state.mode = snapshot.state.mode;
        state.selection = snapshot.state.selection.clone();
        state.set_viewport_offset(snapshot.viewport.0, snapshot.viewport.1);
        self.state = state;
        self.events = EditorEventHandler::vim_mode();
        self.command_line = None;
        self.block = snapshot.block;
        self.block_insert = None;
        self.reset_pending_input();
        self.set_reference_ranges(snapshot.reference_ranges.clone())
    }

    pub fn replace_text_and_ranges(
        &mut self,
        text: impl AsRef<str>,
        ranges: impl IntoIterator<Item = TextRange>,
    ) -> Result<()> {
        let cursor = self.cursor_char_offset().min(text.as_ref().chars().count());
        let mode = self.state.mode;
        self.state = EditorState::new(Lines::from(text.as_ref()));
        self.state.mode = mode;
        self.events = EditorEventHandler::vim_mode();
        self.command_line = None;
        self.block = None;
        self.block_insert = None;
        self.reset_pending_input();
        self.reference_ranges.clear();
        self.state.clear_highlights();
        self.set_cursor_char_offset(cursor)?;
        self.set_reference_ranges(ranges)
    }

    fn replace_char_range(
        &mut self,
        range: TextRange,
        replacement: &str,
        cursor_offset: usize,
        insert: bool,
    ) {
        let mut chars: Vec<_> = self.text().chars().collect();
        chars.splice(range.start..range.end, replacement.chars());
        let text: String = chars.into_iter().collect();
        let cursor =
            char_index_to_position(&text, cursor_offset.min(text.chars().count())).unwrap();
        self.replace_all_text(text, cursor, insert);
    }

    fn replace_rows(&mut self, rows: Vec<Vec<char>>, cursor: Index2, insert: bool) {
        let text = rows
            .into_iter()
            .map(|row| row.into_iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        self.replace_all_text(text, cursor, insert);
    }

    fn replace_all_text(&mut self, text: String, cursor: Index2, insert: bool) {
        let block_insert = self.block_insert.clone();
        self.state = EditorState::new(Lines::from(text));
        self.state.cursor = clamp_position(&self.state.lines, cursor);
        self.state.mode = if insert {
            EditorMode::Insert
        } else {
            EditorMode::Normal
        };
        self.block_insert = block_insert;
        self.reference_ranges.clear();
        self.state.clear_highlights();
    }
}

impl Default for EditorSession {
    fn default() -> Self {
        Self::new("")
    }
}

fn char_index_to_position(text: &str, index: usize) -> Option<Index2> {
    if index > text.chars().count() {
        return None;
    }
    let mut position = Index2::new(0, 0);
    for character in text.chars().take(index) {
        if character == '\n' {
            position.row += 1;
            position.col = 0;
        } else {
            position.col += 1;
        }
    }
    Some(position)
}

fn position_to_char_index(text: &str, position: Index2) -> Option<usize> {
    validate_position(text, position).ok()?;
    let mut row = 0;
    let mut column = 0;
    for (index, character) in text.chars().enumerate() {
        if row == position.row && column == position.col {
            return Some(index);
        }
        if character == '\n' {
            row += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    (row == position.row && column == position.col).then_some(text.chars().count())
}

fn text_rows(text: &str) -> Vec<Vec<char>> {
    text.split('\n')
        .map(|line| line.chars().collect())
        .collect()
}

fn char_slice(text: &str, range: TextRange) -> Option<String> {
    (range.start <= range.end && range.end <= text.chars().count()).then(|| {
        text.chars()
            .skip(range.start)
            .take(range.end - range.start)
            .collect()
    })
}

fn clamp_position(lines: &Lines, position: Index2) -> Index2 {
    let row = position.row.min(lines.len().saturating_sub(1));
    let column = position.col.min(lines.len_col(row).unwrap_or(0));
    Index2::new(row, column)
}

fn validate_position(text: &str, position: Index2) -> Result<()> {
    let lines = Lines::from(text);
    ensure!(
        position.row < lines.len().max(1),
        "cursor row is outside the editor buffer"
    );
    let row_len = lines.len_col(position.row).unwrap_or(0);
    ensure!(
        position.col <= row_len,
        "cursor column is outside the editor buffer"
    );
    Ok(())
}
