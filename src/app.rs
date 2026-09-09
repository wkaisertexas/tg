mod help;
mod input;
mod references;
mod shell;
#[cfg(test)]
mod tests;
mod view;

use crate::config::{ColorMode, Config, LeadersConfig, LoadedConfig, PreviewMode, TokenConfig};
use crate::editor::command::CommandDispatcher;
use crate::editor::save::{AtomicSaver, SaveTarget};
use crate::editor::{Document, EditorSession};
use crate::onboarding::ProviderPanel;
use crate::references::ThreadExecutor;
use crate::references::model::{CandidateId, Preview, ReferenceKind};
use crate::references::session::{DocumentRevision, ReferenceSession};
use crate::repository::Repository;
use crate::tokens::ContextTotal;
use anyhow::{Context, Result};
use crossterm::event;
use help::config_help_lines;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::style::{Color, Modifier, Style};
use shell::{PendingShell, ShellResult};
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use view::{draw, status_is_sticky};

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
    providers: ProviderPanel,
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
        // External executables are intentionally not probed here. A missing or
        // unauthenticated CLI therefore affects only a query for its leader.
        let providers = crate::references::configured(&repo, config, ReferenceKind::ALL)?;
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
        let providers = ProviderPanel::new(repo.clone(), config.clone());
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
            providers,
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

    fn tick(&mut self, now: Instant) {
        self.providers.poll();
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
}

pub struct Startup {
    pub repository: Repository,
    pub config: LoadedConfig,
    pub document: Document,
    pub save_target: Option<SaveTarget>,
}

pub fn run(startup: Startup) -> Result<()> {
    let mut app = App::new(
        startup.repository,
        &startup.config.config,
        startup.document,
        startup.save_target,
    )
    .context("could not initialize editor")?;
    app.providers.set_loaded_config(&startup.config);
    if app.save_target.is_none() {
        app.status = "i to write · Space p for reference setup · Space ? for help".into();
    }
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
