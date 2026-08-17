use crate::config::{Config, LeadersConfig};
use crate::editor::Document;
use crate::references::ThreadExecutor;
use crate::references::activation::{CharReplacement, apply_char_replacements, detect_activation};
use crate::references::file::FileProvider;
use crate::references::model::{
    CandidateId, ContextCost, Preview, ReferenceCandidate, ReferenceKind, ResolvedReference,
    TextRange,
};
use crate::references::session::{
    DocumentRevision, LowerPurpose, OperationKind, ReferenceEvent, ReferenceSession,
};
use crate::references::symbol::SymbolProvider;
use crate::repository::Repository;
use crate::tokens::{ContextTotal, format_tokens};
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;

struct CachedPreview {
    candidate_id: CandidateId,
    preview: Option<Preview>,
}

struct App {
    repo: Repository,
    text: String,
    references: Vec<ResolvedReference>,
    revision: DocumentRevision,
    reference_session: ReferenceSession,
    leaders: LeadersConfig,
    search_limit: usize,
    status: String,
    preview: Option<CachedPreview>,
    preview_scroll: usize,
    transcript: Vec<String>,
    transcript_scroll: usize,
    pending_clipboard: Option<String>,
    submitting: bool,
    exit_after_submit: bool,
    should_exit: bool,
    refs_total: ContextTotal,
}

impl App {
    /// Provider construction may walk the repository, so callers construct the
    /// app before entering raw mode. Everything after this boundary is driven
    /// through `ReferenceSession` worker jobs.
    fn new(repo: Repository, config: &Config) -> Result<Self> {
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
        let indexed_files = broad.files().len();
        let symbols = SymbolProvider::new(&repo.search_root, config.leaders.files.clone())?;
        let reference_session = ReferenceSession::with_tokenizer(
            [
                Arc::new(git) as Arc<dyn crate::references::ReferenceProvider>,
                Arc::new(broad),
                Arc::new(symbols),
            ],
            Arc::new(ThreadExecutor),
            &config.tokens.tokenizer,
        )?;
        let status = format!(
            "{indexed_files} files indexed · {} mode",
            if repo.git_aware {
                "Git-aware references"
            } else {
                "no Git repository; local ignores apply"
            }
        );
        Ok(Self {
            repo,
            text: String::new(),
            references: Vec::new(),
            revision: DocumentRevision(0),
            reference_session,
            leaders: config.leaders.clone(),
            search_limit: config.search.limit,
            status,
            preview: None,
            preview_scroll: 0,
            transcript: Vec::new(),
            transcript_scroll: 0,
            pending_clipboard: None,
            submitting: false,
            exit_after_submit: false,
            should_exit: false,
            refs_total: ContextTotal::default(),
        })
    }

    fn completion_open(&self) -> bool {
        !self.reference_session.candidates().is_empty()
    }

    fn edit_changed(&mut self) {
        self.revision.0 = self.revision.0.wrapping_add(1);
        self.reference_session.document_changed(self.revision);
        if let Err(error) = self
            .reference_session
            .update_references(self.revision, self.references.clone().into())
        {
            self.status = format!("Cannot update reference total: {error}");
        }
        self.preview = None;
        self.submitting = false;
        self.exit_after_submit = false;
    }

    fn refresh_activation(&mut self) {
        let cursor = self.text.chars().count();
        match detect_activation(&self.text, cursor, &self.leaders, &self.repo.search_root) {
            Ok(Some(activation)) => {
                let label = match activation.kind {
                    ReferenceKind::GitFile => "@ GIT",
                    ReferenceKind::BroadFile => "% BROAD",
                    ReferenceKind::Symbol => "SYMBOLS",
                    _ => "REFERENCES",
                };
                match self
                    .reference_session
                    .activate(activation, self.search_limit)
                {
                    Ok(_) => self.status = format!("{label} SEARCH · searching…"),
                    Err(error) => self.status = error.to_string(),
                }
            }
            Ok(None) => self.reference_session.close(),
            Err(error) => self.status = error.to_string(),
        }
    }

    fn request_preview(&mut self) {
        self.preview = None;
        self.preview_scroll = 0;
        if let Err(error) = self.reference_session.begin_preview_selected() {
            self.status = format!("Preview unavailable: {error}");
        }
    }

    fn drain_reference_events(&mut self) {
        for event in self.reference_session.drain_events() {
            match event {
                ReferenceEvent::Query(update) => {
                    if let Some(error) = update.error {
                        self.status = error;
                    } else if update.candidates_changed {
                        self.status = format!(
                            "{} matches · Tab/Enter selects",
                            self.reference_session.candidates().len()
                        );
                        if self.reference_session.selected().is_some() {
                            self.request_preview();
                        }
                    } else if let Some(progress) = update.progress {
                        self.status = format!(
                            "{} / {} files · {} symbols",
                            progress.scanned, progress.total, progress.indexed_symbols
                        );
                    }
                }
                ReferenceEvent::Accepted {
                    revision, accepted, ..
                } if revision == self.revision => {
                    if let Err(error) = self.apply_accepted(*accepted) {
                        self.status = format!("Cannot accept reference: {error}");
                    }
                }
                ReferenceEvent::PreviewReady {
                    candidate_id,
                    preview,
                    ..
                } => {
                    self.preview_scroll = preview
                        .as_ref()
                        .and_then(|value| value.highlighted_lines.as_ref())
                        .map_or(0, |lines| centered_preview_scroll(*lines.start()));
                    self.preview = Some(CachedPreview {
                        candidate_id,
                        preview,
                    });
                }
                ReferenceEvent::SnapshotLowered {
                    revision,
                    purpose: LowerPurpose::Copy,
                    snapshot,
                    ..
                } if revision == self.revision => self.finish_submit(snapshot.text),
                ReferenceEvent::OperationFailed {
                    kind,
                    reference_id,
                    message,
                    ..
                } => {
                    if kind == OperationKind::Lower {
                        self.submitting = false;
                        self.exit_after_submit = false;
                    }
                    if let Some(reference_id) = reference_id {
                        self.status = format!(
                            "Cannot {} reference {}: {message}",
                            operation_name(kind),
                            reference_id.0
                        );
                    } else {
                        self.status = format!("Cannot {}: {message}", operation_name(kind));
                    }
                }
                ReferenceEvent::CandidateCostsChanged { .. } => {}
                ReferenceEvent::ContextTotalChanged { revision, total }
                    if revision == self.revision =>
                {
                    self.refs_total = total;
                }
                _ => {}
            }
        }
    }

    fn apply_accepted(
        &mut self,
        accepted: crate::references::model::AcceptedReference,
    ) -> Result<()> {
        self.text = apply_char_replacements(
            &self.text,
            &[CharReplacement {
                range: accepted.replacement_range,
                text: accepted.replacement_text,
            }],
        )?;
        self.references
            .retain(|reference| !ranges_overlap(reference.range, accepted.replacement_range));
        self.references.push(accepted.reference);
        self.references
            .sort_by_key(|reference| reference.range.start);
        self.edit_changed();
        self.status = "Resolved ✓ · type :: after a file for symbols · Ctrl-J submits".into();
        Ok(())
    }

    fn active_unresolved_reference(&self) -> Result<bool> {
        let cursor = self.text.chars().count();
        let Some(activation) =
            detect_activation(&self.text, cursor, &self.leaders, &self.repo.search_root)?
        else {
            return Ok(false);
        };
        Ok(!self.references.iter().any(|reference| {
            reference.range == activation.replacement_range
                && reference.friendly_text
                    == char_slice(&self.text, activation.replacement_range).unwrap_or_default()
        }))
    }

    fn begin_submit(&mut self) {
        match self.active_unresolved_reference() {
            Ok(true) => {
                self.status = "Resolve or remove the active reference before submitting".into();
            }
            Err(error) => self.status = format!("Cannot submit: {error}"),
            Ok(false) => {
                let text: Arc<str> = Arc::from(self.text.clone());
                let references: Arc<[ResolvedReference]> = self.references.clone().into();
                match self.reference_session.begin_lower_snapshot(
                    self.revision,
                    LowerPurpose::Copy,
                    text,
                    references,
                    self.leaders.clone(),
                ) {
                    Ok(_) => {
                        self.submitting = true;
                        self.status = "Validating references…".into();
                    }
                    Err(error) => self.status = format!("Cannot submit: {error}"),
                }
            }
        }
    }

    fn finish_submit(&mut self, lowered: String) {
        let should_exit = self.exit_after_submit;
        self.submitting = false;
        self.transcript.push(lowered.clone());
        self.pending_clipboard = Some(lowered);
        self.transcript_scroll = self.transcript.len().saturating_sub(3);
        self.text.clear();
        self.references.clear();
        self.edit_changed();
        self.status = "Submitted ✓ · copied with OSC 52 · ready for another prompt".into();
        if should_exit {
            self.should_exit = true;
        }
    }

    fn accept_selected(&mut self) {
        match self.reference_session.begin_accept_selected(self.revision) {
            Ok(_) => self.status = "Resolving reference…".into(),
            Err(error) => self.status = format!("Cannot accept reference: {error}"),
        }
    }

    fn key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('d') if self.text.is_empty() => return true,
                KeyCode::Char('d') if self.submitting => {
                    self.exit_after_submit = true;
                    self.status = "Finishing submission before exit…".into();
                    return false;
                }
                KeyCode::Char('c') => {
                    if self.completion_open() {
                        self.reference_session.close();
                        self.preview = None;
                        self.status = "Completion dismissed".into();
                    } else if !self.text.is_empty() {
                        self.text.clear();
                        self.references.clear();
                        self.edit_changed();
                        self.status = "Composer cleared; Ctrl-C again exits".into();
                    } else {
                        return true;
                    }
                    return false;
                }
                KeyCode::Char('j') => {
                    self.begin_submit();
                    return false;
                }
                _ => {}
            }
        }
        match key.code {
            code if self.completion_open() && is_completion_accept_key(code) => {
                self.accept_selected()
            }
            KeyCode::Enter => self.begin_submit(),
            KeyCode::Up if self.completion_open() => {
                self.reference_session.select_previous();
                self.request_preview();
            }
            KeyCode::Down if self.completion_open() => {
                self.reference_session.select_next();
                self.request_preview();
            }
            KeyCode::PageUp if !self.completion_open() => {
                self.transcript_scroll = self.transcript_scroll.saturating_sub(3)
            }
            KeyCode::PageDown if !self.completion_open() => {
                self.transcript_scroll =
                    (self.transcript_scroll + 3).min(self.transcript.len().saturating_sub(1))
            }
            KeyCode::PageUp => self.preview_scroll = self.preview_scroll.saturating_sub(5),
            KeyCode::PageDown => self.preview_scroll += 5,
            KeyCode::Esc => {
                self.reference_session.close();
                self.preview = None;
                self.status =
                    "Completion dismissed; reference text remains literal until resolved".into();
            }
            KeyCode::Backspace => {
                if self.text.pop().is_some() {
                    let length = self.text.chars().count();
                    self.references
                        .retain(|reference| reference.range.end <= length);
                    self.edit_changed();
                    self.refresh_activation();
                }
            }
            KeyCode::Char(character) => {
                self.text.push(character);
                self.edit_changed();
                self.refresh_activation();
            }
            _ => {}
        }
        false
    }
}

fn operation_name(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Accept => "resolve reference",
        OperationKind::Preview => "load preview",
        OperationKind::Lower => "submit",
    }
}

fn ranges_overlap(left: TextRange, right: TextRange) -> bool {
    left.start < right.end && right.start < left.end
}

fn char_slice(text: &str, range: TextRange) -> Option<&str> {
    let start = if range.start == text.chars().count() {
        text.len()
    } else {
        text.char_indices().nth(range.start)?.0
    };
    let end = if range.end == text.chars().count() {
        text.len()
    } else {
        text.char_indices().nth(range.end)?.0
    };
    text.get(start..end)
}

fn preview_lines(app: &App) -> Vec<Line<'static>> {
    let selected = app.reference_session.selected();
    let Some(cached) = app
        .preview
        .as_ref()
        .filter(|cached| selected.is_some_and(|candidate| candidate.id == cached.candidate_id))
    else {
        return vec![Line::from(if selected.is_some() {
            "Loading preview…"
        } else {
            "Type a configured reference leader to search."
        })];
    };
    let Some(preview) = &cached.preview else {
        return vec![Line::from("Preview unavailable")];
    };
    preview
        .lines
        .iter()
        .map(|line| {
            let text = match line.number {
                Some(number) => format!("{number:>5} │ {}", line.text),
                None => line.text.clone(),
            };
            let highlighted = line.number.is_some_and(|number| {
                preview
                    .highlighted_lines
                    .as_ref()
                    .is_some_and(|range| range.contains(&number))
            });
            if highlighted {
                Line::styled(
                    text,
                    Style::default().fg(Color::Black).bg(Color::LightYellow),
                )
            } else {
                Line::from(text)
            }
        })
        .collect()
}

fn candidate_text(candidate: &ReferenceCandidate) -> String {
    let mut text = candidate.display.primary.clone();
    if let Some(secondary) = &candidate.display.secondary {
        text.push_str("  ");
        text.push_str(secondary);
    }
    let context = format_context_cost(&candidate.context_cost);
    let file = candidate
        .file_context_cost
        .as_ref()
        .and_then(format_context_cost);
    if let Some(context) = context {
        text.push_str(" · ");
        text.push_str(&context);
    }
    if let Some(file) = file {
        text.push_str(" · file ");
        text.push_str(&file);
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

fn draw(frame: &mut ratatui::Frame, app: &App) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Min(8),
            Constraint::Length(5),
            Constraint::Length(2),
        ])
        .split(frame.area());
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "  TS ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " CODE SELECTION  ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(app.repo.search_root.display().to_string()),
    ]))
    .block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(header, vertical[0]);
    let transcript = app
        .transcript
        .iter()
        .map(|line| {
            Line::from(vec![
                Span::styled("› ", Style::default().fg(Color::Cyan)),
                Span::raw(line.clone()),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(transcript)
            .scroll((app.transcript_scroll as u16, 0))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Transcript · PgUp/PgDn when idle ")
                    .borders(Borders::ALL),
            ),
        vertical[1],
    );
    let middle = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(vertical[2]);
    let selected = app.reference_session.selected_index();
    let items = app
        .reference_session
        .candidates()
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let style = if index == selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(candidate_text(candidate)).style(style)
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(Block::default().title(" Matches ").borders(Borders::ALL)),
        middle[0],
    );
    frame.render_widget(
        Paragraph::new(preview_lines(app))
            .scroll((app.preview_scroll as u16, 0))
            .block(
                Block::default()
                    .title(" Preview · PgUp/PgDn · long lines clipped ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            ),
        middle[1],
    );
    frame.render_widget(
        Paragraph::new(app.text.as_str())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Prompt · Tab/Enter completes · Enter submits · Ctrl-J always submits ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
        vertical[3],
    );
    frame.render_widget(
        Paragraph::new(format!(
            " {} · refs {}",
            app.status,
            format_context_total(app.refs_total)
        ))
        .style(Style::default().fg(Color::DarkGray)),
        vertical[4],
    );
}

/// Fully validated state prepared before the application enters raw mode.
pub struct Startup {
    pub repository: Repository,
    pub config: Config,
    pub document: Document,
}

pub fn run(startup: Startup) -> Result<()> {
    let Startup {
        repository,
        config,
        document,
    } = startup;
    let mut app =
        App::new(repository, &config).context("could not initialize reference providers")?;
    // Transitional bridge until the Phase 7 shell makes `Document` its source
    // of truth. Keeping the validated document in the startup API prevents the
    // file lifecycle from being rediscovered after raw mode begins.
    app.text = document.text().to_owned();
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;
    let result = (|| -> Result<Vec<String>> {
        loop {
            app.drain_reference_events();
            terminal.draw(|frame| draw(frame, &app))?;
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
            let next_event = event::poll(Duration::from_millis(50))?
                .then(event::read)
                .transpose()?;
            if let Some(Event::Key(key)) = next_event
                && key.kind == crossterm::event::KeyEventKind::Press
            {
                let should_exit = app.key(key);
                if should_exit {
                    break;
                }
            }
        }
        Ok(app.transcript)
    })();
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    let transcript = result?;
    for line in transcript {
        writeln!(io::stdout(), "{line}")?;
    }
    io::stdout().flush()?;
    Ok(())
}

pub fn is_terminal() -> bool {
    crossterm::tty::IsTty::is_tty(&io::stdin())
}

fn is_completion_accept_key(code: KeyCode) -> bool {
    matches!(code, KeyCode::Tab | KeyCode::Enter)
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
    use crate::references::model::{FileOrigin, FileTarget, ReferenceId, ReferenceTarget};
    use std::time::Instant;

    fn drain_until(app: &mut App, predicate: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !predicate(app) {
            assert!(Instant::now() < deadline, "background operation timed out");
            app.drain_reference_events();
            std::thread::yield_now();
        }
    }

    #[test]
    fn tab_and_enter_accept_completions() {
        assert!(is_completion_accept_key(KeyCode::Tab));
        assert!(is_completion_accept_key(KeyCode::Enter));
        assert!(!is_completion_accept_key(KeyCode::Char('x')));
    }

    #[test]
    fn preview_starts_five_lines_before_the_symbol() {
        assert_eq!(centered_preview_scroll(109), 103);
        assert_eq!(centered_preview_scroll(3), 0);
    }

    #[test]
    fn accepted_reference_replaces_unicode_character_range_and_invalidates_overlap() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("notes.txt"), "notes\n").unwrap();
        let repo = Repository::discover(temp.path()).unwrap();
        let mut app = App::new(repo, &Config::default()).unwrap();
        app.text = "é @no".into();
        app.references.push(ResolvedReference {
            id: ReferenceId(1),
            range: TextRange::new(2, 5).unwrap(),
            friendly_text: "@no".into(),
            target: ReferenceTarget::File(FileTarget {
                canonical_path: temp.path().join("notes.txt"),
                relative_path: "notes.txt".into(),
                origin: FileOrigin::GitAware,
                source_version: None,
            }),
        });
        let accepted = crate::references::model::AcceptedReference {
            replacement_range: TextRange::new(2, 5).unwrap(),
            replacement_text: "@notes.txt".into(),
            reference: ResolvedReference {
                id: ReferenceId(2),
                range: TextRange::new(2, 12).unwrap(),
                friendly_text: "@notes.txt".into(),
                target: ReferenceTarget::File(FileTarget {
                    canonical_path: temp.path().join("notes.txt"),
                    relative_path: "notes.txt".into(),
                    origin: FileOrigin::GitAware,
                    source_version: None,
                }),
            },
        };

        app.apply_accepted(accepted).unwrap();
        assert_eq!(app.text, "é @notes.txt");
        assert_eq!(app.references.len(), 1);
        assert_eq!(app.references[0].id, ReferenceId(2));
        assert_eq!(app.revision, DocumentRevision(1));
    }

    #[test]
    fn candidate_rendering_never_reads_provider_data() {
        let candidate = ReferenceCandidate {
            id: CandidateId {
                provider: ReferenceKind::GitFile,
                opaque: "gone".into(),
            },
            generation: crate::references::model::GenerationId(1),
            kind: ReferenceKind::GitFile,
            friendly_text: "@gone".into(),
            display: crate::references::model::CandidateDisplay {
                primary: "gone".into(),
                secondary: Some("file".into()),
                ..Default::default()
            },
            context_cost: ContextCost::Tokens(1_234),
            file_context_cost: None,
            source_version: None,
            token_source: None,
        };
        assert_eq!(candidate_text(&candidate), "gone  file · 1.2k");
    }

    #[test]
    fn configured_leader_accepts_and_lowers_through_async_app_pipeline() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("notes.txt"), "notes\n").unwrap();
        let repo = Repository::discover(temp.path()).unwrap();
        let mut config = Config::default();
        config.leaders.files = "§".into();
        let mut app = App::new(repo, &config).unwrap();

        for character in "§not".chars() {
            app.key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        drain_until(&mut app, App::completion_open);
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        drain_until(&mut app, |app| !app.references.is_empty());

        assert_eq!(app.text, "§notes.txt");
        assert_eq!(app.references[0].range, TextRange::new(0, 10).unwrap());

        app.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        drain_until(&mut app, |app| app.pending_clipboard.is_some());
        assert_eq!(app.pending_clipboard.as_deref(), Some("notes.txt"));
        assert_eq!(app.transcript, ["notes.txt"]);
    }

    #[test]
    fn osc52_encodes_the_lowered_prompt() {
        assert_eq!(
            crate::clipboard::osc52_sequence("src/lib.rs"),
            "\u{1b}]52;c;c3JjL2xpYi5ycw==\u{7}"
        );
    }
}
