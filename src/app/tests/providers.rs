use super::*;

#[test]
fn provider_view_preserves_editing_and_command_input() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.providers.github.enabled = false;
    config.providers.jira.enabled = false;
    let mut app = app_for(temp.path(), &config, "keep this prompt");
    app.editor.set_cursor_char_offset(5).unwrap();
    let before = (
        app.document.text().to_owned(),
        app.document.revision(),
        app.editor.cursor_char_offset(),
    );
    app.dispatch_command(":providers".into());
    assert!(app.providers.visible);
    app.handle_event(Event::Paste("must not enter the prompt".into()));
    for code in [
        KeyCode::End,
        KeyCode::Enter,
        KeyCode::PageDown,
        KeyCode::Esc,
        KeyCode::Esc,
    ] {
        app.handle_event(Event::Key(event::KeyEvent::new(code, KeyModifiers::NONE)));
    }
    assert!(!app.providers.visible);
    assert_eq!(
        (
            app.document.text().to_owned(),
            app.document.revision(),
            app.editor.cursor_char_offset()
        ),
        before
    );
    for code in [KeyCode::Char(' '), KeyCode::Char('p')] {
        app.handle_event(Event::Key(event::KeyEvent::new(code, KeyModifiers::NONE)));
    }
    assert!(app.providers.visible);
    app.handle_event(Event::Key(event::KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    )));
    for character in ":r !printf".chars() {
        app.handle_event(Event::Key(event::KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::NONE,
        )));
    }
    assert!(!app.providers.visible);
    assert_eq!(app.editor.command_line(), Some("r !printf"));
}

#[test]
fn provider_recovery_hint_survives_long_filenames_and_status_expiry() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_for(temp.path(), &Config::default(), "prompt");
    app.save_target = Some(
        SaveTarget::open(&temp.path().join("a-long-prompt-filename-".repeat(8)))
            .unwrap()
            .target,
    );
    app.providers
        .record_failure(ReferenceKind::JiraIssue, "HTTP 401 unauthorized");
    app.status = "Jira authentication failed".into();
    let start = Instant::now();
    app.tick(start);
    app.tick(start + app.status_timeout + Duration::from_millis(1));
    assert!(app.status.is_empty());
    let status = status_text(&app, 40);
    assert!(status.contains("Space p"), "{status}");
    assert!(app.providers.problem_count() > 0);
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
    wait_for(&mut app, |app| {
        app.providers.reports.iter().any(|report| {
            report.kind == ReferenceKind::GitHubIssue
                && report.state == crate::references::diagnostics::HealthState::MissingExecutable
        })
    });
    assert!(app.status.contains(":providers"));
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
