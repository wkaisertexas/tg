use crate::config::{ColorMode, Config, LoadedConfig};
use crate::references::diagnostics::{self, HealthState, ProviderHealth};
use crate::references::model::ReferenceKind;
use crate::references::{BackgroundExecutor, CancellationFlag, ThreadExecutor};
use crate::repository::Repository;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

impl Drop for ProviderPanel {
    fn drop(&mut self) {
        self.cancel_check();
    }
}

fn wrapped_lines(lines: Vec<String>, width: u16) -> Vec<Line<'static>> {
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

fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() || *character == '\n')
        .collect()
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn configuration_lines(loaded: &LoadedConfig) -> Vec<String> {
    let mut lines = vec![
        "tg settings: defaults < user file < project file < environment < CLI flags".into(),
        String::new(),
    ];
    if let Some(path) = &loaded.user_path {
        lines.push(format!(
            "User configuration: {}{}",
            path.display(),
            if path.exists() {
                ""
            } else {
                " (not created; defaults apply)"
            }
        ));
    }
    for source in &loaded.sources {
        lines.push(format!(
            "Loaded {}{}",
            source.kind,
            source
                .path
                .as_ref()
                .map_or(String::new(), |path| format!(": {}", path.display()))
        ));
    }
    lines.extend([
        String::new(),
        "Overrides (unlisted keys use defaults):".into(),
    ]);
    for (key, source) in &loaded.provenance {
        lines.push(format!(
            "{key}: {}{}",
            source.kind,
            source
                .path
                .as_ref()
                .map_or(String::new(), |path| format!(" ({})", path.display()))
        ));
    }
    lines.extend([
        String::new(),
        "Customize input with [leaders]. Provider paths and enabled flags belong in user config, not project config.".into(),
        "Optional services can be disabled with [providers.github] enabled = false or [providers.jira] enabled = false.".into(),
        "Jira key_prefix only expands numeric issue keys; it is not an API credential or the CLI's active project.".into(),
        "Authentication and server settings belong to gh / Jira CLI. tg never stores credentials.".into(),
        "After changing tg configuration, restart tg. After changing environment credentials, restart from the updated shell. External CLI credential/config changes can be retested here.".into(),
    ]);
    lines
}

pub fn detail_lines(report: &ProviderHealth) -> Vec<String> {
    let mut lines = vec![
        format!("{} {}", report.leader, report.name),
        format!("Status: {}", report.state.label()),
        report.summary.clone(),
    ];
    if let Some(target) = &report.target {
        lines.push(format!("Target: {target}"));
    }
    lines.push(match report.checked_at {
        Some(checked) => format!(
            "Last result: {}s ago (this session)",
            now_seconds().saturating_sub(checked)
        ),
        None if matches!(
            report.kind,
            ReferenceKind::GitHubIssue
                | ReferenceKind::GitHubPullRequest
                | ReferenceKind::JiraIssue
        ) =>
        {
            "Last check: never; configured does not mean verified".into()
        }
        None => "Local feature; no service credentials required".into(),
    });
    lines.extend([String::new(), format!("Try: {}", report.example)]);
    lines.extend(report.details.clone());
    lines.extend([String::new(), "Next steps:".into()]);
    lines.extend(report.actions.iter().map(|action| format!("  {action}")));
    lines
}

pub fn doctor(
    repository: &Repository,
    loaded: &LoadedConfig,
    check: Option<&str>,
    json: bool,
) -> Result<bool> {
    let mut reports = diagnostics::inventory(repository, &loaded.config);
    let mut failed = false;
    if let Some(scope) = check {
        for report in &mut reports {
            let selected = match scope {
                "all" => matches!(
                    report.kind,
                    ReferenceKind::GitHubIssue
                        | ReferenceKind::GitHubPullRequest
                        | ReferenceKind::JiraIssue
                ),
                "github" => matches!(
                    report.kind,
                    ReferenceKind::GitHubIssue | ReferenceKind::GitHubPullRequest
                ),
                "jira" => report.kind == ReferenceKind::JiraIssue,
                _ => false,
            };
            if selected && report.state != HealthState::Disabled {
                *report = diagnostics::check(
                    repository,
                    &loaded.config,
                    report.kind,
                    &CancellationFlag::default(),
                );
                failed |= report.state.is_problem();
            }
        }
    }
    let configuration = configuration_lines(loaded);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "providers": reports, "configuration": configuration })
            )?
        );
    } else {
        println!("References & connections\n");
        println!(
            "{:<8} {:<24} {:<24} Target",
            "Leader", "Reference", "Status"
        );
        for report in &reports {
            println!(
                "{}",
                clean_text(&format!(
                    "{:<8} {:<24} {:<24} {}",
                    report.leader,
                    report.name,
                    report.state.label(),
                    report.target.as_deref().unwrap_or("-")
                ))
            );
        }
        println!("\nSetup and connection details\n");
        for report in reports.iter().filter(|report| {
            report.state.is_problem()
                || matches!(
                    report.kind,
                    ReferenceKind::GitHubIssue
                        | ReferenceKind::GitHubPullRequest
                        | ReferenceKind::JiraIssue
                )
        }) {
            println!("{}", clean_text(&detail_lines(report).join("\n")));
            println!();
        }
        println!("{}", clean_text(&configuration.join("\n")));
        if check.is_none() {
            println!(
                "\nNo network checks run. Use `tg doctor --check jira`, `--check github`, or `--check all`.\nFor guided onboarding, run `tg setup`. In the editor, use `:providers` or Space p."
            );
        }
    }
    Ok(failed)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn panel() -> ProviderPanel {
        let repo = Repository {
            invocation_root: "/fixture".into(),
            search_root: "/fixture".into(),
            git_aware: true,
        };
        let mut panel = ProviderPanel::new(repo, Config::default());
        panel.reports = [
            (ReferenceKind::GitFile, "@", "Files"),
            (ReferenceKind::BroadFile, "%", "Broad files"),
            (ReferenceKind::Symbol, "::", "Symbols"),
            (ReferenceKind::Skill, "$", "Skills"),
            (ReferenceKind::GitHubIssue, "#", "GitHub issues"),
            (
                ReferenceKind::GitHubPullRequest,
                "!",
                "GitHub pull requests",
            ),
            (ReferenceKind::JiraIssue, "&", "Jira issues"),
        ]
        .into_iter()
        .map(|(kind, leader, name)| ProviderHealth {
            kind,
            leader: leader.into(),
            name: name.into(),
            example: format!("{leader}example"),
            state: HealthState::NotChecked,
            summary: "Not yet checked".into(),
            target: Some("https://jira.example.test/jira".into()),
            details: Vec::new(),
            actions: vec!["Configure the CLI, then retry".into()],
            checked_at: None,
        })
        .collect();
        panel.open();
        panel
    }

    fn press(panel: &mut ProviderPanel, code: KeyCode) {
        panel.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn render(panel: &mut ProviderPanel, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| panel.draw(frame, frame.area(), false))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_leader_is_visible_and_last_selection_scrolls_into_a_small_viewport() {
        let mut panel = panel();
        let wide = render(&mut panel, 120, 30);
        for name in [
            "Files",
            "Broad files",
            "Symbols",
            "Skills",
            "GitHub issues",
            "GitHub pull requests",
            "Jira issues",
        ] {
            assert!(wide.contains(name), "{name} missing from {wide}");
        }
        press(&mut panel, KeyCode::End);
        let narrow = render(&mut panel, 40, 8);
        assert!(narrow.contains("> & Jira issues"), "{narrow}");
        assert!(narrow.contains("Not checked"), "{narrow}");
        assert!(narrow.contains("Enter details"));
    }

    #[test]
    fn wrapped_details_can_scroll_to_the_last_recovery_step() {
        let mut panel = panel();
        panel.selection.select(Some(6));
        panel.reports[6]
            .actions
            .push("最後の手順: retry-after-fixing-the-self-hosted-instance".into());
        panel.reports[6].actions.push("Retry now".into());
        press(&mut panel, KeyCode::Enter);
        render(&mut panel, 40, 8);
        assert!(panel.max_scroll > 0);
        press(&mut panel, KeyCode::End);
        let rendered = render(&mut panel, 40, 8);
        assert!(rendered.contains("Retry now"), "{rendered}");
        assert!(rendered.contains("Esc/q back"));
        for line in wrapped_lines(vec!["日本語の設定 abcdefghijklmnopqrstuvwxyz".into()], 12)
        {
            assert!(line.width() <= 12);
        }
    }

    #[test]
    fn testing_requires_confirmation_and_escape_does_not_start_a_worker() {
        let mut panel = panel();
        panel.selection.select(Some(6));
        press(&mut panel, KeyCode::Char('r'));
        assert_eq!(panel.confirmation, Some(vec![ReferenceKind::JiraIssue]));
        assert!(panel.pending.is_none());
        let rendered = render(&mut panel, 80, 24);
        assert!(rendered.contains("https://jira.example.test/jira"));
        assert!(rendered.contains("Enter/y test"));
        press(&mut panel, KeyCode::Esc);
        assert!(panel.confirmation.is_none());
        assert!(panel.pending.is_none());
    }

    #[test]
    fn errors_survive_dismissal_and_queries_do_not_clear_a_failed_auth_check() {
        let mut panel = panel();
        panel.record_failure(ReferenceKind::JiraIssue, "HTTP 401 unauthorized");
        press(&mut panel, KeyCode::Char('q'));
        panel.open();
        assert_eq!(panel.reports[6].state, HealthState::AuthFailed);
        panel.record_query_success(ReferenceKind::JiraIssue);
        assert_eq!(panel.reports[6].state, HealthState::NotChecked);
        panel.reports[6].state = HealthState::AuthFailed;
        panel.record_query_success(ReferenceKind::JiraIssue);
        assert_eq!(panel.reports[6].state, HealthState::AuthFailed);
    }

    #[test]
    fn cancelled_and_stale_checks_cannot_overwrite_newer_failures() {
        let mut panel = panel();
        let previous = panel.reports[6].clone();
        let cancellation = CancellationFlag::default();
        panel.pending = Some(PendingCheck {
            id: 7,
            cancellation: cancellation.clone(),
            previous: vec![previous.clone()],
        });
        panel.reports[6].state = HealthState::Checking;
        panel.cancel_check();
        assert!(cancellation.is_cancelled());
        assert_eq!(panel.reports[6].state, previous.state);
        let mut stale = previous.clone();
        stale.state = HealthState::Ready;
        panel.sender.send((7, stale.clone())).unwrap();
        panel.poll();
        assert_eq!(panel.reports[6].state, previous.state);
        panel.pending = Some(PendingCheck {
            id: 8,
            cancellation: CancellationFlag::default(),
            previous: vec![previous],
        });
        panel.reports[6].state = HealthState::Checking;
        panel.record_failure(ReferenceKind::JiraIssue, "HTTP 401 unauthorized");
        panel.sender.send((8, stale)).unwrap();
        panel.poll();
        assert_eq!(panel.reports[6].state, HealthState::AuthFailed);
    }
}
