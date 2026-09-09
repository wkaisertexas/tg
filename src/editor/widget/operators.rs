use super::*;

impl EditorSession {
    pub(super) fn apply_word_operator(&mut self, operator: char) -> EditorInput {
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

    pub(super) fn apply_line_operator(&mut self, operator: char) -> EditorInput {
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

    pub(super) fn repeat_event(&mut self, key: KeyEvent, count: usize) -> EditorInput {
        let before = self.text();
        for _ in 0..count.max(1) {
            self.delegate(Event::Key(key));
        }
        EditorInput::Delegated {
            text_changed: self.text() != before,
        }
    }

    pub(super) fn apply_text_object(
        &mut self,
        operator: char,
        kind: char,
        object: char,
    ) -> EditorInput {
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

pub(super) fn transform_chars(chars: &mut [char], operator: TextOperator) -> bool {
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
