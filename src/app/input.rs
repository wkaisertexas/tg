use super::*;
use crate::editor::{AdapterMode, EditorInput, TextEdit};
use crate::references::model::TextRange;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

impl App {
    pub(super) fn sync_widget_edit(&mut self) -> Result<()> {
        let after = self.editor.text();
        if after == self.document.text() {
            return Ok(());
        }
        let edit = single_edit(self.document.text(), &after);
        self.document.apply(&[edit])?;
        self.sync_reference_state()?;
        self.refresh_activation()
    }

    pub(super) fn replace_widget_from_document(&mut self) -> Result<()> {
        self.editor.replace_text_and_ranges(
            self.document.text(),
            self.document
                .references()
                .iter()
                .map(|reference| reference.range),
        )
    }

    pub(super) fn open_providers(&mut self) {
        self.reference_session.close();
        self.preview = None;
        self.help_visible = false;
        self.help_pending = false;
        self.providers.open();
    }

    fn handle_help_key(&mut self, key: event::KeyEvent) -> bool {
        if self.help_visible {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q' | '?')) {
                self.help_visible = false;
            }
            return true;
        }
        if self.editor.mode() != AdapterMode::Normal || self.editor.command_line().is_some() {
            self.help_pending = false;
            return false;
        }
        if self.help_pending {
            self.help_pending = false;
            if key.code == KeyCode::Char('p') && key.modifiers.is_empty() {
                self.open_providers();
                return true;
            }
            if key.code == KeyCode::Char('?')
                && matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT)
            {
                self.help_visible = true;
                self.reference_session.close();
                self.preview = None;
                return true;
            }
        }
        if key.code == KeyCode::Char(' ') && key.modifiers.is_empty() {
            self.help_pending = true;
            return true;
        }
        false
    }

    pub(super) fn handle_event(&mut self, event: Event) {
        if self.providers.visible {
            if let Event::Key(key) = event
                && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            {
                self.providers.handle_key(key);
            }
            return;
        }
        if let Event::Key(key) = event
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && self.handle_help_key(key)
        {
            return;
        }
        if let Event::Key(key) = event
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && self.completion_active()
        {
            match key.code {
                KeyCode::Tab | KeyCode::Enter => {
                    self.accept_selected();
                    return;
                }
                KeyCode::Up => {
                    self.reference_session.select_previous();
                    self.schedule_preview();
                    return;
                }
                KeyCode::Down => {
                    self.reference_session.select_next();
                    self.schedule_preview();
                    return;
                }
                KeyCode::Char('j') if key.modifiers == KeyModifiers::CONTROL => {
                    self.reference_session.select_next();
                    self.schedule_preview();
                    return;
                }
                KeyCode::Char('k') if key.modifiers == KeyModifiers::CONTROL => {
                    self.reference_session.select_previous();
                    self.schedule_preview();
                    return;
                }
                _ => {}
            }
        }
        let was_grouped = matches!(
            self.editor.mode(),
            AdapterMode::Insert | AdapterMode::Replace
        );
        match self.editor.handle_event(event, self.completion_active()) {
            EditorInput::Delegated { text_changed: true } => {
                if let Err(error) = self.sync_widget_edit() {
                    self.status = format!("Edit failed: {error}");
                }
            }
            EditorInput::Delegated {
                text_changed: false,
            } => {
                let _ = self.refresh_activation();
            }
            EditorInput::TogglePreview => {
                if self.preview_mode == PreviewMode::Disabled {
                    self.status = "Preview disabled".into();
                } else {
                    self.preview_visible = !self.preview_visible;
                    if self.preview_visible {
                        self.request_preview();
                    } else {
                        self.preview = None;
                    }
                }
            }
            EditorInput::CommandSubmitted(command) => self.dispatch_command(command),
            EditorInput::Undo => {
                if self.document.undo() {
                    let _ = self
                        .replace_widget_from_document()
                        .and_then(|_| self.sync_reference_state());
                }
            }
            EditorInput::Redo => {
                if self.document.redo() {
                    let _ = self
                        .replace_widget_from_document()
                        .and_then(|_| self.sync_reference_state());
                }
            }
            EditorInput::CommandCancelled => self.status.clear(),
            EditorInput::CommandStarted | EditorInput::CommandUpdated | EditorInput::Ignored => {}
        }
        let is_grouped = matches!(
            self.editor.mode(),
            AdapterMode::Insert | AdapterMode::Replace
        );
        if !was_grouped && is_grouped && !self.edit_group_active {
            self.document.begin_insert_group();
            self.edit_group_active = true;
        } else if was_grouped && !is_grouped && self.edit_group_active {
            self.document.end_insert_group();
            self.edit_group_active = false;
        }
    }
}

pub(super) fn single_edit(before: &str, after: &str) -> TextEdit {
    let before_chars: Vec<_> = before.chars().collect();
    let after_chars: Vec<_> = after.chars().collect();
    let prefix = before_chars
        .iter()
        .zip(&after_chars)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = before_chars[prefix..]
        .iter()
        .rev()
        .zip(after_chars[prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    TextEdit::new(
        TextRange {
            start: prefix,
            end: before_chars.len() - suffix,
        },
        after_chars[prefix..after_chars.len() - suffix]
            .iter()
            .collect::<String>(),
    )
}
