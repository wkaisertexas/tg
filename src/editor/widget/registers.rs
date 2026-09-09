use super::*;

impl EditorSession {
    pub(super) fn write_register(&mut self, value: RegisterValue) {
        self.registers.insert('"', value.clone());
        self.registers.insert(self.selected_register, value);
        self.selected_register = '"';
    }

    pub(super) fn current_register(&self) -> Option<RegisterValue> {
        self.registers
            .get(&self.selected_register)
            .or_else(|| self.registers.get(&'"'))
            .cloned()
    }

    pub fn register_text(&self, register: char) -> Option<String> {
        self.registers.get(&register).map(register_as_text)
    }

    pub(super) fn handle_visual_key(&mut self, key: KeyEvent) -> Option<EditorInput> {
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
            let start = position_to_char_index(&text, selection.start())?;
            let mut end = position_to_char_index(&text, selection.end())?;
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
        self.replace_char_range(range, &register_as_text(&value), range.start, false);
        self.selected_register = '"';
        EditorInput::Delegated { text_changed: true }
    }

    pub(super) fn paste_register(&mut self, before: bool) -> EditorInput {
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
}

pub(super) fn register_as_text(value: &RegisterValue) -> String {
    match value {
        RegisterValue::Character(text) | RegisterValue::Line(text) => text.clone(),
        RegisterValue::Block(rows) => rows.join("\n"),
    }
}
