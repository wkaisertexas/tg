mod render;
mod report;
#[cfg(test)]
mod tests;

use crate::config::{ColorMode, Config, LoadedConfig};
use crate::references::diagnostics::{self, HealthState, ProviderHealth};
use crate::references::model::ReferenceKind;
use crate::references::{BackgroundExecutor, CancellationFlag, ThreadExecutor};
use crate::repository::Repository;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use report::now_seconds;
pub use report::{configuration_lines, detail_lines, doctor};
use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

#[derive(Default, PartialEq, Eq)]
enum Page {
    #[default]
    Overview,
    Details,
    Configuration,
    QuickStart,
}

struct PendingCheck {
    id: u64,
    cancellation: CancellationFlag,
    previous: Vec<ProviderHealth>,
}

pub struct ProviderPanel {
    pub visible: bool,
    repository: Repository,
    config: Config,
    pub reports: Vec<ProviderHealth>,
    query_failures: HashSet<ReferenceKind>,
    configuration: Vec<String>,
    page: Page,
    selection: ListState,
    scroll: u16,
    max_scroll: u16,
    page_height: u16,
    confirmation: Option<Vec<ReferenceKind>>,
    pending: Option<PendingCheck>,
    next_check: u64,
    sender: Sender<(u64, ProviderHealth)>,
    receiver: Receiver<(u64, ProviderHealth)>,
}

impl ProviderPanel {
    pub fn new(repository: Repository, config: Config) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            visible: false,
            repository,
            config,
            reports: Vec::new(),
            query_failures: HashSet::new(),
            configuration: Vec::new(),
            page: Page::Overview,
            selection: ListState::default().with_selected(Some(0)),
            scroll: 0,
            max_scroll: 0,
            page_height: 1,
            confirmation: None,
            pending: None,
            next_check: 0,
            sender,
            receiver,
        }
    }

    pub fn set_loaded_config(&mut self, loaded: &LoadedConfig) {
        self.configuration = configuration_lines(loaded);
    }

    fn ensure_inventory(&mut self) {
        if self.reports.is_empty() {
            self.reports = diagnostics::inventory(&self.repository, &self.config);
        }
    }

    pub fn open(&mut self) {
        self.ensure_inventory();
        self.visible = true;
        self.page = Page::Overview;
        self.scroll = 0;
    }

    pub fn record_failure(&mut self, kind: ReferenceKind, message: &str) {
        self.ensure_inventory();
        if let Some((index, report)) = self
            .reports
            .iter_mut()
            .enumerate()
            .find(|(_, report)| report.kind == kind)
        {
            if report.state != HealthState::Disabled {
                diagnostics::record_failure(report, message);
                self.query_failures.insert(kind);
            }
            if !self.visible {
                self.selection.select(Some(index));
            }
        }
    }

    pub fn record_query_success(&mut self, kind: ReferenceKind) {
        if self.query_failures.remove(&kind)
            && let Some(report) = self.reports.iter_mut().find(|report| report.kind == kind)
            && report.state.is_problem()
        {
            report.state = HealthState::NotChecked;
            report.summary =
                "Latest query succeeded. Test connection to verify authentication and access."
                    .into();
            report.actions = vec!["Press r to test the connection, or continue editing.".into()];
            report.checked_at = Some(now_seconds());
        }
    }

    pub fn problem_count(&self) -> usize {
        self.reports
            .iter()
            .filter(|report| report.state.is_problem())
            .count()
    }

    pub fn poll(&mut self) {
        while let Ok((id, report)) = self.receiver.try_recv() {
            let Some(pending) = self.pending.as_mut().filter(|pending| pending.id == id) else {
                continue;
            };
            pending
                .previous
                .retain(|previous| previous.kind != report.kind);
            if let Some(current) = self
                .reports
                .iter_mut()
                .find(|current| current.kind == report.kind)
                && current.state == HealthState::Checking
            {
                self.query_failures.remove(&report.kind);
                *current = report;
            }
            if pending.previous.is_empty() {
                self.pending = None;
            }
        }
    }

    fn cancel_check(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.cancellation.cancel();
            for previous in pending.previous {
                if let Some(current) = self
                    .reports
                    .iter_mut()
                    .find(|current| current.kind == previous.kind)
                    && current.state == HealthState::Checking
                {
                    *current = previous;
                }
            }
        }
    }

    fn start_check(&mut self, kinds: Vec<ReferenceKind>) {
        self.cancel_check();
        let previous: Vec<_> = self
            .reports
            .iter()
            .filter(|report| kinds.contains(&report.kind))
            .cloned()
            .collect();
        if previous.is_empty() {
            return;
        }
        for report in &mut self.reports {
            if kinds.contains(&report.kind) {
                report.state = HealthState::Checking;
            }
        }
        self.next_check = self.next_check.wrapping_add(1);
        let id = self.next_check;
        let cancellation = CancellationFlag::default();
        let worker_cancellation = cancellation.clone();
        let repository = self.repository.clone();
        let config = self.config.clone();
        let sender = self.sender.clone();
        ThreadExecutor.spawn(Box::new(move || {
            for kind in kinds {
                if worker_cancellation.is_cancelled() {
                    break;
                }
                let report = diagnostics::check(&repository, &config, kind, &worker_cancellation);
                if sender.send((id, report)).is_err() {
                    break;
                }
            }
        }));
        self.pending = Some(PendingCheck {
            id,
            cancellation,
            previous,
        });
    }

    fn request_check(&mut self, all: bool) {
        let selected = self.selection.selected().unwrap_or(0);
        let kinds: Vec<_> = self
            .reports
            .iter()
            .enumerate()
            .filter(|(index, report)| {
                (all || *index == selected)
                    && report.state != HealthState::Disabled
                    && matches!(
                        report.kind,
                        ReferenceKind::GitHubIssue
                            | ReferenceKind::GitHubPullRequest
                            | ReferenceKind::JiraIssue
                    )
            })
            .map(|(_, report)| report.kind)
            .collect();
        if kinds.is_empty() {
            let fresh = diagnostics::inventory(&self.repository, &self.config);
            if let (Some(current), Some(updated)) = (
                self.reports.get_mut(selected),
                fresh.into_iter().nth(selected),
            ) {
                *current = updated;
            }
        } else {
            self.confirmation = Some(kinds);
            self.scroll = 0;
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.confirmation.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let kinds = self.confirmation.take().unwrap_or_default();
                    self.start_check(kinds);
                }
                KeyCode::Esc | KeyCode::Char('n' | 'q') => self.confirmation = None,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.scroll = self.scroll.saturating_add(1).min(self.max_scroll)
                }
                KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                _ => {}
            }
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
            self.cancel_check();
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.page == Page::Overview {
                    self.cancel_check();
                    self.visible = false;
                } else {
                    self.page = Page::Overview;
                    self.scroll = 0;
                }
            }
            KeyCode::Enter if self.page == Page::Overview => {
                self.page = Page::Details;
                self.scroll = 0;
            }
            KeyCode::Char('c') => {
                self.page = Page::Configuration;
                self.scroll = 0;
            }
            KeyCode::Char('?') => {
                self.page = Page::QuickStart;
                self.scroll = 0;
            }
            KeyCode::Char('r') => self.request_check(false),
            KeyCode::Char('R') => self.request_check(true),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::PageDown => self.move_by(i32::from(self.page_height)),
            KeyCode::PageUp => self.move_by(-i32::from(self.page_height)),
            KeyCode::Home | KeyCode::Char('g') => self.move_by(-i32::MAX),
            KeyCode::End | KeyCode::Char('G') => self.move_by(i32::MAX),
            _ => {}
        }
    }

    fn move_by(&mut self, delta: i32) {
        if self.page == Page::Overview {
            let selected = self.selection.selected().unwrap_or(0) as i64;
            self.selection.select(Some(
                (selected + i64::from(delta)).clamp(0, self.reports.len().saturating_sub(1) as i64)
                    as usize,
            ));
        } else {
            self.scroll = (i64::from(self.scroll) + i64::from(delta))
                .clamp(0, i64::from(self.max_scroll)) as u16;
        }
    }
}

impl Drop for ProviderPanel {
    fn drop(&mut self) {
        self.cancel_check();
    }
}

pub fn run(repository: Repository, loaded: LoadedConfig) -> Result<()> {
    let color = loaded.config.ui.color != ColorMode::Never;
    let mut panel = ProviderPanel::new(repository, loaded.config.clone());
    panel.set_loaded_config(&loaded);
    panel.open();
    let mut guard = crate::terminal::TerminalGuard::stderr()?;
    {
        let mut terminal = Terminal::new(CrosstermBackend::new(guard.backend_mut().writer_mut()))?;
        while panel.visible {
            panel.poll();
            terminal.draw(|frame| panel.draw(frame, frame.area(), color))?;
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(key) = event::read()?
                && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            {
                panel.handle_key(key);
            }
        }
    }
    guard.restore()?;
    Ok(())
}
