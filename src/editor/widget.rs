//! Character-safe adapter around `edtui`.
//!
//! The adapter deliberately owns application key routing and public snapshots.
//! Visual Block operations and named registers remain follow-on work and are
//! reported through [`EditorSession::follow_on_features`].

use crate::references::model::TextRange;
use anyhow::{Result, ensure};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use edtui::{
    EditorEventHandler, EditorMode, EditorState, EditorView, Highlight, Index2, LineNumbers, Lines,
};
use ratatui::style::{Color, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterMode {
    Normal,
    Insert,
    VisualCharacter,
    VisualLine,
    Search,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowOnFeature {
    VisualBlock,
    NamedRegisters,
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
    FollowOnRequired(FollowOnFeature),
    Ignored,
}

/// Public widget state used by the project-owned compound history.
#[derive(Clone)]
pub struct EditorSnapshot {
    text: String,
    state: EditorState,
    viewport: (usize, usize),
    reference_ranges: Vec<TextRange>,
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
}

impl EditorSession {
    pub const FOLLOW_ON_FEATURES: [FollowOnFeature; 2] = [
        FollowOnFeature::VisualBlock,
        FollowOnFeature::NamedRegisters,
    ];

    pub fn new(text: impl AsRef<str>) -> Self {
        Self {
            state: EditorState::new(Lines::from(text.as_ref())),
            events: EditorEventHandler::vim_mode(),
            command_line: None,
            reference_ranges: Vec::new(),
            reference_style: Style::default().fg(Color::Cyan),
        }
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

    pub fn follow_on_features(&self) -> &'static [FollowOnFeature] {
        &Self::FOLLOW_ON_FEATURES
    }

    /// Returns the standard tg editor view: relative numbers with an absolute
    /// number on the cursor row.
    pub fn view(&mut self) -> EditorView<'_, '_> {
        EditorView::new(&mut self.state).line_numbers(LineNumbers::Relative)
    }

    pub fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            text: self.text(),
            state: self.state.clone(),
            viewport: self.state.viewport_offset(),
            reference_ranges: self.reference_ranges.clone(),
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
        self.set_reference_ranges(snapshot.reference_ranges.clone())
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
        let highlights = reference_highlights(&self.text(), &ranges, self.reference_style)?;
        self.reference_ranges = ranges;
        self.state.set_highlights(highlights);
        Ok(())
    }

    pub fn handle_event(&mut self, event: Event, completion_active: bool) -> EditorInput {
        if let Event::Key(key) = event {
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                return EditorInput::Ignored;
            }
            if self.command_line.is_some() {
                return self.handle_command_key(key);
            }
            if completion_active && is_ctrl_p(key) {
                return EditorInput::TogglePreview;
            }
            if matches!(self.state.mode, EditorMode::Normal | EditorMode::Visual) {
                if is_ctrl_v(key) {
                    return EditorInput::FollowOnRequired(FollowOnFeature::VisualBlock);
                }
                if key.code == KeyCode::Char('"') {
                    return EditorInput::FollowOnRequired(FollowOnFeature::NamedRegisters);
                }
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

    fn rebuild_highlights(&mut self) -> Result<()> {
        let highlights =
            reference_highlights(&self.text(), &self.reference_ranges, self.reference_style)?;
        self.state.set_highlights(highlights);
        Ok(())
    }
}

impl Default for EditorSession {
    fn default() -> Self {
        Self::new("")
    }
}

fn is_ctrl_p(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('p') && key.modifiers == KeyModifiers::CONTROL
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
            EditorInput::FollowOnRequired(FollowOnFeature::VisualBlock)
        );
        assert_eq!(
            session.handle_event(key(KeyCode::Char('"')), false),
            EditorInput::FollowOnRequired(FollowOnFeature::NamedRegisters)
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
        assert_eq!(
            session.follow_on_features(),
            &[
                FollowOnFeature::VisualBlock,
                FollowOnFeature::NamedRegisters
            ]
        );
    }
}
