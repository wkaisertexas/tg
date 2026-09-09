use super::*;
use edtui::{EditorView, Highlight};
use ratatui::layout::{Position, Rect};

impl EditorSession {
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

    pub fn view(&mut self) -> EditorView<'_, '_> {
        EditorView::new(&mut self.state)
            .line_numbers(self.line_numbers)
            .tab_width(self.tab_width)
            .wrap(self.wrap)
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

    pub(super) fn rebuild_highlights(&mut self) -> Result<()> {
        let mut highlights =
            reference_highlights(&self.text(), &self.reference_ranges, self.reference_style)?;
        highlights.extend(block_highlights(&self.text(), self.block));
        self.state.set_highlights(highlights);
        Ok(())
    }
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
