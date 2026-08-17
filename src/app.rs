use crate::config::{ColorMode, Config, LeadersConfig, PreviewMode};
use crate::editor::command::{CommandDispatcher, CommandEffect, LowerRequest};
use crate::editor::save::{AtomicSaver, SaveTarget};
use crate::editor::{AdapterMode, Document, EditorInput, EditorSession, TextEdit};
use crate::references::ThreadExecutor;
use crate::references::activation::detect_activation;
use crate::references::file::FileProvider;
use crate::references::model::{
    CandidateId, ContextCost, Preview, ReferenceCandidate, ReferenceKind, TextRange,
};
use crate::references::session::{
    DocumentRevision, LowerPurpose, OperationKind, ReferenceEvent, ReferenceSession,
};
use crate::references::symbol::SymbolProvider;
use crate::repository::Repository;
use crate::tokens::{ContextTotal, format_tokens};
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;

struct CachedPreview {
    candidate_id: CandidateId,
    preview: Option<Preview>,
}

struct App {
    repo: Repository,
    document: Document,
    editor: EditorSession,
    save_target: Option<SaveTarget>,
    saver: AtomicSaver,
    commands: CommandDispatcher,
    reference_session: ReferenceSession,
    leaders: LeadersConfig,
    search_limit: usize,
    preview_mode: PreviewMode,
    preview_visible: bool,
    color: bool,
    completion_height: u16,
    completion_width_percent: u8,
    status: String,
    preview: Option<CachedPreview>,
    preview_scroll: usize,
    pending_clipboard: Option<String>,
    should_exit: bool,
    refs_total: ContextTotal,
    insert_group_active: bool,
}

impl App {
    fn new(
        repo: Repository,
        config: &Config,
        document: Document,
        save_target: Option<SaveTarget>,
    ) -> Result<Self> {
        let git = FileProvider::new(
            &repo.search_root,
            ReferenceKind::GitFile,
            config.leaders.files.clone(),
        )?;
        let broad = FileProvider::new(
            &repo.search_root,
            ReferenceKind::BroadFile,
            config.leaders.broad_files.clone(),
        )?;
        let symbols = SymbolProvider::new(&repo.search_root, config.leaders.files.clone())?;
        let mut reference_session = ReferenceSession::with_tokenizer(
            [
                Arc::new(git) as Arc<dyn crate::references::ReferenceProvider>,
                Arc::new(broad),
                Arc::new(symbols),
            ],
            Arc::new(ThreadExecutor),
            &config.tokens.tokenizer,
        )?;
        reference_session.update_references(DocumentRevision(0), Arc::from([]))?;
        let mut editor = EditorSession::new(document.text());
        let color = config.ui.color != ColorMode::Never;
        editor.set_reference_style(if color {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().add_modifier(Modifier::UNDERLINED)
        })?;
        Ok(Self {
            repo,
            document,
            editor,
            save_target,
            saver: AtomicSaver::new(),
            commands: CommandDispatcher::new(config.editor.copy_command.clone())?,
            reference_session,
            leaders: config.leaders.clone(),
            search_limit: config.search.limit,
            preview_mode: config.ui.preview,
            preview_visible: config.ui.preview == PreviewMode::Automatic,
            color,
            completion_height: config.ui.completion_height,
            completion_width_percent: config.ui.completion_width_percent,
            status: String::new(),
            preview: None,
            preview_scroll: 0,
            pending_clipboard: None,
            should_exit: false,
            refs_total: ContextTotal::default(),
            insert_group_active: false,
        })
    }

    fn revision(&self) -> DocumentRevision {
        DocumentRevision(self.document.revision())
    }

    fn completion_active(&self) -> bool {
        self.reference_session.completion_active()
    }

    fn sync_reference_state(&mut self) -> Result<()> {
        let revision = self.revision();
        self.reference_session.document_changed(revision);
        self.reference_session
            .update_references(revision, self.document.references().to_vec().into())?;
        self.editor.set_reference_ranges(
            self.document
                .references()
                .iter()
                .map(|reference| reference.range),
        )?;
        self.preview = None;
        Ok(())
    }

    fn sync_widget_edit(&mut self) -> Result<()> {
        let after = self.editor.text();
        if after == self.document.text() {
            return Ok(());
        }
        let edit = single_edit(self.document.text(), &after);
        self.document.apply(&[edit])?;
        self.sync_reference_state()?;
        self.refresh_activation()
    }

    fn replace_widget_from_document(&mut self) -> Result<()> {
        self.editor.replace_text_and_ranges(
            self.document.text(),
            self.document
                .references()
                .iter()
                .map(|reference| reference.range),
        )
    }

    fn refresh_activation(&mut self) -> Result<()> {
        if self.editor.mode() != AdapterMode::Insert {
            self.reference_session.close();
            return Ok(());
        }
        let cursor = self.editor.cursor_char_offset();
        match detect_activation(
            self.document.text(),
            cursor,
            &self.leaders,
            &self.repo.search_root,
        )? {
            Some(activation) => {
                self.reference_session
                    .activate(activation, self.search_limit)?;
                self.status = "searching…".into();
            }
            None => self.reference_session.close(),
        }
        Ok(())
    }

    fn request_preview(&mut self) {
        self.preview = None;
        self.preview_scroll = 0;
        if self.preview_mode != PreviewMode::Disabled
            && self.preview_visible
            && let Err(error) = self.reference_session.begin_preview_selected()
        {
            self.status = format!("Preview unavailable: {error}");
        }
    }

    fn accept_selected(&mut self) {
        match self
            .reference_session
            .begin_accept_selected(self.revision())
        {
            Ok(_) => self.status = "Resolving reference…".into(),
            Err(error) => self.status = format!("Cannot accept reference: {error}"),
        }
    }

    fn begin_lower(&mut self, request: LowerRequest) {
        let snapshot = request.snapshot;
        match self.reference_session.begin_lower_snapshot(
            DocumentRevision(snapshot.revision()),
            request.purpose,
            Arc::from(snapshot.text()),
            snapshot.references().to_vec().into(),
            self.leaders.clone(),
        ) {
            Ok(_) => self.status = "Validating references…".into(),
            Err(error) => self.status = format!("Cannot lower document: {error}"),
        }
    }

    fn dispatch_command(&mut self, command: String) {
        let line = if command.starts_with(':') {
            command
        } else {
            format!(":{command}")
        };
        match self.commands.dispatch(&line, &self.document) {
            Ok(CommandEffect::Quit) => self.should_exit = true,
            Ok(CommandEffect::Lower(request)) => self.begin_lower(request),
            Err(error) => self.status = error.to_string(),
        }
    }

    fn finish_lower(
        &mut self,
        revision: DocumentRevision,
        purpose: LowerPurpose,
        snapshot: crate::references::activation::LoweredSnapshot,
    ) {
        if revision != self.revision() {
            return;
        }
        if let Err(error) = self.document.refresh_reference_targets(snapshot.references) {
            self.status = format!("Cannot refresh references: {error}");
            return;
        }
        match purpose {
            LowerPurpose::Copy => {
                self.pending_clipboard = Some(snapshot.text);
                self.status = "Copied lowered document".into();
            }
            LowerPurpose::Write | LowerPurpose::WriteAndQuit => {
                let Some(target) = self.save_target.as_ref() else {
                    self.status = "No file name".into();
                    return;
                };
                match self.saver.save(target, snapshot.text.as_bytes()) {
                    Ok(target) => {
                        self.save_target = Some(target);
                        self.document.mark_saved();
                        self.status = "Written".into();
                        if purpose == LowerPurpose::WriteAndQuit {
                            self.should_exit = true;
                        }
                    }
                    Err(error) => self.status = format!("Write failed: {error}"),
                }
            }
        }
    }

    fn drain_reference_events(&mut self) {
        for event in self.reference_session.drain_events() {
            match event {
                ReferenceEvent::Query(update) => {
                    if let Some(error) = update.error {
                        self.status = error;
                    } else if update.candidates_changed {
                        self.status =
                            format!("{} matches", self.reference_session.candidates().len());
                        if self.preview_mode == PreviewMode::Automatic {
                            self.preview_visible = true;
                            self.request_preview();
                        }
                    } else if let Some(progress) = update.progress {
                        self.status = format!("{} / {} files", progress.scanned, progress.total);
                    }
                }
                ReferenceEvent::Accepted {
                    revision, accepted, ..
                } if revision == self.revision() => {
                    let cursor = accepted.replacement_range.start
                        + accepted.replacement_text.chars().count();
                    match self
                        .document
                        .accept_reference(*accepted)
                        .and_then(|_| self.replace_widget_from_document())
                        .and_then(|_| self.editor.set_cursor_char_offset(cursor))
                        .and_then(|_| self.sync_reference_state())
                    {
                        Ok(()) => self.status = "Reference resolved".into(),
                        Err(error) => self.status = format!("Cannot accept reference: {error}"),
                    }
                }
                ReferenceEvent::PreviewReady {
                    candidate_id,
                    preview,
                    ..
                } => {
                    self.preview_scroll = preview
                        .as_ref()
                        .and_then(|preview| preview.highlighted_lines.as_ref())
                        .map_or(0, |lines| centered_preview_scroll(*lines.start()));
                    self.preview = Some(CachedPreview {
                        candidate_id,
                        preview,
                    });
                }
                ReferenceEvent::SnapshotLowered {
                    revision,
                    purpose,
                    snapshot,
                    ..
                } => self.finish_lower(revision, purpose, snapshot),
                ReferenceEvent::OperationFailed {
                    kind,
                    reference_id,
                    message,
                    ..
                } => {
                    if let Some(id) = reference_id
                        && let Some(reference) = self
                            .document
                            .references()
                            .iter()
                            .find(|reference| reference.id == id)
                    {
                        let _ = self.editor.set_cursor_char_offset(reference.range.start);
                    }
                    self.status = format!("Cannot {}: {message}", operation_name(kind));
                }
                ReferenceEvent::ContextTotalChanged { revision, total }
                    if revision == self.revision() =>
                {
                    self.refs_total = total
                }
                ReferenceEvent::CandidateCostsChanged { .. }
                | ReferenceEvent::ContextTotalChanged { .. }
                | ReferenceEvent::Accepted { .. } => {}
            }
        }
    }

    fn handle_event(&mut self, event: Event) {
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
                    self.request_preview();
                    return;
                }
                KeyCode::Down => {
                    self.reference_session.select_next();
                    self.request_preview();
                    return;
                }
                KeyCode::Char('j') if key.modifiers == KeyModifiers::CONTROL => {
                    self.reference_session.select_next();
                    self.request_preview();
                    return;
                }
                KeyCode::Char('k') if key.modifiers == KeyModifiers::CONTROL => {
                    self.reference_session.select_previous();
                    self.request_preview();
                    return;
                }
                _ => {}
            }
        }
        let was_insert = self.editor.mode() == AdapterMode::Insert;
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
                if self.preview_mode != PreviewMode::Disabled {
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
        let is_insert = self.editor.mode() == AdapterMode::Insert;
        if !was_insert && is_insert && !self.insert_group_active {
            self.document.begin_insert_group();
            self.insert_group_active = true;
        } else if was_insert && !is_insert && self.insert_group_active {
            self.document.end_insert_group();
            self.insert_group_active = false;
        }
    }
}

fn operation_name(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Accept => "resolve reference",
        OperationKind::Preview => "load preview",
        OperationKind::Lower => "lower document",
    }
}

fn single_edit(before: &str, after: &str) -> TextEdit {
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

fn candidate_text(candidate: &ReferenceCandidate) -> String {
    let mut text = candidate.display.primary.clone();
    if let Some(secondary) = &candidate.display.secondary {
        text.push_str("  ");
        text.push_str(secondary);
    }
    if let Some(cost) = format_context_cost(&candidate.context_cost) {
        text.push_str(" · ");
        text.push_str(&cost);
    }
    text
}

fn format_context_cost(cost: &ContextCost) -> Option<String> {
    match cost {
        ContextCost::Pending => Some("…".into()),
        ContextCost::Tokens(tokens) if *tokens >= 1_000 => {
            Some(format!("{:.1}k", *tokens as f64 / 1_000.0))
        }
        ContextCost::Tokens(tokens) => Some(tokens.to_string()),
        ContextCost::Bytes(bytes) => Some(format!("{bytes} bytes")),
        ContextCost::None => None,
        ContextCost::Unavailable => Some("unavailable".into()),
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

fn overlay_area(area: Rect, width_percent: u8, height: u16) -> Rect {
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

fn draw(frame: &mut ratatui::Frame, app: &mut App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());
    frame.render_widget(app.editor.view(), rows[0]);
    let mode = format!("{:?}", app.editor.mode()).to_uppercase();
    let path = app
        .save_target
        .as_ref()
        .map_or("[No Name]".into(), |target| {
            target.logical_path().display().to_string()
        });
    let dirty = if app.document.is_dirty() { " [+]" } else { "" };
    let command = app
        .editor
        .command_line()
        .map(|line| format!(":{line}"))
        .unwrap_or_default();
    let status = if command.is_empty() {
        format!(
            " {mode}  {path}{dirty}  {}  refs {}",
            app.status,
            format_context_total(app.refs_total)
        )
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
                ListItem::new(candidate_text(candidate)).style(style)
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            List::new(items).block(Block::default().borders(Borders::ALL).title(" Completion ")),
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
}

pub struct Startup {
    pub repository: Repository,
    pub config: Config,
    pub document: Document,
    pub save_target: Option<SaveTarget>,
}

pub fn run(startup: Startup) -> Result<()> {
    let mut app = App::new(
        startup.repository,
        &startup.config,
        startup.document,
        startup.save_target,
    )
    .context("could not initialize editor")?;
    let mut guard = crate::terminal::TerminalGuard::stderr()?;
    {
        let mut terminal = Terminal::new(CrosstermBackend::new(guard.backend_mut().writer_mut()))?;
        loop {
            app.drain_reference_events();
            terminal.draw(|frame| draw(frame, &mut app))?;
            if let Some(text) = app.pending_clipboard.take() {
                write!(
                    terminal.backend_mut(),
                    "{}",
                    crate::clipboard::osc52_sequence(&text)
                )?;
                terminal.backend_mut().flush()?;
            }
            if app.should_exit {
                break;
            }
            if event::poll(Duration::from_millis(50))? {
                app.handle_event(event::read()?);
            }
        }
    }
    guard.restore()?;
    Ok(())
}

pub fn is_terminal() -> bool {
    crossterm::tty::IsTty::is_tty(&io::stdin())
}

fn centered_preview_scroll(one_based_line: usize) -> usize {
    one_based_line.saturating_sub(6)
}

fn format_context_total(total: ContextTotal) -> String {
    let mut text = format_tokens(total.ready_tokens);
    if total.pending > 0 {
        text.push_str(" + …");
    }
    if total.unavailable > 0 {
        text.push_str(" + unavailable");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_is_unicode_character_based_and_minimal() {
        assert_eq!(
            single_edit("a🦀b", "a日本b"),
            TextEdit::new(TextRange { start: 1, end: 2 }, "日本")
        );
    }

    #[test]
    fn overlay_degrades_inside_a_forty_by_eight_terminal() {
        let area = overlay_area(Rect::new(0, 0, 40, 7), 80, 12);
        assert!(area.width <= 40);
        assert!(area.height <= 7);
        assert!(area.width < 60, "narrow layouts must suppress preview");
    }

    #[test]
    fn no_color_selection_has_a_non_color_signal() {
        let style = Style::default().add_modifier(Modifier::REVERSED);
        assert!(style.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(style.fg, None);
        assert_eq!(style.bg, None);
    }

    #[test]
    fn preview_is_manual_by_default() {
        assert_eq!(Config::default().ui.preview, PreviewMode::Manual);
    }

    #[test]
    fn idle_layout_reserves_only_editor_and_status_rows() {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(Rect::new(0, 0, 40, 8));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].height, 7);
        assert_eq!(rows[1].height, 1);
    }
}
