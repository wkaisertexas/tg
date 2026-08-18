//! Character-safe adapter around `edtui`.
//!
//! The adapter deliberately owns application key routing and public snapshots.
//! Project history, Visual Block operations, registers, and missing text
//! operators live here instead of depending on edtui's private state.

use crate::config::{EditorConfig, LineNumbers as ConfigLineNumbers};
use crate::references::model::TextRange;
use anyhow::{Context, Result, ensure};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use edtui::{
    EditorEventHandler, EditorMode, EditorState, EditorView, Highlight, Index2, LineNumbers, Lines,
};
use ratatui::layout::{Position, Rect};
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

    pub fn cursor_screen_position(&self) -> Option<Position> {
        self.state.cursor_screen_position()
    }

    pub fn virtual_text_position(&self, offset: usize, area: Rect) -> Option<Position> {
        let text = self.text();
        let position = char_index_to_position(&text, offset)?;
        let rows = text_rows(&text);
        let (offset_x, offset_y) = self.state.viewport_offset();
        if position.row < offset_y {
            return None;
        }
        let gutter = if self.line_numbers == LineNumbers::None {
            0
        } else {
            self.state.lines.len().max(1).to_string().len() + 1
        };
        let width = usize::from(area.width).saturating_sub(gutter);
        if width == 0 {
            return None;
        }
        let mut screen_row = 0;
        for row in rows.iter().skip(offset_y).take(position.row - offset_y) {
            screen_row += if self.wrap {
                visual_rows(row, width, self.tab_width)
            } else {
                1
            };
        }
        let column = visual_column(
            rows.get(position.row)?.iter().take(position.col).copied(),
            self.tab_width,
        );
        let (screen_column, wrapped_rows) = if self.wrap {
            (column % width, column / width)
        } else {
            if column < offset_x {
                return None;
            }
            (column - offset_x, 0)
        };
        screen_row += wrapped_rows;
        (screen_row < usize::from(area.height) && screen_column < width).then(|| {
            Position::new(
                area.x + u16::try_from(gutter + screen_column).unwrap_or(u16::MAX),
                area.y + u16::try_from(screen_row).unwrap_or(u16::MAX),
            )
        })
    }

    pub fn current_line_number_override(&self) -> Option<(Position, u16)> {
        (self.line_numbers == LineNumbers::Relative && !self.current_line_absolute).then(|| {
            (
                self.cursor_screen_position()
                    .expect("rendered editor cursor has a screen position"),
                (self.state.lines.len().max(1).to_string().len() + 1) as u16,
            )
        })
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

    pub fn view(&mut self) -> EditorView<'_, '_> {
        EditorView::new(&mut self.state)
            .line_numbers(self.line_numbers)
            .tab_width(self.tab_width)
            .wrap(self.wrap)
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

    pub fn set_reference_style(&mut self, style: Style) -> Result<()> {
        self.reference_style = style;
        self.rebuild_highlights()
    }

    pub fn set_reference_ranges(
        &mut self,
        ranges: impl IntoIterator<Item = TextRange>,
    ) -> Result<()> {
        let ranges: Vec<_> = ranges.into_iter().collect();
        let text = self.text();
        let mut highlights = reference_highlights(&text, &ranges, self.reference_style)?;
        highlights.extend(block_highlights(&text, self.block));
        self.reference_ranges = ranges;
        self.state.set_highlights(highlights);
        Ok(())
    }

    pub fn handle_event(&mut self, event: Event, completion_active: bool) -> EditorInput {
        if !self.focused {
            return EditorInput::Ignored;
        }
        if let Event::Key(key) = event {
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                return EditorInput::Ignored;
            }
            if self.command_line.is_some() {
                return self.handle_command_key(key);
            }
            if completion_active && self.preview_toggle.matches(key) {
                return EditorInput::TogglePreview;
            }
            if self.register_prefix {
                self.register_prefix = false;
                if let KeyCode::Char(register) = key.code
                    && (register == '"' || register.is_ascii_alphanumeric())
                {
                    self.selected_register = register;
                }
                return EditorInput::Ignored;
            }
            if self.block_insert.is_some() {
                return self.handle_block_insert_key(key);
            }
            if self.block.is_some() {
                return self.handle_block_key(key);
            }
            if self.replace_mode {
                return self.handle_replace_mode_key(key);
            }
            if self.state.mode == EditorMode::Normal
                && let Some(input) = self.handle_normal_adapter_key(key)
            {
                return input;
            }
            if self.state.mode == EditorMode::Normal {
                if key.code == KeyCode::Char('u') && key.modifiers.is_empty() {
                    return EditorInput::Undo;
                }
                if key.code == KeyCode::Char('r') && key.modifiers == KeyModifiers::CONTROL {
                    return EditorInput::Redo;
                }
            }
            if matches!(self.state.mode, EditorMode::Normal | EditorMode::Visual) {
                if is_ctrl_v(key) {
                    self.block = Some(BlockSelection {
                        anchor: self.state.cursor,
                        cursor: self.state.cursor,
                    });
                    let _ = self.rebuild_highlights();
                    return EditorInput::Delegated {
                        text_changed: false,
                    };
                }
                if key.code == KeyCode::Char('"') {
                    self.register_prefix = true;
                    return EditorInput::Ignored;
                }
            }
            if self.state.mode == EditorMode::Visual
                && let Some(input) = self.handle_visual_key(key)
            {
                return input;
            }
            if self.state.mode == EditorMode::Normal
                && matches!(key.code, KeyCode::Char('p') | KeyCode::Char('P'))
                && self.current_register().is_some()
            {
                return self.paste_register(key.code == KeyCode::Char('P'));
            }
            if self.state.mode == EditorMode::Normal
                && key.code == KeyCode::Char(':')
                && matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT)
            {
                self.command_line = Some(String::new());
                return EditorInput::CommandStarted;
            }
            if !edtui_supports_key(key.code) {
                return EditorInput::Ignored;
            }
            return self.delegate(Event::Key(key));
        }

        match event {
            Event::Paste(text) if self.block_insert.is_some() => self.insert_block_text(&text),
            Event::Paste(_) => self.delegate(event),
            _ => EditorInput::Ignored,
        }
    }

    fn delegate(&mut self, event: Event) -> EditorInput {
        let before = self.text();
        self.events.on_event(event, &mut self.state);
        let text_changed = self.text() != before;
        if text_changed {
            // Until the document layer maps its compound transaction back into
            // this adapter, stale highlights are less safe than no highlights.
            self.reference_ranges.clear();
            self.state.clear_highlights();
        }
        EditorInput::Delegated { text_changed }
    }

    fn handle_normal_adapter_key(&mut self, key: KeyEvent) -> Option<EditorInput> {
        if self.pending_replace {
            self.pending_replace = false;
            return Some(match key.code {
                KeyCode::Esc => {
                    self.count = 0;
                    EditorInput::Delegated {
                        text_changed: false,
                    }
                }
                KeyCode::Char(character)
                    if matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) =>
                {
                    let count = self.take_count();
                    self.replace_characters(character, count)
                }
                _ => {
                    self.count = 0;
                    EditorInput::Ignored
                }
            });
        }

        if let Some(operator) = self.pending_operator {
            if let KeyCode::Char(digit) = key.code
                && key.modifiers.is_empty()
                && digit.is_ascii_digit()
            {
                self.push_count(digit);
                return Some(EditorInput::Ignored);
            }
            if self.pending_text_object.is_none()
                && let KeyCode::Char(kind @ ('i' | 'a')) = key.code
            {
                self.pending_text_object = Some(kind);
                return Some(EditorInput::Ignored);
            }
            if let Some(kind) = self.pending_text_object
                && let KeyCode::Char(object) = key.code
            {
                return Some(self.apply_text_object(operator, kind, object));
            }
            return Some(match key.code {
                KeyCode::Char(character) if character == operator => {
                    self.apply_line_operator(operator)
                }
                KeyCode::Char('w') => self.apply_word_operator(operator),
                KeyCode::Esc => {
                    self.cancel_operator();
                    EditorInput::Delegated {
                        text_changed: false,
                    }
                }
                _ => {
                    self.cancel_operator();
                    EditorInput::Ignored
                }
            });
        }

        if let KeyCode::Char(digit) = key.code
            && key.modifiers.is_empty()
            && digit.is_ascii_digit()
            && (digit != '0' || self.count > 0)
        {
            self.push_count(digit);
            return Some(EditorInput::Ignored);
        }
        if let KeyCode::Char(operator @ ('d' | 'c' | 'y')) = key.code
            && key.modifiers.is_empty()
        {
            self.operator_count = self.take_count();
            self.pending_operator = Some(operator);
            return Some(EditorInput::Ignored);
        }
        if key.code == KeyCode::Char('r') && key.modifiers.is_empty() {
            self.pending_replace = true;
            return Some(EditorInput::Ignored);
        }
        if key.code == KeyCode::Char('R') && key.modifiers == KeyModifiers::SHIFT {
            self.count = 0;
            self.replace_mode = true;
            return Some(EditorInput::Delegated {
                text_changed: false,
            });
        }
        if self.count > 0
            && matches!(
                key.code,
                KeyCode::Char('h' | 'j' | 'k' | 'l' | 'w' | 'e' | 'b')
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
            )
        {
            let count = self.take_count();
            return Some(self.repeat_event(key, count));
        }
        self.count = 0;
        None
    }

    fn apply_word_operator(&mut self, operator: char) -> EditorInput {
        let count = self.combined_count();
        let Some(range) = self.word_motion_range(count) else {
            self.cancel_operator();
            return EditorInput::Ignored;
        };
        if operator == 'y' || (operator == 'c' && count > 1) {
            return self.apply_operator_range(operator, range, false);
        }
        let removed = char_slice(&self.text(), range).expect("word range must be valid");
        self.write_register(RegisterValue::Character(removed));
        self.reset_operator();
        let before = self.text();
        self.delegate(Event::Key(KeyEvent::new(
            KeyCode::Char(operator),
            KeyModifiers::NONE,
        )));
        self.delegate(Event::Key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::NONE,
        )));
        if operator == 'd' {
            for _ in 1..count {
                self.delegate(Event::Key(KeyEvent::new(
                    KeyCode::Char('d'),
                    KeyModifiers::NONE,
                )));
                self.delegate(Event::Key(KeyEvent::new(
                    KeyCode::Char('w'),
                    KeyModifiers::NONE,
                )));
            }
        }
        EditorInput::Delegated {
            text_changed: self.text() != before,
        }
    }

    fn apply_line_operator(&mut self, operator: char) -> EditorInput {
        let count = self.combined_count();
        let Some(range) = self.line_operator_range(count) else {
            self.cancel_operator();
            return EditorInput::Ignored;
        };
        if operator != 'd' {
            return self.apply_operator_range(operator, range, true);
        }
        let removed = char_slice(&self.text(), range).expect("line range must be valid");
        self.write_register(RegisterValue::Line(removed));
        self.reset_operator();
        let before = self.text();
        for _ in 0..count {
            self.delegate(Event::Key(KeyEvent::new(
                KeyCode::Char('d'),
                KeyModifiers::NONE,
            )));
            self.delegate(Event::Key(KeyEvent::new(
                KeyCode::Char('d'),
                KeyModifiers::NONE,
            )));
        }
        EditorInput::Delegated {
            text_changed: self.text() != before,
        }
    }

    fn repeat_event(&mut self, key: KeyEvent, count: usize) -> EditorInput {
        let before = self.text();
        for _ in 0..count.max(1) {
            self.delegate(Event::Key(key));
        }
        EditorInput::Delegated {
            text_changed: self.text() != before,
        }
    }

    fn apply_text_object(&mut self, operator: char, kind: char, object: char) -> EditorInput {
        let count = self.combined_count();
        let Some(mut range) = self.text_object_range(kind, object) else {
            self.cancel_operator();
            return EditorInput::Ignored;
        };
        if object == 'w' && count > 1 {
            range.end = self
                .word_motion_range_from(range.end, count - 1)
                .unwrap_or(range.end);
        }
        self.apply_operator_range(operator, range, false)
    }

    fn apply_operator_range(
        &mut self,
        operator: char,
        range: TextRange,
        linewise: bool,
    ) -> EditorInput {
        self.reset_operator();
        let removed = char_slice(&self.text(), range).expect("operator range must be valid");
        let preserve_line = operator == 'c' && linewise && removed.ends_with('\n');
        self.write_register(if linewise {
            RegisterValue::Line(removed)
        } else {
            RegisterValue::Character(removed)
        });
        match operator {
            'y' => EditorInput::Delegated {
                text_changed: false,
            },
            'c' | 'd' => {
                self.replace_char_range(
                    range,
                    if preserve_line { "\n" } else { "" },
                    range.start,
                    operator == 'c',
                );
                EditorInput::Delegated { text_changed: true }
            }
            _ => EditorInput::Ignored,
        }
    }

    fn word_motion_range(&self, count: usize) -> Option<TextRange> {
        let start = self.cursor_char_offset();
        self.word_motion_range_from(start, count)
            .map(|end| TextRange { start, end })
    }

    fn word_motion_range_from(&self, start: usize, count: usize) -> Option<usize> {
        let chars: Vec<_> = self.text().chars().collect();
        let mut end = start;
        for _ in 0..count {
            let class = word_class(*chars.get(end)?);
            while end < chars.len() && word_class(chars[end]) == class {
                end += 1;
            }
            while end < chars.len() && chars[end].is_whitespace() {
                end += 1;
            }
        }
        Some(end)
    }

    fn line_operator_range(&self, count: usize) -> Option<TextRange> {
        let text = self.text();
        let rows = text_rows(&text);
        let start_row = self.state.cursor.row;
        let end_row = (start_row + count).min(rows.len());
        let start = position_to_char_index(&text, Index2::new(start_row, 0))?;
        let end = if end_row < rows.len() {
            position_to_char_index(&text, Index2::new(end_row, 0))?
        } else {
            text.chars().count()
        };
        Some(TextRange { start, end })
    }

    fn text_object_range(&self, kind: char, object: char) -> Option<TextRange> {
        let around = kind == 'a';
        if object == 'w' {
            return self.word_text_object_range(self.cursor_char_offset(), around);
        }
        let (opening, closing) = match object {
            '\'' => ('\'', '\''),
            '"' => ('"', '"'),
            '(' | ')' => ('(', ')'),
            '[' | ']' => ('[', ']'),
            '{' | '}' => ('{', '}'),
            _ => return None,
        };
        let text = self.text();
        let chars: Vec<_> = text.chars().collect();
        let cursor = self.cursor_char_offset();
        let start = (0..=cursor.min(chars.len().saturating_sub(1)))
            .rev()
            .find(|index| chars[*index] == opening)?;
        let end = (cursor.max(start + 1)..chars.len()).find(|index| chars[*index] == closing)?;
        Some(if around {
            TextRange {
                start,
                end: end + 1,
            }
        } else {
            TextRange {
                start: start + 1,
                end,
            }
        })
    }

    fn word_text_object_range(&self, offset: usize, around: bool) -> Option<TextRange> {
        let chars: Vec<_> = self.text().chars().collect();
        let character = *chars.get(offset)?;
        let class = word_class(character);
        let mut start = offset;
        while start > 0 && word_class(chars[start - 1]) == class {
            start -= 1;
        }
        let mut end = offset + 1;
        while end < chars.len() && word_class(chars[end]) == class {
            end += 1;
        }
        if around {
            if end < chars.len() && chars[end].is_whitespace() {
                while end < chars.len() && chars[end].is_whitespace() {
                    end += 1;
                }
            } else {
                while start > 0 && chars[start - 1].is_whitespace() {
                    start -= 1;
                }
            }
        }
        Some(TextRange { start, end })
    }

    fn replace_characters(&mut self, character: char, count: usize) -> EditorInput {
        let text = self.text();
        let rows = text_rows(&text);
        let cursor = self.state.cursor;
        let available = rows
            .get(cursor.row)
            .map_or(0, |row| row.len().saturating_sub(cursor.col));
        let count = count.max(1).min(available);
        if count == 0 {
            return EditorInput::Ignored;
        }
        let start = self.cursor_char_offset();
        let replacement: String = std::iter::repeat_n(character, count).collect();
        self.replace_char_range(
            TextRange {
                start,
                end: start + count,
            },
            &replacement,
            start,
            false,
        );
        EditorInput::Delegated { text_changed: true }
    }

    fn handle_replace_mode_key(&mut self, key: KeyEvent) -> EditorInput {
        match key.code {
            KeyCode::Esc => {
                self.replace_mode = false;
                if self.state.cursor.col > 0 {
                    self.state.cursor.col -= 1;
                }
                EditorInput::Delegated {
                    text_changed: false,
                }
            }
            KeyCode::Char(character)
                if matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) =>
            {
                let start = self.cursor_char_offset();
                let row_len = text_rows(&self.text())[self.state.cursor.row].len();
                let end = start + usize::from(self.state.cursor.col < row_len);
                self.replace_char_range(
                    TextRange { start, end },
                    &character.to_string(),
                    start + 1,
                    false,
                );
                self.replace_mode = true;
                EditorInput::Delegated { text_changed: true }
            }
            _ => EditorInput::Ignored,
        }
    }

    fn push_count(&mut self, digit: char) {
        self.count = self
            .count
            .saturating_mul(10)
            .saturating_add(digit.to_digit(10).unwrap() as usize);
    }

    fn take_count(&mut self) -> usize {
        std::mem::take(&mut self.count).max(1)
    }

    fn combined_count(&mut self) -> usize {
        self.operator_count.max(1).saturating_mul(self.take_count())
    }

    fn reset_operator(&mut self) {
        self.count = 0;
        self.operator_count = 0;
        self.pending_operator = None;
        self.pending_text_object = None;
    }

    fn cancel_operator(&mut self) {
        self.reset_operator();
        self.selected_register = '"';
    }

    fn reset_pending_input(&mut self) {
        self.cancel_operator();
        self.pending_replace = false;
        self.replace_mode = false;
    }

    fn handle_command_key(&mut self, key: KeyEvent) -> EditorInput {
        match key.code {
            KeyCode::Esc => {
                self.command_line = None;
                EditorInput::CommandCancelled
            }
            KeyCode::Backspace => {
                let command = self.command_line.as_mut().expect("command mode checked");
                if command.pop().is_some() {
                    EditorInput::CommandUpdated
                } else {
                    self.command_line = None;
                    EditorInput::CommandCancelled
                }
            }
            KeyCode::Enter => {
                let command = self.command_line.take().expect("command mode checked");
                EditorInput::CommandSubmitted(command)
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.command_line
                    .as_mut()
                    .expect("command mode checked")
                    .push(character);
                EditorInput::CommandUpdated
            }
            _ => EditorInput::Ignored,
        }
    }

    fn handle_block_key(&mut self, key: KeyEvent) -> EditorInput {
        match key.code {
            KeyCode::Esc => {
                self.block = None;
                let _ = self.rebuild_highlights();
                EditorInput::Delegated {
                    text_changed: false,
                }
            }
            KeyCode::Char('h') | KeyCode::Left => self.move_block(0, -1),
            KeyCode::Char('j') | KeyCode::Down => self.move_block(1, 0),
            KeyCode::Char('k') | KeyCode::Up => self.move_block(-1, 0),
            KeyCode::Char('l') | KeyCode::Right => self.move_block(0, 1),
            KeyCode::Char('y') => self.yank_block(),
            KeyCode::Char('d') => self.delete_block(false),
            KeyCode::Char('c') => self.delete_block(true),
            KeyCode::Char('I') => self.begin_block_insert(false),
            KeyCode::Char('A') => self.begin_block_insert(true),
            KeyCode::Char('p') | KeyCode::Char('P') => self.paste_block(),
            KeyCode::Char('>') => self.transform_block(TextOperator::Indent),
            KeyCode::Char('<') => self.transform_block(TextOperator::Dedent),
            KeyCode::Char('~') => self.transform_block(TextOperator::ToggleCase),
            _ => EditorInput::Ignored,
        }
    }

    fn handle_block_insert_key(&mut self, key: KeyEvent) -> EditorInput {
        match key.code {
            KeyCode::Esc => {
                self.block_insert = None;
                self.state.mode = EditorMode::Normal;
                EditorInput::Delegated {
                    text_changed: false,
                }
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.insert_block_text(&character.to_string())
            }
            KeyCode::Backspace => self.backspace_block_insert(),
            _ => EditorInput::Ignored,
        }
    }

    fn handle_visual_key(&mut self, key: KeyEvent) -> Option<EditorInput> {
        match key.code {
            KeyCode::Char('y') => Some(self.yank_visual()),
            KeyCode::Char('d') => Some(self.delete_visual(false)),
            KeyCode::Char('c') => Some(self.delete_visual(true)),
            KeyCode::Char('p') | KeyCode::Char('P') if self.current_register().is_some() => {
                Some(self.paste_visual())
            }
            KeyCode::Char('>') => Some(self.transform_visual(TextOperator::Indent)),
            KeyCode::Char('<') => Some(self.transform_visual(TextOperator::Dedent)),
            KeyCode::Char('~') => Some(self.transform_visual(TextOperator::ToggleCase)),
            KeyCode::Char('U') => Some(self.transform_visual(TextOperator::Uppercase)),
            KeyCode::Char('u') => Some(self.transform_visual(TextOperator::Lowercase)),
            _ => None,
        }
    }

    fn move_block(&mut self, row_delta: isize, column_delta: isize) -> EditorInput {
        let text = self.text();
        let rows = text_rows(&text);
        let block = self.block.as_mut().expect("block mode checked");
        let row = block
            .cursor
            .row
            .saturating_add_signed(row_delta)
            .min(rows.len().saturating_sub(1));
        let column = block
            .cursor
            .col
            .saturating_add_signed(column_delta)
            .min(rows.get(row).map_or(0, Vec::len));
        block.cursor = Index2::new(row, column);
        self.state.cursor = block.cursor;
        let _ = self.rebuild_highlights();
        EditorInput::Delegated {
            text_changed: false,
        }
    }

    fn block_bounds(&self) -> (usize, usize, usize, usize) {
        let block = self.block.expect("block mode checked");
        (
            block.anchor.row.min(block.cursor.row),
            block.anchor.row.max(block.cursor.row),
            block.anchor.col.min(block.cursor.col),
            block.anchor.col.max(block.cursor.col),
        )
    }

    fn yank_block(&mut self) -> EditorInput {
        let (top, bottom, left, right) = self.block_bounds();
        let rows = text_rows(&self.text());
        let value = (top..=bottom)
            .map(|row| {
                rows[row]
                    .iter()
                    .skip(left)
                    .take(right.saturating_sub(left) + 1)
                    .collect()
            })
            .collect();
        self.write_register(RegisterValue::Block(value));
        self.block = None;
        let _ = self.rebuild_highlights();
        EditorInput::Delegated {
            text_changed: false,
        }
    }

    fn delete_block(&mut self, enter_insert: bool) -> EditorInput {
        let (top, bottom, left, right) = self.block_bounds();
        let mut rows = text_rows(&self.text());
        let mut removed = Vec::new();
        for row in rows.iter_mut().take(bottom + 1).skip(top) {
            let start = left.min(row.len());
            let end = (right + 1).min(row.len());
            removed.push(row.drain(start..end).collect());
        }
        self.write_register(RegisterValue::Block(removed));
        self.replace_rows(rows, Index2::new(top, left), enter_insert);
        if enter_insert {
            self.block_insert = Some(BlockInsert {
                rows: (top..=bottom).collect(),
                column: left,
                inserted_chars: 0,
            });
        }
        EditorInput::Delegated { text_changed: true }
    }

    fn begin_block_insert(&mut self, append: bool) -> EditorInput {
        let (top, bottom, left, right) = self.block_bounds();
        let column = if append { right + 1 } else { left };
        self.block = None;
        self.state.mode = EditorMode::Insert;
        self.state.cursor = Index2::new(top, column);
        self.block_insert = Some(BlockInsert {
            rows: (top..=bottom).collect(),
            column,
            inserted_chars: 0,
        });
        let _ = self.rebuild_highlights();
        EditorInput::Delegated {
            text_changed: false,
        }
    }

    fn insert_block_text(&mut self, inserted: &str) -> EditorInput {
        let Some(block_insert) = self.block_insert.clone() else {
            return EditorInput::Ignored;
        };
        if inserted.contains('\n') {
            return EditorInput::Ignored;
        }
        let chars: Vec<_> = inserted.chars().collect();
        let mut rows = text_rows(&self.text());
        for row in &block_insert.rows {
            let column = (block_insert.column + block_insert.inserted_chars).min(rows[*row].len());
            rows[*row].splice(column..column, chars.iter().copied());
        }
        if let Some(active) = &mut self.block_insert {
            active.inserted_chars += chars.len();
        }
        let cursor = Index2::new(
            block_insert.rows[0],
            block_insert.column + block_insert.inserted_chars + chars.len(),
        );
        self.replace_rows(rows, cursor, true);
        EditorInput::Delegated { text_changed: true }
    }

    fn backspace_block_insert(&mut self) -> EditorInput {
        let Some(block_insert) = self.block_insert.clone() else {
            return EditorInput::Ignored;
        };
        if block_insert.inserted_chars == 0 {
            return EditorInput::Ignored;
        }
        let mut rows = text_rows(&self.text());
        let column = block_insert.column + block_insert.inserted_chars - 1;
        for row in &block_insert.rows {
            if column < rows[*row].len() {
                rows[*row].remove(column);
            }
        }
        if let Some(active) = &mut self.block_insert {
            active.inserted_chars -= 1;
        }
        self.replace_rows(rows, Index2::new(block_insert.rows[0], column), true);
        EditorInput::Delegated { text_changed: true }
    }

    fn write_register(&mut self, value: RegisterValue) {
        self.registers.insert('"', value.clone());
        self.registers.insert(self.selected_register, value);
        self.selected_register = '"';
    }

    fn current_register(&self) -> Option<RegisterValue> {
        self.registers
            .get(&self.selected_register)
            .or_else(|| self.registers.get(&'"'))
            .cloned()
    }

    pub fn register_text(&self, register: char) -> Option<String> {
        self.registers.get(&register).map(|value| match value {
            RegisterValue::Character(text) | RegisterValue::Line(text) => text.clone(),
            RegisterValue::Block(rows) => rows.join("\n"),
        })
    }

    fn visual_range(&self) -> Option<(TextRange, bool)> {
        let selection = self.state.selection.as_ref()?;
        let text = self.text();
        if selection.line_mode {
            let top = selection.start.row.min(selection.end.row);
            let bottom = selection.start.row.max(selection.end.row);
            let start = position_to_char_index(&text, Index2::new(top, 0))?;
            let rows = text_rows(&text);
            let end = if bottom + 1 < rows.len() {
                position_to_char_index(&text, Index2::new(bottom + 1, 0))?
            } else {
                text.chars().count()
            };
            Some((TextRange { start, end }, true))
        } else {
            let start_position = selection.start();
            let end_position = selection.end();
            let start = position_to_char_index(&text, start_position)?;
            let mut end = position_to_char_index(&text, end_position)?;
            if end < text.chars().count() {
                end += 1;
            }
            Some((TextRange { start, end }, false))
        }
    }

    fn yank_visual(&mut self) -> EditorInput {
        let Some((range, linewise)) = self.visual_range() else {
            return EditorInput::Ignored;
        };
        let text = char_slice(&self.text(), range).unwrap_or_default();
        self.write_register(if linewise {
            RegisterValue::Line(text)
        } else {
            RegisterValue::Character(text)
        });
        self.state.mode = EditorMode::Normal;
        self.state.selection = None;
        EditorInput::Delegated {
            text_changed: false,
        }
    }

    fn delete_visual(&mut self, enter_insert: bool) -> EditorInput {
        let Some((range, linewise)) = self.visual_range() else {
            return EditorInput::Ignored;
        };
        let text = self.text();
        let removed = char_slice(&text, range).unwrap_or_default();
        self.write_register(if linewise {
            RegisterValue::Line(removed)
        } else {
            RegisterValue::Character(removed)
        });
        self.replace_char_range(range, "", range.start, enter_insert);
        EditorInput::Delegated { text_changed: true }
    }

    fn paste_visual(&mut self) -> EditorInput {
        let Some(value) = self.current_register() else {
            return EditorInput::Ignored;
        };
        let Some((range, _)) = self.visual_range() else {
            return EditorInput::Ignored;
        };
        let replacement = register_as_text(&value);
        self.replace_char_range(range, &replacement, range.start, false);
        self.selected_register = '"';
        EditorInput::Delegated { text_changed: true }
    }

    fn paste_register(&mut self, before: bool) -> EditorInput {
        let Some(value) = self.current_register() else {
            return EditorInput::Ignored;
        };
        let text = self.text();
        let cursor = self.cursor_char_offset();
        match value {
            RegisterValue::Character(value) => {
                let offset = if before || cursor == text.chars().count() {
                    cursor
                } else {
                    cursor + 1
                };
                self.replace_char_range(
                    TextRange {
                        start: offset,
                        end: offset,
                    },
                    &value,
                    offset,
                    false,
                );
            }
            RegisterValue::Line(value) => {
                let rows = text_rows(&text);
                let row = self.state.cursor.row + usize::from(!before);
                let offset = if row < rows.len() {
                    position_to_char_index(&text, Index2::new(row, 0)).unwrap()
                } else {
                    text.chars().count()
                };
                self.replace_char_range(
                    TextRange {
                        start: offset,
                        end: offset,
                    },
                    &value,
                    offset,
                    false,
                );
            }
            RegisterValue::Block(values) => {
                let mut rows = text_rows(&text);
                let first_row = self.state.cursor.row;
                let column = self.state.cursor.col + usize::from(!before);
                while rows.len() < first_row + values.len() {
                    rows.push(Vec::new());
                }
                for (index, value) in values.iter().enumerate() {
                    let row = &mut rows[first_row + index];
                    let insert = column.min(row.len());
                    row.splice(insert..insert, value.chars());
                }
                self.replace_rows(rows, Index2::new(first_row, column), false);
            }
        }
        self.selected_register = '"';
        EditorInput::Delegated { text_changed: true }
    }

    fn paste_block(&mut self) -> EditorInput {
        let Some(value) = self.current_register() else {
            return EditorInput::Ignored;
        };
        let (top, bottom, left, right) = self.block_bounds();
        let values = match value {
            RegisterValue::Block(values) => values,
            other => vec![register_as_text(&other); bottom - top + 1],
        };
        let mut rows = text_rows(&self.text());
        for (row_index, row) in rows.iter_mut().enumerate().take(bottom + 1).skip(top) {
            let start = left.min(row.len());
            let end = (right + 1).min(row.len());
            row.splice(
                start..end,
                values
                    .get(row_index - top)
                    .or_else(|| values.last())
                    .into_iter()
                    .flat_map(|value| value.chars()),
            );
        }
        self.block = None;
        self.replace_rows(rows, Index2::new(top, left), false);
        self.selected_register = '"';
        EditorInput::Delegated { text_changed: true }
    }

    fn transform_visual(&mut self, operator: TextOperator) -> EditorInput {
        let Some((range, _)) = self.visual_range() else {
            return EditorInput::Ignored;
        };
        let changed = self.apply_operator(range, operator).unwrap_or(false);
        self.state.mode = EditorMode::Normal;
        self.state.selection = None;
        EditorInput::Delegated {
            text_changed: changed,
        }
    }

    fn transform_block(&mut self, operator: TextOperator) -> EditorInput {
        let (top, bottom, left, right) = self.block_bounds();
        let mut rows = text_rows(&self.text());
        let mut changed = false;
        for row in rows.iter_mut().take(bottom + 1).skip(top) {
            let start = left.min(row.len());
            let end = (right + 1).min(row.len());
            changed |= transform_chars(&mut row[start..end], operator);
        }
        self.block = None;
        if changed {
            self.replace_rows(rows, Index2::new(top, left), false);
        }
        EditorInput::Delegated {
            text_changed: changed,
        }
    }

    pub fn apply_operator(&mut self, range: TextRange, operator: TextOperator) -> Result<bool> {
        ensure!(range.start <= range.end, "operator range is reversed");
        ensure!(
            range.end <= self.text().chars().count(),
            "operator range is outside the editor buffer"
        );
        let text = self.text();
        let changed = match operator {
            TextOperator::Indent | TextOperator::Dedent => {
                let start = char_index_to_position(&text, range.start).unwrap().row;
                let end = char_index_to_position(&text, range.end).unwrap().row;
                let mut rows = text_rows(&text);
                let mut changed = false;
                let last = end.min(rows.len() - 1);
                for row in &mut rows[start..=last] {
                    match operator {
                        TextOperator::Indent => {
                            row.splice(0..0, [' ', ' ']);
                            changed = true;
                        }
                        TextOperator::Dedent => {
                            let count = row.iter().take(2).take_while(|ch| **ch == ' ').count();
                            if count > 0 {
                                row.drain(0..count);
                                changed = true;
                            } else if row.first() == Some(&'\t') {
                                row.remove(0);
                                changed = true;
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                if changed {
                    self.replace_rows(rows, self.state.cursor, false);
                }
                changed
            }
            _ => {
                let mut chars: Vec<_> = text.chars().collect();
                let changed = transform_chars(&mut chars[range.start..range.end], operator);
                if changed {
                    let replacement: String = chars.into_iter().collect();
                    self.replace_all_text(replacement, self.state.cursor, false);
                }
                changed
            }
        };
        Ok(changed)
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

    fn rebuild_highlights(&mut self) -> Result<()> {
        let mut highlights =
            reference_highlights(&self.text(), &self.reference_ranges, self.reference_style)?;
        highlights.extend(block_highlights(&self.text(), self.block));
        self.state.set_highlights(highlights);
        Ok(())
    }
}

impl Default for EditorSession {
    fn default() -> Self {
        Self::new("")
    }
}

impl KeyBinding {
    fn matches(self, key: KeyEvent) -> bool {
        self.code == key.code && self.modifiers == key.modifiers
    }
}

fn parse_key_binding(notation: &str) -> Result<KeyBinding> {
    let (modifier, key) = notation
        .split_once('-')
        .context("preview toggle must use modifier-key notation")?;
    let modifiers = match modifier.to_ascii_lowercase().as_str() {
        "ctrl" => KeyModifiers::CONTROL,
        "alt" => KeyModifiers::ALT,
        "shift" => KeyModifiers::SHIFT,
        "meta" => KeyModifiers::META,
        _ => return Err(anyhow::anyhow!("unknown preview-toggle modifier")),
    };
    let code = match key.to_ascii_lowercase().as_str() {
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        value if value.chars().count() == 1 => KeyCode::Char(value.chars().next().unwrap()),
        _ => return Err(anyhow::anyhow!("unknown preview-toggle key")),
    };
    Ok(KeyBinding { code, modifiers })
}

fn is_ctrl_v(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('v') && key.modifiers == KeyModifiers::CONTROL
}

fn edtui_supports_key(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Char(_)
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Enter
            | KeyCode::Esc
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Tab
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
    )
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

fn visual_column(characters: impl IntoIterator<Item = char>, tab_width: usize) -> usize {
    characters.into_iter().fold(0, |column, character| {
        if character == '\t' {
            column + tab_width - column % tab_width
        } else {
            column + 1
        }
    })
}

fn visual_rows(row: &[char], width: usize, tab_width: usize) -> usize {
    visual_column(row.iter().copied(), tab_width)
        .max(1)
        .div_ceil(width)
}

fn char_slice(text: &str, range: TextRange) -> Option<String> {
    (range.start <= range.end && range.end <= text.chars().count()).then(|| {
        text.chars()
            .skip(range.start)
            .take(range.end - range.start)
            .collect()
    })
}

fn register_as_text(value: &RegisterValue) -> String {
    match value {
        RegisterValue::Character(text) | RegisterValue::Line(text) => text.clone(),
        RegisterValue::Block(rows) => rows.join("\n"),
    }
}

fn word_class(character: char) -> u8 {
    if character.is_whitespace() {
        0
    } else if character.is_alphanumeric() || character == '_' {
        1
    } else {
        2
    }
}

fn transform_chars(chars: &mut [char], operator: TextOperator) -> bool {
    let mut changed = false;
    for character in chars {
        let replacement = match operator {
            TextOperator::Uppercase => character.to_uppercase().next().unwrap_or(*character),
            TextOperator::Lowercase => character.to_lowercase().next().unwrap_or(*character),
            TextOperator::ToggleCase if character.is_lowercase() => {
                character.to_uppercase().next().unwrap_or(*character)
            }
            TextOperator::ToggleCase if character.is_uppercase() => {
                character.to_lowercase().next().unwrap_or(*character)
            }
            TextOperator::ToggleCase => *character,
            TextOperator::Indent | TextOperator::Dedent => return false,
        };
        changed |= replacement != *character;
        *character = replacement;
    }
    changed
}

fn clamp_position(lines: &Lines, position: Index2) -> Index2 {
    let row = position.row.min(lines.len().saturating_sub(1));
    let column = position.col.min(lines.len_col(row).unwrap_or(0));
    Index2::new(row, column)
}

fn reference_highlights(text: &str, ranges: &[TextRange], style: Style) -> Result<Vec<Highlight>> {
    let length = text.chars().count();
    let mut highlights = Vec::with_capacity(ranges.len());
    for range in ranges {
        ensure!(range.start <= range.end, "reference range is reversed");
        ensure!(
            range.end <= length,
            "reference range is outside the editor buffer"
        );
        if range.start == range.end {
            continue;
        }
        let start = char_index_to_position(text, range.start)
            .expect("validated reference start must map to a position");
        let end = char_index_to_position(text, range.end - 1)
            .expect("validated reference end must map to a position");
        highlights.push(Highlight::new(start, end, style));
    }
    Ok(highlights)
}

fn block_highlights(text: &str, block: Option<BlockSelection>) -> Vec<Highlight> {
    let Some(block) = block else {
        return Vec::new();
    };
    let rows = text_rows(text);
    let top = block.anchor.row.min(block.cursor.row);
    let bottom = block.anchor.row.max(block.cursor.row);
    let left = block.anchor.col.min(block.cursor.col);
    let right = block.anchor.col.max(block.cursor.col);
    (top..=bottom)
        .filter_map(|row| {
            let len = rows.get(row)?.len();
            (left < len).then(|| {
                Highlight::new(
                    Index2::new(row, left),
                    Index2::new(row, right.min(len.saturating_sub(1))),
                    Style::default().bg(Color::DarkGray),
                )
            })
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(character: char) -> Event {
        Event::Key(KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::CONTROL,
        ))
    }

    fn shift(character: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::SHIFT))
    }

    fn type_command(session: &mut EditorSession, command: &str) -> EditorInput {
        assert_eq!(
            session.handle_event(key(KeyCode::Char(':')), false),
            EditorInput::CommandStarted
        );
        for character in command.chars() {
            assert_eq!(
                session.handle_event(key(KeyCode::Char(character)), false),
                EditorInput::CommandUpdated
            );
        }
        session.handle_event(key(KeyCode::Enter), false)
    }

    #[test]
    fn exact_text_unicode_tabs_and_terminal_newline_round_trip() {
        let source = "first\tcolumn\n日本語 🦀\n";
        let session = EditorSession::new(source);
        assert_eq!(session.text(), source);
        assert_eq!(session.snapshot().text(), source);
    }

    #[test]
    fn unicode_edit_and_bracketed_paste_are_character_safe() {
        let mut session = EditorSession::new("a🦀b\t\n");
        session.handle_event(key(KeyCode::Char('l')), false);
        session.handle_event(key(KeyCode::Char('i')), false);
        session.handle_event(key(KeyCode::Char('X')), false);
        session.handle_event(key(KeyCode::Esc), false);
        assert_eq!(session.text(), "aX🦀b\t\n");

        let mut pasted = EditorSession::new("");
        pasted.handle_event(key(KeyCode::Char('i')), false);
        let result = pasted.handle_event(Event::Paste("a\t🦀\nβ\n".into()), false);
        assert_eq!(result, EditorInput::Delegated { text_changed: true });
        assert_eq!(pasted.text(), "a\t🦀\nβ\n");
    }

    #[test]
    fn visual_character_and_line_edits_delegate_to_edtui() {
        let mut character = EditorSession::new("abcd");
        character.handle_event(key(KeyCode::Char('v')), false);
        assert_eq!(character.mode(), AdapterMode::VisualCharacter);
        character.handle_event(key(KeyCode::Char('l')), false);
        character.handle_event(key(KeyCode::Char('d')), false);
        assert_eq!(character.text(), "cd");
        assert_eq!(character.mode(), AdapterMode::Normal);

        let mut line = EditorSession::new("a\nb\nc\n");
        line.handle_event(shift('V'), false);
        assert_eq!(line.mode(), AdapterMode::VisualLine);
        line.handle_event(key(KeyCode::Char('j')), false);
        line.handle_event(key(KeyCode::Char('d')), false);
        assert_eq!(line.text(), "c\n");
        assert_eq!(line.mode(), AdapterMode::Normal);
    }

    #[test]
    fn counted_motions_and_operator_composition_follow_vim_counts() {
        let mut motion = EditorSession::new("one two three four");
        motion.handle_event(key(KeyCode::Char('2')), false);
        motion.handle_event(key(KeyCode::Char('w')), false);
        assert_eq!(motion.state().cursor, Index2::new(0, 8));

        let mut operator = EditorSession::new("one two three four five");
        operator.handle_event(key(KeyCode::Char('2')), false);
        operator.handle_event(key(KeyCode::Char('d')), false);
        operator.handle_event(key(KeyCode::Char('2')), false);
        operator.handle_event(key(KeyCode::Char('w')), false);
        assert_eq!(operator.text(), "five");
        assert_eq!(
            operator.register_text('"').as_deref(),
            Some("one two three four ")
        );

        let mut lines = EditorSession::new("one\ntwo\nthree\nfour\n");
        lines.handle_event(key(KeyCode::Char('2')), false);
        lines.handle_event(key(KeyCode::Char('d')), false);
        lines.handle_event(key(KeyCode::Char('d')), false);
        assert_eq!(lines.text(), "three\nfour\n");
    }

    #[test]
    fn word_text_objects_cover_inner_and_around_forms() {
        let mut inner = EditorSession::new("one two");
        inner.handle_event(key(KeyCode::Char('l')), false);
        inner.handle_event(key(KeyCode::Char('d')), false);
        inner.handle_event(key(KeyCode::Char('i')), false);
        inner.handle_event(key(KeyCode::Char('w')), false);
        assert_eq!(inner.text(), " two");

        let mut around = EditorSession::new("one two");
        around.handle_event(key(KeyCode::Char('l')), false);
        around.handle_event(key(KeyCode::Char('d')), false);
        around.handle_event(key(KeyCode::Char('a')), false);
        around.handle_event(key(KeyCode::Char('w')), false);
        assert_eq!(around.text(), "two");
    }

    #[test]
    fn quote_and_bracket_text_objects_cover_inner_and_around_forms() {
        for (source, cursor, object, expected) in [
            ("say \"hello world\" now", 6, "i\"", "say \"\" now"),
            ("call(alpha + beta) now", 7, "a(", "call now"),
            ("map[key + value] tail", 5, "i]", "map[] tail"),
            ("map{key + value} tail", 5, "a}", "map tail"),
            ("say 'hello' now", 6, "a'", "say  now"),
        ] {
            let mut session = EditorSession::new(source);
            session.set_cursor_char_offset(cursor).unwrap();
            session.handle_event(key(KeyCode::Char('d')), false);
            for character in object.chars() {
                session.handle_event(key(KeyCode::Char(character)), false);
            }
            assert_eq!(session.text(), expected, "object {object}");
        }
    }

    #[test]
    fn change_and_yank_compose_with_motions_and_text_objects() {
        let mut change = EditorSession::new("one two three");
        change.handle_event(key(KeyCode::Char('2')), false);
        change.handle_event(key(KeyCode::Char('c')), false);
        change.handle_event(key(KeyCode::Char('w')), false);
        change.handle_event(key(KeyCode::Char('X')), false);
        change.handle_event(key(KeyCode::Esc), false);
        assert_eq!(change.text(), "Xthree");
        assert_eq!(change.register_text('"').as_deref(), Some("one two "));

        let mut quoted = EditorSession::new("say \"hello world\" now");
        quoted.set_cursor_char_offset(6).unwrap();
        quoted.handle_event(key(KeyCode::Char('"')), false);
        quoted.handle_event(key(KeyCode::Char('a')), false);
        for character in "ci\"".chars() {
            quoted.handle_event(key(KeyCode::Char(character)), false);
        }
        quoted.handle_event(key(KeyCode::Char('X')), false);
        quoted.handle_event(key(KeyCode::Esc), false);
        assert_eq!(quoted.text(), "say \"X\" now");
        assert_eq!(quoted.register_text('a').as_deref(), Some("hello world"));
        assert_eq!(quoted.register_text('"').as_deref(), Some("hello world"));

        let mut yank = EditorSession::new("call(alpha + beta) now");
        yank.set_cursor_char_offset(7).unwrap();
        yank.handle_event(key(KeyCode::Char('"')), false);
        yank.handle_event(key(KeyCode::Char('b')), false);
        for character in "ya(".chars() {
            yank.handle_event(key(KeyCode::Char(character)), false);
        }
        assert_eq!(yank.text(), "call(alpha + beta) now");
        assert_eq!(yank.register_text('b').as_deref(), Some("(alpha + beta)"));
        assert_eq!(yank.register_text('"').as_deref(), Some("(alpha + beta)"));

        let mut motion = EditorSession::new("one two three");
        motion.handle_event(key(KeyCode::Char('2')), false);
        motion.handle_event(key(KeyCode::Char('y')), false);
        motion.handle_event(key(KeyCode::Char('w')), false);
        assert_eq!(motion.text(), "one two three");
        assert_eq!(motion.register_text('"').as_deref(), Some("one two "));

        let mut line_change = EditorSession::new("one\ntwo\n");
        line_change.handle_event(key(KeyCode::Char('c')), false);
        line_change.handle_event(key(KeyCode::Char('c')), false);
        line_change.handle_event(key(KeyCode::Char('X')), false);
        line_change.handle_event(key(KeyCode::Esc), false);
        assert_eq!(line_change.text(), "X\ntwo\n");

        let mut line_yank = EditorSession::new("one\ntwo\n");
        line_yank.handle_event(key(KeyCode::Char('y')), false);
        line_yank.handle_event(key(KeyCode::Char('y')), false);
        assert_eq!(line_yank.text(), "one\ntwo\n");
        assert_eq!(line_yank.register_text('"').as_deref(), Some("one\n"));
    }

    #[test]
    fn cancelled_and_unsupported_operator_sequences_do_not_leak_state() {
        let mut cancelled = EditorSession::new("one two");
        cancelled.handle_event(key(KeyCode::Char('"')), false);
        cancelled.handle_event(key(KeyCode::Char('a')), false);
        cancelled.handle_event(key(KeyCode::Char('2')), false);
        cancelled.handle_event(key(KeyCode::Char('d')), false);
        cancelled.handle_event(key(KeyCode::Esc), false);
        cancelled.handle_event(key(KeyCode::Char('y')), false);
        cancelled.handle_event(key(KeyCode::Char('i')), false);
        cancelled.handle_event(key(KeyCode::Char('w')), false);
        assert_eq!(cancelled.text(), "one two");
        assert_eq!(cancelled.register_text('a'), None);
        assert_eq!(cancelled.register_text('"').as_deref(), Some("one"));

        let mut unsupported = EditorSession::new("one two");
        unsupported.handle_event(key(KeyCode::Char('d')), false);
        unsupported.handle_event(key(KeyCode::Char('i')), false);
        assert_eq!(
            unsupported.handle_event(key(KeyCode::Char('z')), false),
            EditorInput::Ignored
        );
        unsupported.handle_event(key(KeyCode::Char('w')), false);
        assert_eq!(unsupported.text(), "one two");
        assert_eq!(unsupported.state().cursor, Index2::new(0, 4));
    }

    #[test]
    fn dot_repeats_the_last_delegated_change() {
        let mut delete = EditorSession::new("one two three");
        delete.handle_event(key(KeyCode::Char('d')), false);
        delete.handle_event(key(KeyCode::Char('w')), false);
        delete.handle_event(key(KeyCode::Char('.')), false);
        assert_eq!(delete.text(), "three");
    }

    #[test]
    fn replace_operation_replace_mode_and_join_are_available() {
        let mut operation = EditorSession::new("abc");
        operation.handle_event(key(KeyCode::Char('r')), false);
        operation.handle_event(key(KeyCode::Char('X')), false);
        assert_eq!(operation.text(), "Xbc");
        assert_eq!(operation.mode(), AdapterMode::Normal);

        let mut mode = EditorSession::new("abc");
        mode.handle_event(shift('R'), false);
        mode.handle_event(key(KeyCode::Char('X')), false);
        mode.handle_event(key(KeyCode::Char('Y')), false);
        mode.handle_event(key(KeyCode::Esc), false);
        assert_eq!(mode.text(), "XYc");
        assert_eq!(mode.mode(), AdapterMode::Normal);

        let mut join = EditorSession::new("one \ntwo\nthree");
        join.handle_event(shift('J'), false);
        assert_eq!(join.text(), "one two\nthree");
    }

    #[test]
    fn application_commands_and_completion_preview_are_routed_before_widget() {
        for command in ["w", "q", "q!", "wq", "copy"] {
            let mut session = EditorSession::new("unchanged");
            assert_eq!(
                type_command(&mut session, command),
                EditorInput::CommandSubmitted(command.into())
            );
            assert_eq!(session.text(), "unchanged");
            assert_eq!(session.mode(), AdapterMode::Normal);
        }

        let mut session = EditorSession::new("text");
        assert_eq!(
            session.handle_event(ctrl('p'), true),
            EditorInput::TogglePreview
        );
        assert_eq!(
            session.handle_event(ctrl('p'), false),
            EditorInput::Delegated {
                text_changed: false
            }
        );
        assert_eq!(session.text(), "text");
        assert_eq!(
            session.handle_event(ctrl('v'), false),
            EditorInput::Delegated {
                text_changed: false
            }
        );
        assert_eq!(session.mode(), AdapterMode::VisualBlock);
        session.handle_event(key(KeyCode::Esc), false);
        assert_eq!(
            session.handle_event(key(KeyCode::Char('"')), false),
            EditorInput::Ignored
        );
        assert_eq!(
            session.handle_event(key(KeyCode::Char('a')), false),
            EditorInput::Ignored
        );
        assert_eq!(
            session.handle_event(key(KeyCode::F(1)), false),
            EditorInput::Ignored
        );

        session.handle_event(key(KeyCode::Char('i')), false);
        session.handle_event(key(KeyCode::Char(':')), false);
        assert_eq!(session.text(), ":text");
        assert_eq!(session.mode(), AdapterMode::Insert);
    }

    #[test]
    fn editor_render_and_preview_key_options_are_configurable() {
        let mut session = EditorSession::new("one\ttwo\nthree");
        let config = EditorConfig {
            line_numbers: ConfigLineNumbers::None,
            current_line_absolute: false,
            tab_width: 8,
            wrap: false,
            ..EditorConfig::default()
        };
        session.configure(&config, "ctrl-x").unwrap();

        assert_eq!(session.line_numbers, LineNumbers::None);
        assert!(!session.current_line_absolute);
        assert_eq!(session.tab_width, 8);
        assert!(!session.wrap);
        assert_eq!(
            session.handle_event(
                Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
                true,
            ),
            EditorInput::TogglePreview
        );
        assert_ne!(
            session.handle_event(
                Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
                true,
            ),
            EditorInput::TogglePreview
        );
    }

    #[test]
    fn global_character_ranges_map_to_multiline_unicode_highlights() {
        let mut session = EditorSession::new("α\nβ🦀z");
        session
            .set_reference_ranges([TextRange { start: 2, end: 4 }])
            .unwrap();
        assert_eq!(session.state().highlights.len(), 1);
        assert_eq!(session.state().highlights[0].start, Index2::new(1, 0));
        assert_eq!(session.state().highlights[0].end, Index2::new(1, 1));
        assert_eq!(session.text(), "α\nβ🦀z");
        assert!(
            session
                .set_reference_ranges([TextRange { start: 0, end: 99 }])
                .is_err()
        );
    }

    #[test]
    fn relative_gutter_uses_absolute_number_on_the_cursor_row() {
        let text = (1..=12)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut session = EditorSession::new(text);
        session.state.cursor = Index2::new(8, 0);
        let area = Rect::new(0, 0, 16, 12);
        let mut buffer = Buffer::empty(area);
        session.view().render(area, &mut buffer);

        let row = |y| (0..3).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert_eq!(row(7), " 1 ");
        assert_eq!(row(8), "9  ");
        assert_eq!(row(9), " 1 ");
    }

    #[test]
    fn virtual_text_positions_follow_gutters_tabs_and_wrapping() {
        let area = Rect::new(2, 3, 20, 8);
        let mut session = EditorSession::new("abc\nx\tz");
        session.line_numbers = LineNumbers::None;
        session.wrap = false;
        session.tab_width = 4;
        assert_eq!(
            session.virtual_text_position(3, area),
            Some(Position::new(5, 3))
        );
        assert_eq!(
            session.virtual_text_position(7, area),
            Some(Position::new(7, 4))
        );

        let mut wrapped = EditorSession::new("abcdef\nz");
        wrapped.line_numbers = LineNumbers::None;
        wrapped.wrap = true;
        assert_eq!(
            wrapped.virtual_text_position(8, Rect::new(0, 0, 5, 5)),
            Some(Position::new(1, 2))
        );
    }

    #[test]
    fn snapshots_restore_widget_and_reference_state_for_project_history() {
        let mut session = EditorSession::new("a🦀b\n");
        session.state.cursor = Index2::new(0, 1);
        session
            .set_reference_ranges([TextRange { start: 1, end: 2 }])
            .unwrap();
        let snapshot = session.snapshot();

        session.handle_event(key(KeyCode::Char('i')), false);
        session.handle_event(Event::Paste("changed".into()), false);
        assert_ne!(session.text(), snapshot.text());
        assert!(session.state().highlights.is_empty());

        session.restore(&snapshot).unwrap();
        assert_eq!(session.text(), "a🦀b\n");
        assert_eq!(session.state().cursor, Index2::new(0, 1));
        assert_eq!(session.state().highlights.len(), 1);
        assert_eq!(session.snapshot().text(), snapshot.text());
        assert_eq!(session.snapshot().cursor(), snapshot.cursor());
    }

    #[test]
    fn cursor_focus_replacement_and_external_history_routes_are_public() {
        let mut session = EditorSession::new("a🦀\nβ");
        session.set_cursor_char_offset(2).unwrap();
        assert_eq!(session.cursor_char_offset(), 2);
        assert_eq!(session.state().cursor, Index2::new(0, 2));
        assert!(session.set_cursor_char_offset(99).is_err());
        assert_eq!(
            session.handle_event(key(KeyCode::Char('u')), false),
            EditorInput::Undo
        );
        assert_eq!(session.handle_event(ctrl('r'), false), EditorInput::Redo);
        session.blur();
        assert!(!session.is_focused());
        assert_eq!(
            session.handle_event(key(KeyCode::Char('i')), false),
            EditorInput::Ignored
        );
        session.focus();
        session
            .replace_text_and_ranges("x🦀y", [TextRange { start: 1, end: 2 }])
            .unwrap();
        assert_eq!(session.text(), "x🦀y");
        assert_eq!(session.state().highlights.len(), 1);
    }

    #[test]
    fn accepted_reference_replacement_preserves_insert_mode() {
        let mut session = EditorSession::new("@src");
        session.set_cursor_char_offset(4).unwrap();
        session.handle_event(key(KeyCode::Char('i')), false);
        session
            .replace_text_and_ranges("@src/app.rs", [TextRange { start: 0, end: 11 }])
            .unwrap();
        session.set_cursor_char_offset(11).unwrap();

        assert_eq!(session.mode(), AdapterMode::Insert);
        assert_eq!(
            session.handle_event(key(KeyCode::Char(':')), false),
            EditorInput::Delegated { text_changed: true }
        );
        assert_eq!(session.text(), "@src/app.rs:");
        assert_eq!(session.mode(), AdapterMode::Insert);
        assert_eq!(session.command_line(), None);
    }

    #[test]
    fn visual_block_registers_delete_change_insert_append_and_paste() {
        let mut yank = EditorSession::new("αβγ\nδεζ\n");
        yank.set_cursor_char_offset(1).unwrap();
        yank.handle_event(key(KeyCode::Char('"')), false);
        yank.handle_event(key(KeyCode::Char('a')), false);
        yank.handle_event(ctrl('v'), false);
        yank.handle_event(key(KeyCode::Char('j')), false);
        yank.handle_event(key(KeyCode::Char('l')), false);
        yank.handle_event(key(KeyCode::Char('y')), false);
        assert_eq!(yank.register_text('a').as_deref(), Some("βγ\nεζ"));
        assert_eq!(yank.register_text('"').as_deref(), Some("βγ\nεζ"));

        yank.set_cursor_char_offset(0).unwrap();
        yank.handle_event(ctrl('v'), false);
        yank.handle_event(key(KeyCode::Char('j')), false);
        yank.handle_event(key(KeyCode::Char('p')), false);
        assert_eq!(yank.text(), "βγβγ\nεζεζ\n");

        let mut insert = EditorSession::new("abc\ndef\n");
        insert.set_cursor_char_offset(1).unwrap();
        insert.handle_event(ctrl('v'), false);
        insert.handle_event(key(KeyCode::Char('j')), false);
        insert.handle_event(shift('I'), false);
        insert.handle_event(key(KeyCode::Char('X')), false);
        insert.handle_event(key(KeyCode::Esc), false);
        assert_eq!(insert.text(), "aXbc\ndXef\n");

        let mut append = EditorSession::new("abc\ndef\n");
        append.set_cursor_char_offset(1).unwrap();
        append.handle_event(ctrl('v'), false);
        append.handle_event(key(KeyCode::Char('j')), false);
        append.handle_event(shift('A'), false);
        append.handle_event(key(KeyCode::Char('X')), false);
        append.handle_event(key(KeyCode::Esc), false);
        assert_eq!(append.text(), "abXc\ndeXf\n");

        let mut change = EditorSession::new("abc\ndef\n");
        change.set_cursor_char_offset(1).unwrap();
        change.handle_event(ctrl('v'), false);
        change.handle_event(key(KeyCode::Char('j')), false);
        change.handle_event(key(KeyCode::Char('c')), false);
        change.handle_event(key(KeyCode::Char('Z')), false);
        change.handle_event(key(KeyCode::Esc), false);
        assert_eq!(change.text(), "aZc\ndZf\n");
    }

    #[test]
    fn project_text_operators_are_unicode_safe_transactions() {
        let mut session = EditorSession::new("  éAb\n\t日z\n");
        assert!(
            session
                .apply_operator(TextRange { start: 2, end: 5 }, TextOperator::Uppercase)
                .unwrap()
        );
        assert_eq!(session.text(), "  ÉAB\n\t日z\n");
        assert!(
            session
                .apply_operator(TextRange { start: 2, end: 5 }, TextOperator::Lowercase)
                .unwrap()
        );
        assert!(
            session
                .apply_operator(TextRange { start: 0, end: 0 }, TextOperator::Indent)
                .unwrap()
        );
        assert!(
            session
                .apply_operator(TextRange { start: 0, end: 0 }, TextOperator::Dedent)
                .unwrap()
        );
        assert!(
            session
                .apply_operator(TextRange { start: 2, end: 5 }, TextOperator::ToggleCase)
                .unwrap()
        );
        assert_eq!(session.text(), "  ÉAB\n\t日z\n");
    }
}
