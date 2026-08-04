use crate::language::{self, ParsedFile, Symbol};
use crate::preview;
use crate::repository::Repository;
use crate::search::{self, FileMatch, SearchMode};
use anyhow::Result;
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
use std::path::PathBuf;

enum Candidate {
    File(FileMatch),
    Symbol(Symbol),
}

struct ResolvedSpan {
    start: usize,
    end: usize,
    lowered: String,
    path: PathBuf,
}

struct App {
    repo: Repository,
    text: String,
    ordinary: Vec<PathBuf>,
    broad: Vec<PathBuf>,
    candidates: Vec<Candidate>,
    selected: usize,
    accepted: Option<String>,
    parsed: Option<(String, ParsedFile)>,
    status: String,
    preview_scroll: usize,
    resolutions: Vec<ResolvedSpan>,
}

impl App {
    fn new(repo: Repository) -> Self {
        let ordinary = search::walk(&repo.search_root, SearchMode::GitAware);
        let broad = search::walk(&repo.search_root, SearchMode::Broad);
        let status = format!(
            "{} files indexed · {} mode",
            broad.len(),
            if repo.git_aware {
                "Git-aware @ + broad %"
            } else {
                "no Git repository; @ uses local ignores"
            }
        );
        Self {
            repo,
            text: String::new(),
            ordinary,
            broad,
            candidates: Vec::new(),
            selected: 0,
            accepted: None,
            parsed: None,
            status,
            preview_scroll: 0,
            resolutions: Vec::new(),
        }
    }

    fn current_token(&self) -> Option<&str> {
        self.text
            .rsplit(|c: char| c.is_whitespace())
            .next()
            .filter(|token| token.starts_with('@') || token.starts_with('%'))
    }

    fn refresh(&mut self) {
        self.candidates.clear();
        self.selected = 0;
        self.preview_scroll = 0;
        let Some(token) = self.current_token().map(str::to_owned) else {
            return;
        };
        if self.accepted.as_deref() == Some(&token) {
            return;
        }
        let sigil = token.as_bytes()[0] as char;
        let body = &token[1..];
        if let Some((file, query)) = body.split_once("::") {
            let needs_parse = self
                .parsed
                .as_ref()
                .is_none_or(|(cached, _)| cached != file);
            if needs_parse {
                let path = self.repo.search_root.join(file);
                match language::parse(&path) {
                    Ok(Some(parsed)) => self.parsed = Some((file.to_owned(), parsed)),
                    Ok(None) => {
                        self.status = "Unsupported file type: whole-file selection only".into();
                        return;
                    }
                    Err(error) => {
                        self.status = error.to_string();
                        return;
                    }
                }
            }
            if let Some((_, parsed)) = &self.parsed {
                self.candidates = language::find_symbols(&parsed.symbols, query)
                    .into_iter()
                    .take(100)
                    .cloned()
                    .map(Candidate::Symbol)
                    .collect();
                self.status = format!(
                    "SYMBOLS · {} matches · Enter selects",
                    self.candidates.len()
                );
            }
        } else {
            let (mode, files) = if sigil == '@' {
                (SearchMode::GitAware, &self.ordinary)
            } else {
                (SearchMode::Broad, &self.broad)
            };
            self.candidates = search::find(&self.repo.search_root, files, body, 100)
                .into_iter()
                .map(Candidate::File)
                .collect();
            self.status = format!(
                "{} SEARCH · {} matches · type to filter",
                if mode == SearchMode::Broad {
                    "% BROAD"
                } else {
                    "@ GIT"
                },
                self.candidates.len()
            );
        }
    }

    fn accept(&mut self) {
        let Some(candidate) = self.candidates.get(self.selected) else {
            return;
        };
        let old_token = self.current_token().unwrap_or_default().to_owned();
        let (replacement, lowered, path) = match candidate {
            Candidate::File(file) => (
                format!("{}{}", &old_token[..1], file.relative),
                file.relative.clone(),
                file.path.clone(),
            ),
            Candidate::Symbol(symbol) => {
                let prefix = old_token
                    .split_once("::")
                    .map(|(p, _)| p)
                    .unwrap_or(&old_token);
                let file = prefix[1..].to_owned();
                (
                    format!("{prefix}::{}", symbol.leaf_name),
                    format!("{file}:{}:{}", symbol.start.line, symbol.start.column),
                    self.repo.search_root.join(file),
                )
            }
        };
        let start = self.text.len() - old_token.len();
        self.text.replace_range(start.., &replacement);
        self.resolutions.retain(|span| span.start != start);
        self.resolutions.push(ResolvedSpan {
            start,
            end: start + replacement.len(),
            lowered,
            path,
        });
        self.accepted = Some(replacement);
        self.candidates.clear();
        self.status = "Resolved ✓ · type :: after a file for symbols · Ctrl-J submits".into();
    }

    fn submit(&mut self) {
        if self.current_token().is_some_and(|token| {
            (token.starts_with('@') || token.starts_with('%'))
                && self.accepted.as_deref() != Some(token)
        }) {
            self.status = "Resolve or remove the active reference before submitting".into();
            return;
        }
        let result = self.lower_resolved();
        match result {
            Ok(lowered) => {
                let _ = writeln!(io::stdout(), "{lowered}");
                let _ = io::stdout().flush();
                self.text.clear();
                self.accepted = None;
                self.parsed = None;
                self.candidates.clear();
                self.resolutions.clear();
                self.status = "Submitted ✓ · ready for another prompt".into();
            }
            Err(error) => self.status = format!("Cannot submit: {error}"),
        }
    }

    fn lower_resolved(&self) -> Result<String> {
        let mut lowered = self.text.clone();
        let mut spans: Vec<_> = self.resolutions.iter().collect();
        spans.sort_by_key(|span| std::cmp::Reverse(span.start));
        for span in spans {
            anyhow::ensure!(span.path.is_file(), "selected file no longer exists");
            anyhow::ensure!(span.end <= lowered.len(), "selected reference is stale");
            lowered.replace_range(span.start..span.end, &span.lowered);
        }
        Ok(lowered)
    }

    fn key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('d') if self.text.is_empty() => return true,
                KeyCode::Char('c') => {
                    if !self.candidates.is_empty() {
                        self.candidates.clear();
                        self.status = "Completion dismissed".into();
                    } else if !self.text.is_empty() {
                        self.text.clear();
                        self.accepted = None;
                        self.status = "Composer cleared; Ctrl-C again exits".into();
                    } else {
                        return true;
                    }
                    return false;
                }
                KeyCode::Char('j') => {
                    self.submit();
                    return false;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Enter if !self.candidates.is_empty() => self.accept(),
            KeyCode::Enter => self.submit(),
            KeyCode::Up if !self.candidates.is_empty() => {
                self.selected = self.selected.saturating_sub(1)
            }
            KeyCode::Down if !self.candidates.is_empty() => {
                self.selected = (self.selected + 1).min(self.candidates.len() - 1)
            }
            KeyCode::PageUp => self.preview_scroll = self.preview_scroll.saturating_sub(5),
            KeyCode::PageDown => self.preview_scroll += 5,
            KeyCode::Esc => {
                self.candidates.clear();
                self.status =
                    "Completion dismissed; sigil will be submitted literally only after removal"
                        .into();
            }
            KeyCode::Backspace => {
                self.text.pop();
                self.resolutions.retain(|span| span.end <= self.text.len());
                self.accepted = None;
                self.refresh();
            }
            KeyCode::Char(character) => {
                self.text.push(character);
                self.accepted = None;
                self.refresh();
            }
            _ => {}
        }
        false
    }
}

fn preview_lines(app: &App) -> Vec<Line<'static>> {
    match app.candidates.get(app.selected) {
        Some(Candidate::File(file)) => std::fs::read_to_string(&file.path)
            .map(|source| {
                source
                    .lines()
                    .take(40)
                    .enumerate()
                    .map(|(i, line)| Line::from(format!("{:>5} │ {}", i + 1, line)))
                    .collect()
            })
            .unwrap_or_else(|_| vec![Line::from("Preview unavailable")]),
        Some(Candidate::Symbol(symbol)) => app
            .parsed
            .as_ref()
            .map(|(_, parsed)| {
                preview::window(parsed, symbol, 5)
                    .into_iter()
                    .map(|(number, line)| {
                        let style = if number == symbol.start.line {
                            Style::default().fg(Color::Black).bg(Color::LightYellow)
                        } else {
                            Style::default()
                        };
                        Line::styled(format!("{number:>5} │ {line}"), style)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        None => vec![
            Line::from("Type @ for Git-aware files or % to include ignored files."),
            Line::from("After choosing a file, type :: to search declarations."),
        ],
    }
}

fn draw(frame: &mut ratatui::Frame, app: &App) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
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
    let middle = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(vertical[1]);
    let items: Vec<_> = app
        .candidates
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let text = match item {
                Candidate::File(file) => format!(
                    "{} {}",
                    if app.current_token().is_some_and(|t| t.starts_with('%')) {
                        "%"
                    } else {
                        "@"
                    },
                    file.relative
                ),
                Candidate::Symbol(symbol) => format!(
                    "{}  {}  · {}:{}",
                    symbol.qualified_name, symbol.kind, symbol.start.line, symbol.start.column
                ),
            };
            let style = if index == app.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(text).style(style)
        })
        .collect();
    frame.render_widget(
        List::new(items).block(Block::default().title(" Matches ").borders(Borders::ALL)),
        middle[0],
    );
    frame.render_widget(
        Paragraph::new(preview_lines(app))
            .scroll((app.preview_scroll as u16, 0))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Preview · PgUp/PgDn ")
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
                    .title(" Prompt · Enter selects/submits · Ctrl-J always submits ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
        vertical[2],
    );
    frame.render_widget(
        Paragraph::new(format!(" {}", app.status)).style(Style::default().fg(Color::DarkGray)),
        vertical[3],
    );
}

pub fn run(repo: Repository) -> Result<()> {
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;
    let result = (|| -> Result<()> {
        let mut app = App::new(repo);
        loop {
            terminal.draw(|frame| draw(frame, &app))?;
            if let Event::Key(key) = event::read()?
                && key.kind == crossterm::event::KeyEventKind::Press
                && app.key(key)
            {
                break;
            }
        }
        Ok(())
    })();
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

pub fn is_terminal() -> bool {
    crossterm::tty::IsTty::is_tty(&io::stdin())
}
