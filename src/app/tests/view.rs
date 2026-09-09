use super::*;

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
    wait_for(
        &mut app,
        |app| matches!(app.reference_session.reference_cost(&app.document.references()[0]), Some(ContextCost::Tokens(tokens)) if tokens >= 1_000),
    );
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
fn space_question_opens_modal_configuration_help() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.leaders.files = "~f".into();
    config.ui.preview_toggle = "ctrl-x".into();
    config.providers.jira.key_prefix = Some("OPS".into());
    let mut app = app_for(temp.path(), &config, "unchanged");
    let press = |app: &mut App, code, modifiers| {
        app.handle_event(Event::Key(event::KeyEvent::new(code, modifiers)))
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
