use super::operators::transform_chars;
use super::registers::register_as_text;
use super::*;

impl EditorSession {
    pub(super) fn handle_block_key(&mut self, key: KeyEvent) -> EditorInput {
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

    pub(super) fn handle_block_insert_key(&mut self, key: KeyEvent) -> EditorInput {
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

    pub(super) fn insert_block_text(&mut self, inserted: &str) -> EditorInput {
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
}
