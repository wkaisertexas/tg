use crate::config::{ColorMode, Config, LeadersConfig, PreviewMode, TokenConfig};
use crate::editor::command::{CommandDispatcher, CommandEffect, LowerRequest};
use crate::editor::save::{AtomicSaver, SaveTarget};
use crate::editor::{AdapterMode, Document, EditorInput, EditorSession, TextEdit};
use crate::references::activation::detect_activation;
use crate::references::file::FileProvider;
use crate::references::github::GithubProvider;
use crate::references::jira::JiraProvider;
use crate::references::model::{
    CandidateId, ContextCost, Preview, ReferenceCandidate, ReferenceKind, ReferenceTarget,
    TextRange,
};
use crate::references::process::{ProcessRequest, run as run_process};
use crate::references::session::{
    DocumentRevision, LowerPurpose, OperationKind, ReferenceEvent, ReferenceSession,
};
use crate::references::skill::SkillProvider;
use crate::references::symbol::SymbolProvider;
use crate::references::{BackgroundExecutor, CancellationFlag, ReferenceProvider, ThreadExecutor};
use crate::repository::Repository;
use crate::tokens::ContextTotal;
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

struct CachedPreview {
    candidate_id: CandidateId,
    preview: Option<Preview>,
}

const SHELL_TIMEOUT: Duration = Duration::from_secs(30);
const SHELL_OUTPUT_LIMIT: usize = 1024 * 1024;

struct PendingShell {
    id: u64,
    cancellation: CancellationFlag,
}

struct ShellResult {
    id: u64,
    revision: DocumentRevision,
    offset: usize,
    prefix_newline: bool,
    output: Result<Vec<u8>, String>,
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
    search_debounce: Duration,
    preview_mode: PreviewMode,
    preview_visible: bool,
    color: bool,
    completion_height: u16,
    completion_width_percent: u8,
    status: String,
    observed_status: String,
    status_changed_at: Instant,
    status_timeout: Duration,
    preview_due: Option<Instant>,
    token_config: TokenConfig,
    preview: Option<CachedPreview>,
    preview_scroll: usize,
    pending_clipboard: Option<String>,
    should_exit: bool,
    refs_total: ContextTotal,
    edit_group_active: bool,
    help_lines: Vec<String>,
    help_pending: bool,
    help_visible: bool,
    shell_sender: Sender<ShellResult>,
    shell_receiver: Receiver<ShellResult>,
    pending_shell: Option<PendingShell>,
    next_shell_id: u64,
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
        let broad = FileProvider::with_broad_excludes(
            &repo.search_root,
            ReferenceKind::BroadFile,
            config.leaders.broad_files.clone(),
            &config.search.broad_excludes,
        )?;
        let symbols = SymbolProvider::with_broad_excludes(
            &repo.search_root,
            config.leaders.files.clone(),
            &config.search.broad_excludes,
        )?;
        let skills = SkillProvider::new(
            &repo.search_root,
            &repo.invocation_root,
            &config.skills,
            config.leaders.skills.clone(),
        )?;
        let mut providers = vec![
            Arc::new(git) as Arc<dyn ReferenceProvider>,
            Arc::new(broad),
            Arc::new(symbols),
            Arc::new(skills),
        ];
        // External executables are intentionally not probed here. A missing or
        // unauthenticated CLI therefore affects only a query for its leader.
        if config.providers.github.enabled {
            providers.push(Arc::new(GithubProvider::issues(
                &repo.invocation_root,
                config.leaders.github_issues.clone(),
                &config.providers.github,
            )));
            providers.push(Arc::new(GithubProvider::pull_requests(
                &repo.invocation_root,
                config.leaders.github_pull_requests.clone(),
                &config.providers.github,
            )));
        }
        if config.providers.jira.enabled {
            providers.push(Arc::new(JiraProvider::new(
                &config.providers.jira,
                config.leaders.jira_issues.clone(),
            )?));
        }
        let mut reference_session = ReferenceSession::with_tokenizer(
            providers,
            Arc::new(ThreadExecutor),
            &config.tokens.tokenizer,
        )?;
        reference_session.update_references(DocumentRevision(0), Arc::from([]))?;
        let mut editor = EditorSession::new(document.text());
        editor.configure(&config.editor, &config.ui.preview_toggle)?;
        let color = config.ui.color != ColorMode::Never;
        editor.set_reference_style(if color {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().add_modifier(Modifier::UNDERLINED)
        })?;
        let (shell_sender, shell_receiver) = mpsc::channel();
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
            search_debounce: Duration::from_millis(config.search.debounce_ms),
            preview_mode: config.ui.preview,
            preview_visible: config.ui.preview == PreviewMode::Automatic,
            color,
            completion_height: config.ui.completion_height,
            completion_width_percent: config.ui.completion_width_percent,
            status: String::new(),
            observed_status: String::new(),
            status_changed_at: Instant::now(),
            status_timeout: Duration::from_millis(config.ui.status_timeout_ms),
            preview_due: None,
            token_config: config.tokens.clone(),
            preview: None,
            preview_scroll: 0,
            pending_clipboard: None,
            should_exit: false,
            refs_total: ContextTotal::default(),
            edit_group_active: false,
            help_lines: config_help_lines(config),
            help_pending: false,
            help_visible: false,
            shell_sender,
            shell_receiver,
            pending_shell: None,
            next_shell_id: 0,
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

    fn schedule_preview(&mut self) {
        if self.preview_mode == PreviewMode::Automatic {
            self.preview = None;
            self.preview_due = Some(Instant::now() + self.search_debounce);
        } else {
            self.request_preview();
        }
    }

    fn tick(&mut self, now: Instant) {
        self.drain_shell_results();
        if self.status != self.observed_status {
            self.observed_status.clone_from(&self.status);
            self.status_changed_at = now;
        } else if !self.status.is_empty()
            && !status_is_sticky(&self.status)
            && now.duration_since(self.status_changed_at) >= self.status_timeout
        {
            self.status.clear();
            self.observed_status.clear();
        }
        if self.preview_due.is_some_and(|due| now >= due) {
            self.preview_due = None;
            self.request_preview();
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

    fn begin_shell_read(&mut self, command: String) {
        if let Some(pending) = self.pending_shell.take() {
            pending.cancellation.cancel();
        }
        self.next_shell_id = self.next_shell_id.wrapping_add(1);
        let id = self.next_shell_id;
        let revision = self.revision();
        let (offset, prefix_newline) =
            shell_insertion_point(self.document.text(), self.editor.cursor_char_offset());
        let cancellation = CancellationFlag::default();
        let worker_cancellation = cancellation.clone();
        let sender = self.shell_sender.clone();
        let cwd = self.repo.invocation_root.clone();
        ThreadExecutor.spawn(Box::new(move || {
            let output = run_process(
                ProcessRequest {
                    executable: "bash".into(),
                    args: vec!["-c".into(), command.into()],
                    cwd,
                    timeout: SHELL_TIMEOUT,
                    output_limit: SHELL_OUTPUT_LIMIT,
                    env: Vec::new(),
                },
                &worker_cancellation,
            )
            .map(|output| output.stdout)
            .map_err(|error| error.to_string());
            let _ = sender.send(ShellResult {
                id,
                revision,
                offset,
                prefix_newline,
                output,
            });
        }));
        self.pending_shell = Some(PendingShell { id, cancellation });
        self.status = "Running shell command…".into();
    }

    fn drain_shell_results(&mut self) {
        loop {
            let result = match self.shell_receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            };
            if self
                .pending_shell
                .as_ref()
                .is_none_or(|pending| pending.id != result.id)
            {
                continue;
            }
            self.pending_shell = None;
            if result.revision != self.revision() {
                self.status = "Command output discarded because the document changed".into();
                continue;
            }
            let bytes = match result.output {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.status = format!("Command failed: {error}");
                    continue;
                }
            };
            if bytes.is_empty() {
                self.status = "Command produced no output".into();
                continue;
            }
            let output = match String::from_utf8(bytes) {
                Ok(output) => output,
                Err(_) => {
                    self.status = "Command failed: output is not UTF-8".into();
                    continue;
                }
            };
            let mut insertion = String::new();
            if result.prefix_newline {
                insertion.push('\n');
            }
            insertion.push_str(&output);
            if !insertion.ends_with('\n') {
                insertion.push('\n');
            }
            let inserted_bytes = insertion.len();
            let cursor = result.offset + usize::from(result.prefix_newline);
            let edit = TextEdit::new(
                TextRange {
                    start: result.offset,
                    end: result.offset,
                },
                insertion,
            );
            let update = self
                .document
                .apply(&[edit])
                .and_then(|_| self.replace_widget_from_document())
                .and_then(|_| self.editor.set_cursor_char_offset(cursor))
                .and_then(|_| self.sync_reference_state());
            match update {
                Ok(()) => self.status = format!("Read {inserted_bytes} bytes from shell"),
                Err(error) => {
                    self.status = format!("Command output could not be inserted: {error}")
                }
            }
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
            Ok(CommandEffect::ReadShell(command)) => self.begin_shell_read(command),
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
                            self.schedule_preview();
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

    fn handle_help_key(&mut self, key: event::KeyEvent) -> bool {
        if self.help_visible {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q' | '?')) {
                self.help_visible = false;
            }
            return true;
        }
        if self.editor.mode() != AdapterMode::Normal {
            self.help_pending = false;
            return false;
        }
        if self.help_pending {
            self.help_pending = false;
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

    fn handle_event(&mut self, event: Event) {
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

impl Drop for App {
    fn drop(&mut self) {
        if let Some(pending) = self.pending_shell.take() {
            pending.cancellation.cancel();
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

fn shell_insertion_point(text: &str, cursor: usize) -> (usize, bool) {
    let characters: Vec<_> = text.chars().collect();
    let cursor = cursor.min(characters.len());
    if let Some(line_end) = characters[cursor..]
        .iter()
        .position(|character| *character == '\n')
    {
        (cursor + line_end + 1, false)
    } else {
        (characters.len(), !characters.is_empty())
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

fn candidate_text(candidate: &ReferenceCandidate, tokens: &TokenConfig) -> String {
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

fn completion_title(kind: Option<ReferenceKind>) -> &'static str {
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

fn token_inlay_hints(app: &App) -> Vec<(usize, String)> {
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
        ContextCost::Tokens(tokens) if *tokens >= 1_000 => Some(format!(
            "{:.*}k",
            usize::from(decimals),
            *tokens as f64 / 1_000.0
        )),
        ContextCost::Tokens(tokens) => Some(tokens.to_string()),
        ContextCost::Bytes(bytes) => Some(format!("{bytes} bytes")),
        ContextCost::None => None,
        ContextCost::Unavailable => Some("unavailable".into()),
    }
}

fn status_is_sticky(status: &str) -> bool {
    status.starts_with("Cannot ")
        || status.starts_with("Write failed:")
        || status.starts_with("Save failed:")
        || status.starts_with("Command failed:")
        || status.starts_with("Command output could not be inserted:")
}

fn status_text(app: &App, width: u16) -> String {
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
    let full = [
        mode.as_str(),
        &format!("{path}{dirty}"),
        app.status.as_str(),
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

fn config_help_lines(config: &Config) -> Vec<String> {
    vec![
        "Keys: Space ? help · Esc/q/? close".into(),
        "Completion: Tab/Enter accept · Up/Down or Ctrl-J/Ctrl-K select".into(),
        format!("Preview: {}", config.ui.preview_toggle),
        format!(
            "Commands: :w · :q · :q! · :wq · {} · :r !command",
            config.editor.copy_command
        ),
        format!(
            "[leaders] files={:?} broad_files={:?} symbols={:?}",
            config.leaders.files, config.leaders.broad_files, config.leaders.symbols
        ),
        format!(
            "[leaders] skills={:?} github_issues={:?} github_pull_requests={:?} jira_issues={:?}",
            config.leaders.skills,
            config.leaders.github_issues,
            config.leaders.github_pull_requests,
            config.leaders.jira_issues
        ),
        format!(
            "[editor] line_numbers={} current_line_absolute={} tab_width={} wrap={}",
            format!("{:?}", config.editor.line_numbers).to_ascii_lowercase(),
            config.editor.current_line_absolute,
            config.editor.tab_width,
            config.editor.wrap
        ),
        format!(
            "[ui] preview={} color={} completion={}x{}%",
            format!("{:?}", config.ui.preview).to_ascii_lowercase(),
            format!("{:?}", config.ui.color).to_ascii_lowercase(),
            config.ui.completion_height,
            config.ui.completion_width_percent
        ),
        format!(
            "[search] limit={} debounce_ms={} broad_excludes={:?}",
            config.search.limit, config.search.debounce_ms, config.search.broad_excludes
        ),
        format!(
            "[tokens] tokenizer={:?} decimals={} file={} symbol={} total={}",
            config.tokens.tokenizer,
            config.tokens.decimals,
            config.tokens.show_file,
            config.tokens.show_symbol,
            config.tokens.show_total
        ),
        format!(
            "[skills] profile={:?} mention={:?} roots={}",
            config.skills.profile,
            config.skills.mention,
            config.skills.roots.len()
        ),
        format!(
            "[providers.github] enabled={} command={} limit={} timeout_ms={}",
            config.providers.github.enabled,
            config.providers.github.command.display(),
            config.providers.github.limit,
            config.providers.github.timeout_ms
        ),
        format!(
            "[providers.jira] enabled={} command={} key_prefix={:?} limit={} timeout_ms={}",
            config.providers.jira.enabled,
            config.providers.jira.command.display(),
            config.providers.jira.key_prefix,
            config.providers.jira.limit,
            config.providers.jira.timeout_ms
        ),
    ]
}

fn styled_help_lines(lines: &[String], color: bool) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| {
            if line.starts_with('[')
                && let Some(end) = line.find(']')
            {
                let section = line[..=end].to_owned();
                let values = line[end + 1..].to_owned();
                return Line::from(vec![
                    Span::styled(
                        section,
                        if color {
                            Style::default()
                                .fg(Color::Magenta)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().add_modifier(Modifier::BOLD)
                        },
                    ),
                    Span::styled(
                        values,
                        if color {
                            Style::default().fg(Color::Green)
                        } else {
                            Style::default()
                        },
                    ),
                ]);
            }
            if let Some((label, value)) = line.split_once(':') {
                return Line::from(vec![
                    Span::styled(
                        format!("{label}:"),
                        if color {
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().add_modifier(Modifier::BOLD)
                        },
                    ),
                    Span::styled(
                        value.to_owned(),
                        if color {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default().add_modifier(Modifier::UNDERLINED)
                        },
                    ),
                ]);
            }
            Line::from(line.clone())
        })
        .collect()
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
            app.tick(Instant::now());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::model::{QueryScope, TextRange};
    use std::fs;
    use std::path::Path;
    use std::thread;
    use std::time::Instant;

    fn app_for(root: &Path, config: &Config, text: &str) -> App {
        App::new(
            Repository::discover(root).unwrap(),
            config,
            Document::from_text(text),
            None,
        )
        .unwrap()
    }

    fn activate_text(app: &mut App, text: &str) {
        let activation = detect_activation(
            text,
            text.chars().count(),
            &app.leaders,
            &app.repo.search_root,
        )
        .unwrap()
        .expect("test text must activate a leader");
        app.reference_session
            .activate(activation, app.search_limit)
            .unwrap();
    }

    fn wait_for(app: &mut App, ready: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            app.drain_reference_events();
            app.drain_shell_results();
            if ready(app) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for app state; status={}", app.status);
    }

    fn accept_and_lower(app: &mut App) -> String {
        wait_for(app, |app| !app.reference_session.candidates().is_empty());
        app.accept_selected();
        wait_for(app, |app| !app.document.references().is_empty());
        app.dispatch_command(":copy".into());
        wait_for(app, |app| app.pending_clipboard.is_some());
        app.pending_clipboard.take().unwrap()
    }

    #[cfg(unix)]
    fn executable(path: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, script).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn diff_is_unicode_character_based_and_minimal() {
        assert_eq!(
            single_edit("a🦀b", "a日本b"),
            TextEdit::new(TextRange { start: 1, end: 2 }, "日本")
        );
    }

    #[test]
    fn shell_insertion_points_follow_the_current_line() {
        assert_eq!(shell_insertion_point("", 0), (0, false));
        assert_eq!(shell_insertion_point("one", 1), (3, true));
        assert_eq!(shell_insertion_point("one\ntwo", 1), (4, false));
        assert_eq!(shell_insertion_point("one\ntwo", 5), (7, true));
    }

    #[cfg(unix)]
    #[test]
    fn read_shell_command_inserts_bounded_output_as_one_undoable_edit() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = app_for(temp.path(), &Config::default(), "first\nlast");
        app.editor.set_cursor_char_offset(1).unwrap();

        app.dispatch_command(":r !printf 'alpha\\nbeta'".into());
        assert!(app.pending_shell.is_some());
        assert_eq!(app.document.text(), "first\nlast");
        wait_for(&mut app, |app| app.pending_shell.is_none());
        assert_eq!(app.document.text(), "first\nalpha\nbeta\nlast");
        assert_eq!(app.editor.text(), app.document.text());
        assert!(app.status.starts_with("Read "));

        app.handle_event(Event::Key(event::KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::NONE,
        )));
        assert_eq!(app.document.text(), "first\nlast");

        app.dispatch_command(
            ":r !printf 'Authorization: Bearer super-secret-token' >&2; exit 7".into(),
        );
        wait_for(&mut app, |app| app.pending_shell.is_none());
        assert!(app.status.starts_with("Command failed:"));
        assert!(!app.status.contains("super-secret-token"));
        assert_eq!(app.document.text(), "first\nlast");
    }

    #[test]
    fn accepted_file_renders_token_count_as_virtual_text() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("README.md"), "word ".repeat(6_000)).unwrap();
        let mut app = app_for(temp.path(), &Config::default(), "@README.md");
        activate_text(&mut app, "@README.md");
        wait_for(&mut app, |app| {
            !app.reference_session.candidates().is_empty()
        });
        app.accept_selected();
        wait_for(&mut app, |app| !app.document.references().is_empty());
        wait_for(&mut app, |app| {
            matches!(
                app.reference_session
                    .reference_cost(&app.document.references()[0]),
                Some(ContextCost::Tokens(tokens)) if tokens >= 1_000
            )
        });

        let hints = token_inlay_hints(&app);
        assert_eq!(hints.len(), 1);
        assert!(hints[0].1.ends_with('k'));
        assert_eq!(app.document.text(), "@README.md");
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 10)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut rendered = String::new();
        for y in 0..10 {
            for x in 0..80 {
                rendered.push_str(buffer[(x, y)].symbol());
            }
        }
        assert!(rendered.contains("k tokens"));
        assert!(!app.document.text().contains("tokens"));
    }

    #[test]
    fn accepted_file_completion_stays_inserted_for_symbol_chaining() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/app.rs"), "pub fn submit() {}\n").unwrap();
        let mut app = app_for(temp.path(), &Config::default(), "@src/app.rs");
        app.editor.set_cursor_char_offset(11).unwrap();
        app.handle_event(Event::Key(event::KeyEvent::new(
            KeyCode::Char('i'),
            KeyModifiers::NONE,
        )));
        activate_text(&mut app, "@src/app.rs");
        wait_for(&mut app, |app| {
            !app.reference_session.candidates().is_empty()
        });

        app.handle_event(Event::Key(event::KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        )));
        wait_for(&mut app, |app| !app.document.references().is_empty());
        assert_eq!(app.editor.mode(), AdapterMode::Insert);

        for _ in 0..2 {
            app.handle_event(Event::Key(event::KeyEvent::new(
                KeyCode::Char(':'),
                KeyModifiers::SHIFT,
            )));
        }
        assert_eq!(app.document.text(), "@src/app.rs::");
        assert_eq!(app.editor.command_line(), None);
        assert_eq!(app.editor.mode(), AdapterMode::Insert);
        assert_eq!(
            app.reference_session.active_kind(),
            Some(ReferenceKind::Symbol)
        );
    }

    #[test]
    fn space_question_opens_modal_configuration_help() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.leaders.files = "~f".into();
        config.ui.preview_toggle = "ctrl-x".into();
        config.providers.jira.key_prefix = Some("OPS".into());
        let mut app = app_for(temp.path(), &config, "unchanged");
        let press = |app: &mut App, code, modifiers| {
            app.handle_event(Event::Key(event::KeyEvent::new(code, modifiers)));
        };

        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.help_pending);
        assert!(!app.help_visible);
        press(&mut app, KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert!(app.help_visible);
        assert!(!app.help_pending);
        assert!(
            app.help_lines
                .iter()
                .any(|line| line.contains("files=\"~f\""))
        );
        assert!(app.help_lines.iter().any(|line| line == "Preview: ctrl-x"));
        assert!(
            app.help_lines
                .iter()
                .any(|line| line.contains("key_prefix=Some(\"OPS\")"))
        );
        let colored = styled_help_lines(&app.help_lines, true);
        assert_eq!(colored[0].spans[0].style.fg, Some(Color::Cyan));
        assert_eq!(colored[4].spans[0].style.fg, Some(Color::Magenta));
        assert_eq!(colored[4].spans[1].style.fg, Some(Color::Green));
        let plain = styled_help_lines(&app.help_lines, false);
        assert!(
            plain
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style.fg.is_none())
        );
        assert!(
            plain[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut rendered = String::new();
        for y in 0..24 {
            for x in 0..100 {
                rendered.push_str(buffer[(x, y)].symbol());
            }
        }
        assert!(rendered.contains("Configuration · Esc/q/? to close"));
        assert!(rendered.contains("[leaders] files=\"~f\""));

        press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        assert!(app.help_visible);
        assert_eq!(app.editor.mode(), AdapterMode::Normal);
        assert_eq!(app.document.text(), "unchanged");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.help_visible);
        press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(app.editor.mode(), AdapterMode::Insert);
    }

    #[test]
    fn replace_mode_edits_form_one_history_group() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = app_for(temp.path(), &Config::default(), "abc");
        for event in [
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('R'),
                KeyModifiers::SHIFT,
            )),
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('X'),
                KeyModifiers::NONE,
            )),
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('Y'),
                KeyModifiers::NONE,
            )),
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )),
        ] {
            app.handle_event(event);
        }
        assert_eq!(app.document.text(), "XYc");
        assert!(!app.edit_group_active);
        assert!(!app.completion_active());

        app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::NONE,
        )));
        assert_eq!(app.document.text(), "abc");
        assert_eq!(app.editor.text(), "abc");
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
    fn token_precision_visibility_and_dual_symbol_costs_follow_config() {
        use crate::references::model::{CandidateDisplay, GenerationId};

        let candidate = ReferenceCandidate {
            id: CandidateId {
                provider: ReferenceKind::Symbol,
                opaque: "symbol".into(),
            },
            generation: GenerationId(1),
            kind: ReferenceKind::Symbol,
            friendly_text: "@file.rs::item".into(),
            display: CandidateDisplay {
                primary: "item".into(),
                ..CandidateDisplay::default()
            },
            context_cost: ContextCost::Tokens(1_234),
            file_context_cost: Some(ContextCost::Tokens(18_765)),
            source_version: None,
            token_source: None,
        };
        let mut config = Config::default().tokens;
        config.decimals = 2;
        assert_eq!(
            candidate_text(&candidate, &config),
            "item · symbol 1.23k · file 18.77k"
        );
        config.show_symbol = false;
        assert_eq!(candidate_text(&candidate, &config), "item · file 18.77k");
        config.show_file = false;
        assert_eq!(candidate_text(&candidate, &config), "item");
    }

    #[test]
    fn status_expires_transient_messages_and_elides_details_when_narrow() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.ui.status_timeout_ms = 10;
        config.tokens.show_total = false;
        let mut app = app_for(temp.path(), &config, "hello");
        app.status = "Written".into();
        let started = Instant::now();
        app.tick(started);
        assert!(status_text(&app, 120).contains("1:1"));
        app.tick(started + Duration::from_millis(11));
        assert!(app.status.is_empty());
        app.status = "a deliberately long transient status".into();
        let narrow = status_text(&app, 12);
        assert!(!narrow.contains("deliberately"));
        assert!(narrow.contains("1:1"));
    }

    #[test]
    fn automatic_preview_waits_for_the_configured_debounce() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.ui.preview = PreviewMode::Automatic;
        config.search.debounce_ms = 80;
        let mut app = app_for(temp.path(), &config, "");
        app.schedule_preview();
        let due = app.preview_due.expect("preview deadline");
        app.tick(due - Duration::from_millis(1));
        assert!(app.preview_due.is_some());
        app.tick(due);
        assert!(app.preview_due.is_none());
    }

    #[test]
    fn completion_headers_name_the_active_provider() {
        assert_eq!(
            completion_title(Some(ReferenceKind::GitHubPullRequest)),
            " GitHub Pull Requests "
        );
        assert_eq!(completion_title(Some(ReferenceKind::Skill)), " Skills ");
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

    #[test]
    fn disabled_external_providers_are_absent_but_skills_remain_available() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join(".agents/skills/local");
        fs::create_dir_all(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: local\ndescription: Local fixture\n---\nbody\n",
        )
        .unwrap();
        let mut config = Config::default();
        config.providers.github.enabled = false;
        config.providers.jira.enabled = false;
        config.leaders.skills = "~s".into();
        config.leaders.github_issues = "~i".into();
        config.leaders.github_pull_requests = "~p".into();
        config.leaders.jira_issues = "~j".into();
        let mut app = app_for(temp.path(), &config, "~slocal");

        let github = detect_activation("~i1", 3, &app.leaders, &app.repo.search_root)
            .unwrap()
            .unwrap();
        assert!(
            app.reference_session
                .activate(github, app.search_limit)
                .unwrap_err()
                .to_string()
                .contains("no provider registered")
        );

        activate_text(&mut app, "~slocal");
        assert_eq!(accept_and_lower(&mut app), "~slocal");
    }

    #[cfg(unix)]
    #[test]
    fn custom_external_leaders_query_accept_and_lower_urls_end_to_end() {
        let temp = tempfile::tempdir().unwrap();
        let gh = temp.path().join("fake-gh");
        executable(
            &gh,
            r##"#!/bin/sh
case "$1" in
  issue) printf '%s' '[{"number":17,"title":"Fixture issue","url":"https://github.example/org/repo/issues/17","state":"OPEN","labels":[],"updatedAt":"2026-08-17T00:00:00Z"}]' ;;
  pr) printf '%s' '[{"number":23,"title":"Fixture PR","url":"https://github.example/org/repo/pull/23","state":"OPEN","isDraft":false,"updatedAt":"2026-08-17T00:00:00Z"}]' ;;
esac
"##,
        );
        let jira = temp.path().join("fake-jira");
        executable(
            &jira,
            r##"#!/bin/sh
printf '%s' '{"key":"OPS-42","self":"https://jira.example/rest/api/3/issue/OPS-42","fields":{"summary":"Fixture Jira","status":{"name":"Open"}}}'
"##,
        );
        let mut config = Config::default();
        config.leaders.github_issues = "~i".into();
        config.leaders.github_pull_requests = "~p".into();
        config.leaders.jira_issues = "~j".into();
        config.providers.github.command = gh;
        config.providers.jira.command = jira;
        config.providers.jira.key_prefix = Some("OPS".into());

        for (text, expected) in [
            ("~i17", "https://github.example/org/repo/issues/17"),
            ("~p23", "https://github.example/org/repo/pull/23"),
            ("~j42", "https://jira.example/browse/OPS-42"),
        ] {
            let mut app = app_for(temp.path(), &config, text);
            activate_text(&mut app, text);
            assert_eq!(accept_and_lower(&mut app), expected);
        }
    }

    #[test]
    fn missing_external_cli_error_does_not_poison_local_provider_queries() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("local.txt"), "fixture").unwrap();
        let skill = temp.path().join(".agents/skills/resilient");
        fs::create_dir_all(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: resilient\ndescription: Still available\n---\n",
        )
        .unwrap();
        let mut config = Config::default();
        config.providers.github.command = temp.path().join("missing-gh");
        config.leaders.github_issues = "~i".into();
        config.leaders.skills = "~s".into();
        let mut app = app_for(temp.path(), &config, "~i1");

        activate_text(&mut app, "~i1");
        wait_for(&mut app, |app| app.status.contains("was not found"));

        app.reference_session
            .start_query(
                ReferenceKind::GitFile,
                "local".into(),
                QueryScope::Repository,
                TextRange::new(0, 0).unwrap(),
                10,
            )
            .unwrap();
        wait_for(&mut app, |app| {
            app.reference_session
                .candidates()
                .iter()
                .any(|candidate| candidate.friendly_text.ends_with("local.txt"))
        });

        activate_text(&mut app, "~sresilient");
        wait_for(&mut app, |app| {
            app.reference_session
                .candidates()
                .iter()
                .any(|candidate| candidate.friendly_text == "~sresilient")
        });

        // Symbol registration remains present as well; an unknown file may
        // yield no symbols, but it must not fail as an unregistered provider.
        app.reference_session
            .start_query(
                ReferenceKind::Symbol,
                "missing.rs::item".into(),
                QueryScope::Repository,
                TextRange::new(0, 0).unwrap(),
                10,
            )
            .unwrap();
    }
}
