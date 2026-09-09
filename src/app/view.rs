use super::help::styled_help_lines;
use super::*;
use crate::references::model::{ContextCost, ReferenceCandidate, ReferenceTarget};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

pub(super) fn candidate_text(candidate: &ReferenceCandidate, tokens: &TokenConfig) -> String {
    let mut text = candidate.display.primary.clone();
    if let Some(secondary) = &candidate.display.secondary {
        text.push_str("  ");
        text.push_str(secondary);
    }
    if candidate.kind == ReferenceKind::Symbol {
        if tokens.show_symbol
            && let Some(cost) = format_context_cost(&candidate.context_cost, tokens.decimals)
        {
            text.push_str(" · symbol ");
            text.push_str(&cost);
        }
        if tokens.show_file
            && let Some(cost) = candidate
                .file_context_cost
                .as_ref()
                .and_then(|cost| format_context_cost(cost, tokens.decimals))
        {
            text.push_str(" · file ");
            text.push_str(&cost);
        }
    } else if tokens.show_file
        && let Some(cost) = format_context_cost(&candidate.context_cost, tokens.decimals)
    {
        text.push_str(" · ");
        text.push_str(&cost);
    }
    text
}

pub(super) fn completion_title(kind: Option<ReferenceKind>) -> &'static str {
    match kind {
        Some(ReferenceKind::GitFile) => " Files ",
        Some(ReferenceKind::BroadFile) => " All Files ",
        Some(ReferenceKind::Symbol) => " Symbols ",
        Some(ReferenceKind::Skill) => " Skills ",
        Some(ReferenceKind::GitHubIssue) => " GitHub Issues ",
        Some(ReferenceKind::GitHubPullRequest) => " GitHub Pull Requests ",
        Some(ReferenceKind::JiraIssue) => " Jira Issues ",
        None => " Completion ",
    }
}

fn line_end_offset(text: &str, offset: usize) -> usize {
    let characters: Vec<_> = text.chars().collect();
    let offset = offset.min(characters.len());
    characters[offset..]
        .iter()
        .position(|character| *character == '\n')
        .map_or(characters.len(), |end| offset + end)
}

pub(super) fn token_inlay_hints(app: &App) -> Vec<(usize, String)> {
    if !app.token_config.show_file {
        return Vec::new();
    }
    let mut hints: Vec<(usize, String)> = Vec::new();
    for reference in app.document.references() {
        if !matches!(reference.target, ReferenceTarget::File(_)) {
            continue;
        }
        let Some(cost) = app.reference_session.reference_cost(reference) else {
            continue;
        };
        let Some(cost) = format_context_cost(&cost, app.token_config.decimals) else {
            continue;
        };
        let offset = line_end_offset(app.document.text(), reference.range.end);
        if let Some((_, hint)) = hints.iter_mut().find(|(existing, _)| *existing == offset) {
            hint.push_str(" · ");
            hint.push_str(&cost);
        } else {
            hints.push((offset, cost));
        }
    }
    hints
}

fn format_context_cost(cost: &ContextCost, decimals: u8) -> Option<String> {
    match cost {
        ContextCost::Pending => Some("…".into()),
        ContextCost::Tokens(tokens) => Some(format_token_count(*tokens, decimals)),
        ContextCost::Bytes(bytes) => Some(format!("{bytes} bytes")),
        ContextCost::None => None,
        ContextCost::Unavailable => Some("unavailable".into()),
    }
}

pub(super) fn status_is_sticky(status: &str) -> bool {
    status.starts_with("Cannot ")
        || status.starts_with("Write failed:")
        || status.starts_with("Save failed:")
        || status.starts_with("Command failed:")
        || status.starts_with("Command output could not be inserted:")
}

pub(super) fn status_text(app: &App, width: u16) -> String {
    let mode = format!("{:?}", app.editor.mode()).to_uppercase();
    let path = app
        .save_target
        .as_ref()
        .map_or("[No Name]".into(), |target| {
            target.logical_path().display().to_string()
        });
    let dirty = if app.document.is_dirty() { " [+]" } else { "" };
    let cursor = app.editor.state().cursor;
    let location = format!("{}:{}", cursor.row + 1, cursor.col + 1);
    let refs = app.token_config.show_total.then(|| {
        format!(
            "refs {}",
            format_context_total(app.refs_total, app.token_config.decimals)
        )
    });
    let issues = app.providers.problem_count();
    let provider_hint = if issues == 0 {
        String::new()
    } else {
        format!("{issues} provider issue(s) · Space p")
    };
    let message = if app.status.is_empty() {
        provider_hint.as_str()
    } else {
        app.status.as_str()
    };
    let full = [
        mode.as_str(),
        &format!("{path}{dirty}"),
        message,
        refs.as_deref().unwrap_or(""),
        &location,
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("  ");
    if full.chars().count() <= usize::from(width) {
        return format!(" {full}");
    }
    if !message.is_empty() {
        let available = usize::from(width).saturating_sub(mode.len() + location.len() + 5);
        let message = if issues > 0 && message.chars().count() > available {
            format!("{issues} issue(s) · Space p")
        } else {
            message.to_owned()
        };
        let message: String = message.chars().take(available).collect();
        return format!(" {mode}  {message} {location}");
    }
    let compact = [mode.as_str(), refs.as_deref().unwrap_or(""), &location]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("  ");
    if compact.chars().count() <= usize::from(width) {
        format!(" {compact}")
    } else {
        format!(" {mode} {location}")
    }
}

fn preview_lines(app: &App) -> Vec<Line<'static>> {
    let selected = app.reference_session.selected();
    let Some(cached) = app
        .preview
        .as_ref()
        .filter(|cached| selected.is_some_and(|candidate| candidate.id == cached.candidate_id))
    else {
        return vec![Line::from("Loading preview…")];
    };
    let Some(preview) = &cached.preview else {
        return vec![Line::from("Preview unavailable")];
    };
    preview
        .lines
        .iter()
        .map(|line| {
            Line::from(match line.number {
                Some(number) => format!("{number:>5} │ {}", line.text),
                None => line.text.clone(),
            })
        })
        .collect()
}

pub(super) fn overlay_area(area: Rect, width_percent: u8, height: u16) -> Rect {
    let width =
        ((u32::from(area.width) * u32::from(width_percent) / 100) as u16).clamp(1, area.width);
    let height = height.clamp(1, area.height.saturating_sub(1).max(1));
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

pub(super) fn draw(frame: &mut ratatui::Frame, app: &mut App) {
    if app.providers.visible {
        app.providers.draw(frame, frame.area(), app.color);
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());
    frame.render_widget(app.editor.view(), rows[0]);
    for (offset, hint) in token_inlay_hints(app) {
        let Some(position) = app.editor.virtual_text_position(offset, rows[0]) else {
            continue;
        };
        let x = position.x.saturating_add(1);
        let width = rows[0].right().saturating_sub(x);
        if width == 0 {
            continue;
        }
        frame.render_widget(
            Paragraph::new(format!("{hint} tokens")).style(if app.color {
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC)
            } else {
                Style::default().add_modifier(Modifier::DIM | Modifier::ITALIC)
            }),
            Rect::new(x, position.y, width, 1),
        );
    }
    if let Some((cursor, gutter_width)) = app.editor.current_line_number_override() {
        frame.render_widget(
            Paragraph::new("0").style(if app.color {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            }),
            Rect::new(rows[0].x, cursor.y, gutter_width, 1),
        );
    }
    let command = app
        .editor
        .command_line()
        .map(|line| format!(":{line}"))
        .unwrap_or_default();
    let status = if command.is_empty() {
        status_text(app, rows[1].width)
    } else {
        command
    };
    frame.render_widget(
        Paragraph::new(status).style(if app.color {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        }),
        rows[1],
    );
    if app.completion_active() {
        let area = overlay_area(rows[0], app.completion_width_percent, app.completion_height);
        frame.render_widget(Clear, area);
        let show_preview = app.preview_visible && area.width >= 60 && area.height >= 6;
        let columns = if show_preview {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                .split(area)
        } else {
            Layout::default()
                .constraints([Constraint::Percentage(100)])
                .split(area)
        };
        let selected = app.reference_session.selected_index();
        let items = app
            .reference_session
            .candidates()
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let style = if index == selected && app.color {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else if index == selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                ListItem::new(candidate_text(candidate, &app.token_config)).style(style)
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            List::new(items).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(completion_title(app.reference_session.active_kind())),
            ),
            columns[0],
        );
        if show_preview {
            frame.render_widget(
                Paragraph::new(preview_lines(app))
                    .scroll((app.preview_scroll as u16, 0))
                    .wrap(Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL).title(" Preview ")),
                columns[1],
            );
        }
    }
    if app.help_visible {
        let height = u16::try_from(app.help_lines.len() + 2).unwrap_or(u16::MAX);
        let area = overlay_area(rows[0], 90, height);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(styled_help_lines(&app.help_lines, app.color))
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(if app.color {
                            Style::default().fg(Color::Cyan)
                        } else {
                            Style::default()
                        })
                        .title(Line::from(" Configuration · Esc/q/? to close ").style(
                            if app.color {
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().add_modifier(Modifier::BOLD)
                            },
                        )),
                ),
            area,
        );
    }
}

fn format_context_total(total: ContextTotal, decimals: u8) -> String {
    let mut text = format_token_count(total.ready_tokens, decimals);
    if total.pending > 0 {
        text.push_str(" + …");
    }
    if total.unavailable > 0 {
        text.push_str(" + unavailable");
    }
    text
}

fn format_token_count(tokens: usize, decimals: u8) -> String {
    if tokens < 1_000 {
        tokens.to_string()
    } else {
        format!("{:.*}k", usize::from(decimals), tokens as f64 / 1_000.0)
    }
}
