use super::*;

impl EditorSession {
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
                if key.code == KeyCode::Char('v') && key.modifiers == KeyModifiers::CONTROL {
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

    pub(super) fn delegate(&mut self, event: Event) -> EditorInput {
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

    pub(super) fn combined_count(&mut self) -> usize {
        self.operator_count.max(1).saturating_mul(self.take_count())
    }

    pub(super) fn reset_operator(&mut self) {
        self.count = 0;
        self.operator_count = 0;
        self.pending_operator = None;
        self.pending_text_object = None;
    }

    pub(super) fn cancel_operator(&mut self) {
        self.reset_operator();
        self.selected_register = '"';
    }

    pub(super) fn reset_pending_input(&mut self) {
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
            KeyCode::Enter => EditorInput::CommandSubmitted(
                self.command_line.take().expect("command mode checked"),
            ),
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
}

impl KeyBinding {
    fn matches(self, key: KeyEvent) -> bool {
        self.code == key.code && self.modifiers == key.modifiers
    }
}

pub(super) fn parse_key_binding(notation: &str) -> Result<KeyBinding> {
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
