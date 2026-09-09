use super::render::wrapped_lines;
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
    for line in wrapped_lines(vec!["日本語の設定 abcdefghijklmnopqrstuvwxyz".into()], 12) {
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
