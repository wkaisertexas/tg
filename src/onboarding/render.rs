use super::report::clean_text;
use super::*;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

impl ProviderPanel {
    fn content_lines(&self) -> Vec<String> {
        if let Some(kinds) = &self.confirmation {
            let mut lines = vec![
                "Test connection? This contacts the selected services using their CLI credentials."
                    .into(),
                String::new(),
            ];
            lines.extend(
                self.reports
                    .iter()
                    .filter(|report| kinds.contains(&report.kind))
                    .map(|report| {
                        format!(
                            "{} {}: {}",
                            report.leader,
                            report.name,
                            report.target.as_deref().unwrap_or("target resolved by CLI")
                        )
                    }),
            );
            lines.extend([
                String::new(),
                "Read-only requests. No login changes, browser launches, or token display.".into(),
                "Ctrl-C cancels a running check. Closing this view also cancels it.".into(),
            ]);
            return lines;
        }
        match self.page {
            Page::Details => self.reports.get(self.selection.selected().unwrap_or(0)).map(detail_lines).unwrap_or_default(),
            Page::Configuration => self.configuration.clone(),
            Page::QuickStart => vec![
                "1. Start with local references. No accounts needed.".into(),
                "Press i to insert text. Type a leader to search, then Tab or Enter to accept. Esc dismisses completion.".into(),
                format!("Try {}README.md or {}src/app.rs{}submit", self.config.leaders.files, self.config.leaders.files, self.config.leaders.symbols),
                format!("Standalone {} searches symbols across the repository.", self.config.leaders.symbols),
                format!("{} includes ignored files: check what you select.", self.config.leaders.broad_files),
                String::new(),
                "2. Enable the services you actually use.".into(),
                "Select a provider, press Enter for setup instructions, then r to test. Missing optional services never block local references.".into(),
                String::new(),
                "3. Return to editing.".into(),
                "Esc leaves Insert mode. :w saves, :wq saves and exits, :q! exits without saving. An unnamed buffer can be copied with :copy; use tg FILE to save to a file.".into(),
                "Space p or :providers reopens this view. Space ? shows all settings and bindings.".into(),
            ],
            Page::Overview => Vec::new(),
        }
    }

    pub fn draw(&mut self, frame: &mut ratatui::Frame, area: Rect, color: bool) {
        if area.width < 3 || area.height < 4 {
            return;
        }
        let area = if area.width >= 90 {
            let width = area.width.saturating_sub(4).min(110);
            Rect::new(
                area.x + (area.width - width) / 2,
                area.y,
                width,
                area.height,
            )
        } else {
            area
        };
        frame.render_widget(Clear, area);
        let title = if self.confirmation.is_some() {
            " Test connection "
        } else {
            match self.page {
                Page::Overview => " References & connections ",
                Page::Details => " Provider details ",
                Page::Configuration => " Configuration sources ",
                Page::QuickStart => " Getting started ",
            }
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(if color {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            });
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(u16::from(inner.height >= 6)),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .split(inner);
        let intro = if self.pending.is_some() {
            "Checking connections... Ctrl-C cancels."
        } else {
            "Local features first. Remote checks are opt-in."
        };
        frame.render_widget(
            Paragraph::new(intro).style(Style::default().add_modifier(Modifier::DIM)),
            rows[0],
        );
        self.page_height = rows[1].height.max(1);
        if self.page == Page::Overview && self.confirmation.is_none() {
            let items: Vec<_> = self
                .reports
                .iter()
                .map(|report| {
                    let style = if color && report.state.is_problem() {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    };
                    let text = if rows[1].width < 55 {
                        format!(
                            "{} {}\n   {}",
                            report.leader,
                            report.name,
                            report.state.label()
                        )
                    } else if rows[1].width >= 90 {
                        format!(
                            "{:<4} {:<24} {:<23} {}",
                            report.leader,
                            report.name,
                            report.state.label(),
                            report
                                .target
                                .as_deref()
                                .unwrap_or("Target resolved on check")
                        )
                    } else {
                        format!(
                            "{:<4} {:<24} {}",
                            report.leader,
                            report.name,
                            report.state.label()
                        )
                    };
                    ListItem::new(clean_text(&text)).style(style)
                })
                .collect();
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_symbol("> ")
                    .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
                rows[1],
                &mut self.selection,
            );
        } else {
            let lines = wrapped_lines(self.content_lines(), rows[1].width);
            self.max_scroll = lines
                .len()
                .saturating_sub(usize::from(rows[1].height))
                .min(usize::from(u16::MAX)) as u16;
            self.scroll = self.scroll.min(self.max_scroll);
            frame.render_widget(Paragraph::new(lines).scroll((self.scroll, 0)), rows[1]);
        }
        let footer = if self.confirmation.is_some() {
            "Enter/y test  Esc/n cancel\nj/k scroll"
        } else if self.page == Page::Overview {
            "Enter details  r test  R test all\nc config  ? quick start  q close"
        } else {
            "j/k scroll  r test  c config\n? quick start  Esc/q back"
        };
        frame.render_widget(
            Paragraph::new(footer).style(if color {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            }),
            rows[2],
        );
    }
}

pub(super) fn wrapped_lines(lines: Vec<String>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let mut wrapped = Vec::new();
    for line in lines {
        for line in clean_text(&line).split('\n') {
            let mut current = String::new();
            for word in line.split_inclusive(' ') {
                if !current.trim().is_empty()
                    && Line::raw(format!("{current}{word}")).width() > width
                {
                    wrapped.push(Line::raw(current.trim_end().to_owned()));
                    current.clear();
                }
                for character in word.chars() {
                    let previous_len = current.len();
                    current.push(character);
                    if previous_len > 0 && Line::raw(current.as_str()).width() > width {
                        current.truncate(previous_len);
                        wrapped.push(Line::raw(std::mem::take(&mut current)));
                        if character != ' ' {
                            current.push(character);
                        }
                    }
                }
            }
            wrapped.push(Line::raw(current));
        }
    }
    wrapped
}
