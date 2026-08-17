use crate::language::{self, ParsedFile, Symbol};
use crate::repository::Repository;
use crate::search::{self, FileMatch, SearchMode};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use rayon::prelude::*;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone)]
struct RepositorySymbol {
    relative: Arc<str>,
    path: Arc<PathBuf>,
    symbol: Symbol,
}

enum Candidate {
    File(FileMatch),
    Symbol(Symbol),
    RepositorySymbol(RepositorySymbol),
}

enum IndexMessage {
    Batch {
        symbols: Vec<RepositorySymbol>,
        scanned: usize,
    },
    Done,
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
    transcript: Vec<String>,
    transcript_scroll: usize,
    resolutions: Vec<ResolvedSpan>,
    repository_symbols: Vec<RepositorySymbol>,
    index_receiver: Option<Receiver<IndexMessage>>,
    index_started: Option<Instant>,
    index_scanned: usize,
    index_total: usize,
    index_done: bool,
    pending_clipboard: Option<String>,
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
            transcript: Vec::new(),
            transcript_scroll: 0,
            resolutions: Vec::new(),
            repository_symbols: Vec::new(),
            index_receiver: None,
            index_started: None,
            index_scanned: 0,
            index_total: 0,
            index_done: false,
            pending_clipboard: None,
        }
    }

    fn current_token(&self) -> Option<&str> {
        self.text
            .rsplit(|c: char| c.is_whitespace())
            .next()
            .filter(|token| {
                token.starts_with('@') || token.starts_with('%') || token.starts_with("::")
            })
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
        if let Some(query) = token.strip_prefix("::") {
            self.start_repository_index();
            self.refresh_repository_symbols(query);
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
                    "SYMBOLS · {} matches · Tab/Enter selects",
                    self.candidates.len()
                );
                self.center_preview();
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

    fn start_repository_index(&mut self) {
        if self.index_receiver.is_some() || self.index_done {
            return;
        }
        let files: Vec<_> = self
            .broad
            .iter()
            .filter(|path| language::supports(path))
            .cloned()
            .collect();
        self.index_total = files.len();
        self.index_started = Some(Instant::now());
        let root = self.repo.search_root.clone();
        let (sender, receiver) = mpsc::channel();
        self.index_receiver = Some(receiver);
        thread::spawn(move || {
            language::install_indexing(|| {
                files
                    .par_chunks(64)
                    .for_each_with(sender.clone(), |sender, paths| {
                        let mut symbols = Vec::new();
                        for path in paths {
                            if let Ok(Some(parsed)) = language::parse_indexed(path) {
                                let relative: Arc<str> = path
                                    .strip_prefix(&root)
                                    .unwrap_or(path)
                                    .to_string_lossy()
                                    .replace('\\', "/")
                                    .into();
                                let path = Arc::new(path.clone());
                                symbols.extend(parsed.symbols.into_iter().map(|symbol| {
                                    RepositorySymbol {
                                        relative: Arc::clone(&relative),
                                        path: Arc::clone(&path),
                                        symbol,
                                    }
                                }));
                            }
                        }
                        let _ = sender.send(IndexMessage::Batch {
                            symbols,
                            scanned: paths.len(),
                        });
                    });
            });
            let _ = sender.send(IndexMessage::Done);
        });
    }

    fn drain_repository_index(&mut self) {
        let mut changed = false;
        let mut finished = false;
        if let Some(receiver) = &self.index_receiver {
            while let Ok(message) = receiver.try_recv() {
                match message {
                    IndexMessage::Batch { symbols, scanned } => {
                        self.repository_symbols.extend(symbols);
                        self.index_scanned += scanned;
                        changed = true;
                    }
                    IndexMessage::Done => {
                        self.index_done = true;
                        finished = true;
                    }
                }
            }
        }
        if finished {
            self.index_receiver = None;
        }
        if (changed || finished)
            && let Some(query) = self
                .current_token()
                .and_then(|token| token.strip_prefix("::"))
                .map(str::to_owned)
        {
            self.refresh_repository_symbols(&query);
        }
    }

    fn refresh_repository_symbols(&mut self, query: &str) {
        self.candidates = rank_repository_symbols(&self.repository_symbols, query)
            .into_iter()
            .map(Candidate::RepositorySymbol)
            .collect();
        let elapsed = self
            .index_started
            .map_or(0.0, |start| start.elapsed().as_secs_f32());
        self.status = format!(
            "REPO SYMBOLS · {}/{} files · {} symbols · {:.2}s{}",
            self.index_scanned,
            self.index_total,
            self.repository_symbols.len(),
            elapsed,
            if self.index_done {
                " · complete"
            } else {
                " · indexing…"
            }
        );
        self.selected = self.selected.min(self.candidates.len().saturating_sub(1));
        self.center_preview();
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
                    format!("{prefix}::{}", language::display_name(symbol)),
                    language::lowered_reference(&file, symbol),
                    self.repo.search_root.join(file),
                )
            }
            Candidate::RepositorySymbol(candidate) => (
                format!(
                    "@{}::{}",
                    candidate.relative,
                    language::display_name(&candidate.symbol)
                ),
                language::lowered_reference(candidate.relative.as_ref(), &candidate.symbol),
                candidate.path.as_ref().clone(),
            ),
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
            (token.starts_with('@') || token.starts_with('%') || token.starts_with("::"))
                && self.accepted.as_deref() != Some(token)
        }) {
            self.status = "Resolve or remove the active reference before submitting".into();
            return;
        }
        let result = self.lower_resolved();
        match result {
            Ok(lowered) => {
                self.transcript.push(lowered.clone());
                self.pending_clipboard = Some(lowered);
                self.transcript_scroll = self.transcript.len().saturating_sub(3);
                self.text.clear();
                self.accepted = None;
                self.parsed = None;
                self.candidates.clear();
                self.resolutions.clear();
                self.status = "Submitted ✓ · copied with OSC 52 · ready for another prompt".into();
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

    fn center_preview(&mut self) {
        self.preview_scroll = match self.candidates.get(self.selected) {
            Some(Candidate::Symbol(symbol)) => centered_preview_scroll(symbol.start.line),
            Some(Candidate::RepositorySymbol(candidate)) => {
                centered_preview_scroll(candidate.symbol.start.line)
            }
            _ => 0,
        };
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
            code if !self.candidates.is_empty() && is_completion_accept_key(code) => self.accept(),
            KeyCode::Enter => self.submit(),
            KeyCode::Up if !self.candidates.is_empty() => {
                self.selected = self.selected.saturating_sub(1);
                self.center_preview();
            }
            KeyCode::Down if !self.candidates.is_empty() => {
                self.selected = (self.selected + 1).min(self.candidates.len() - 1);
                self.center_preview();
            }
            KeyCode::PageUp if self.candidates.is_empty() => {
                self.transcript_scroll = self.transcript_scroll.saturating_sub(3)
            }
            KeyCode::PageDown if self.candidates.is_empty() => {
                self.transcript_scroll =
                    (self.transcript_scroll + 3).min(self.transcript.len().saturating_sub(1))
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
                    .enumerate()
                    .map(|(i, line)| Line::from(format!("{:>5} │ {}", i + 1, line)))
                    .collect()
            })
            .unwrap_or_else(|_| vec![Line::from("Preview unavailable")]),
        Some(Candidate::Symbol(symbol)) => app
            .parsed
            .as_ref()
            .map(|(_, parsed)| {
                parsed
                    .source
                    .lines()
                    .enumerate()
                    .map(|(index, line)| {
                        let number = index + 1;
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
        Some(Candidate::RepositorySymbol(candidate)) => {
            std::fs::read_to_string(candidate.path.as_ref())
                .map(|source| highlighted_source_lines(&source, &candidate.symbol))
                .unwrap_or_else(|_| vec![Line::from("Preview unavailable")])
        }
        None => vec![
            Line::from("Type @ for files, % for ignored files, or :: for all symbols."),
            Line::from("After a symbol or Markdown heading, type . for children."),
        ],
    }
}

fn highlighted_source_lines(source: &str, symbol: &Symbol) -> Vec<Line<'static>> {
    source
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let number = index + 1;
            let style = if number == symbol.start.line {
                Style::default().fg(Color::Black).bg(Color::LightYellow)
            } else {
                Style::default()
            };
            Line::styled(format!("{number:>5} │ {line}"), style)
        })
        .collect()
}

fn rank_repository_symbols(index: &[RepositorySymbol], query: &str) -> Vec<RepositorySymbol> {
    let matcher = SkimMatcherV2::default().ignore_case();
    let member_query = query.rsplit_once('.').filter(|(parent, _)| {
        index.iter().any(|candidate| {
            language::names_equivalent(&candidate.symbol.leaf_name, parent)
                || language::names_equivalent(&candidate.symbol.qualified_name, parent)
        })
    });
    let query_lower = query.to_ascii_lowercase();
    let mut matches: Vec<_> = index
        .iter()
        .filter_map(|candidate| {
            let leaf = candidate.symbol.leaf_name.to_ascii_lowercase();
            let score = if let Some((parent, member)) = member_query {
                language::member_score(&candidate.symbol, parent, member, &matcher)?
            } else if query.is_empty() {
                0
            } else if leaf == query_lower {
                1_000_000
            } else if leaf.starts_with(&query_lower) {
                500_000
            } else {
                matcher
                    .fuzzy_match(&candidate.symbol.leaf_name, query)
                    .or_else(|| matcher.fuzzy_match(&candidate.symbol.qualified_name, query))?
            };
            Some((score, candidate))
        })
        .collect();
    let compare = |(left_score, left): &(i64, &RepositorySymbol),
                   (right_score, right): &(i64, &RepositorySymbol)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.relative.cmp(&right.relative))
            .then_with(|| left.symbol.start.line.cmp(&right.symbol.start.line))
    };
    if matches.len() > 100 {
        matches.select_nth_unstable_by(100, compare);
        matches.truncate(100);
    }
    matches.sort_by(compare);
    matches
        .into_iter()
        .take(100)
        .map(|(_, candidate)| candidate.clone())
        .collect()
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
    let transcript: Vec<_> = app
        .transcript
        .iter()
        .map(|line| {
            Line::from(vec![
                Span::styled("› ", Style::default().fg(Color::Cyan)),
                Span::raw(line.clone()),
            ])
        })
        .collect();
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
                Candidate::Symbol(symbol) if language::is_markdown_symbol(symbol) => format!(
                    "#{} · {}",
                    language::display_name(symbol),
                    symbol.start.line
                ),
                Candidate::Symbol(symbol) => format!(
                    "{}  {} · {}:{}",
                    symbol.qualified_name, symbol.kind, symbol.start.line, symbol.start.column
                ),
                Candidate::RepositorySymbol(candidate)
                    if language::is_markdown_symbol(&candidate.symbol) =>
                {
                    format!(
                        "{}#{} · {}",
                        candidate.relative,
                        language::display_name(&candidate.symbol),
                        candidate.symbol.start.line
                    )
                }
                Candidate::RepositorySymbol(candidate) => format!(
                    "{} :: {}  {} · {}:{}",
                    candidate.relative,
                    candidate.symbol.qualified_name,
                    candidate.symbol.kind,
                    candidate.symbol.start.line,
                    candidate.symbol.start.column
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
        Paragraph::new(format!(" {}", app.status)).style(Style::default().fg(Color::DarkGray)),
        vertical[4],
    );
}

pub fn run(repo: Repository) -> Result<()> {
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;
    let result = (|| -> Result<Vec<String>> {
        let mut app = App::new(repo);
        loop {
            app.drain_repository_index();
            terminal.draw(|frame| draw(frame, &app))?;
            let next_event = if app.index_receiver.is_some() {
                event::poll(Duration::from_millis(50))?
                    .then(event::read)
                    .transpose()?
            } else {
                Some(event::read()?)
            };
            if let Some(Event::Key(key)) = next_event
                && key.kind == crossterm::event::KeyEventKind::Press
            {
                let should_exit = app.key(key);
                if let Some(text) = app.pending_clipboard.take() {
                    write!(
                        terminal.backend_mut(),
                        "{}",
                        crate::clipboard::osc52_sequence(&text)
                    )?;
                    terminal.backend_mut().flush()?;
                }
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn repository_symbol_search_prefers_exact_leaf_names() {
        let candidate = |name: &str| RepositorySymbol {
            relative: "src/lib.rs".into(),
            path: Arc::new(PathBuf::from("src/lib.rs")),
            symbol: Symbol {
                leaf_name: name.into(),
                qualified_name: name.into(),
                kind: "function".into(),
                start: crate::language::SourcePoint { line: 1, column: 1 },
                name_start_byte: 0,
                name_end_byte: name.len(),
                range_start_byte: 0,
                range_end_byte: name.len(),
                is_definition: true,
            },
        };
        let index = [candidate("renderer"), candidate("render")];
        let matches = rank_repository_symbols(&index, "render");
        assert_eq!(matches[0].symbol.leaf_name, "render");
    }

    #[test]
    fn osc52_encodes_the_lowered_prompt() {
        assert_eq!(
            crate::clipboard::osc52_sequence("src/lib.rs"),
            "\u{1b}]52;c;c3JjL2xpYi5ycw==\u{7}"
        );
    }

    #[test]
    fn accepted_references_lower_independently_of_rendering() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("notes.txt");
        std::fs::write(&path, "notes\n").unwrap();
        let repo = Repository::discover(temp.path()).unwrap();
        let mut app = App::new(repo);
        app.text = "Read @notes.txt, then continue.".into();
        app.resolutions.push(ResolvedSpan {
            start: 5,
            end: 15,
            lowered: "notes.txt".into(),
            path,
        });

        assert_eq!(
            app.lower_resolved().unwrap(),
            "Read notes.txt, then continue."
        );
        assert_eq!(app.text, "Read @notes.txt, then continue.");
    }

    #[test]
    fn deleted_reference_targets_are_stale_without_losing_prompt_text() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("notes.txt");
        std::fs::write(&path, "notes\n").unwrap();
        let repo = Repository::discover(temp.path()).unwrap();
        let mut app = App::new(repo);
        app.text = "@notes.txt".into();
        app.resolutions.push(ResolvedSpan {
            start: 0,
            end: app.text.len(),
            lowered: "notes.txt".into(),
            path: path.clone(),
        });
        std::fs::remove_file(path).unwrap();

        assert_eq!(
            app.lower_resolved().unwrap_err().to_string(),
            "selected file no longer exists"
        );
        assert_eq!(app.text, "@notes.txt");
    }
}
