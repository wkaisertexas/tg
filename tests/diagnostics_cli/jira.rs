use super::*;

#[test]
fn jira_invalid_token_keeps_the_exact_self_hosted_target_and_safe_recovery_actions() {
    let fixture = Fixture::new();
    fixture.mode("jira", "auth");
    let started = unix_seconds();
    let report = report(fixture.doctor().args(["--check", "jira"]), 1);
    let jira = provider(&report, ReferenceKind::JiraIssue);
    assert_eq!(jira["state"], "auth_failed");
    assert_eq!(jira["target"], JIRA_SERVER);
    let checked = jira["checked_at"].as_u64().unwrap();
    assert!(checked >= started && checked <= unix_seconds());
    let actions = lines(&jira["actions"]);
    for expected in ["jira init", "bearer", "basic", "mTLS", "restart tg"] {
        assert!(actions.contains(expected), "missing {expected}: {actions}");
    }
    assert!(lines(&jira["details"]).contains("Configured server (unverified)"));
    assert_eq!(
        fixture.calls("jira"),
        vec![vec![
            fixture.cwd.to_str().unwrap().to_owned(),
            "issue".into(),
            "list".into(),
            "--raw".into(),
            "--paginate".into(),
            "1".into()
        ]]
    );
    assert!(fixture.calls("gh").is_empty());
    assert!(fixture.calls("git").is_empty());
}

#[test]
fn jira_permission_network_timeout_malformed_and_unsupported_failures_are_distinct_and_safe() {
    for (mode, expected) in [
        ("denied", "access_denied"),
        ("tls", "connection_failed"),
        ("dns", "connection_failed"),
        ("timeout", "timed_out"),
        ("malformed", "failed"),
        ("unsupported", "unsupported"),
    ] {
        let fixture = Fixture::new();
        fixture.write_config(true, true, if mode == "timeout" { 150 } else { 1_500 }, "");
        fixture.mode("jira", mode);
        let report = report(fixture.doctor().args(["--check", "jira"]), 1);
        let jira = provider(&report, ReferenceKind::JiraIssue);
        assert_eq!(jira["state"], expected, "mode {mode}");
        assert_eq!(jira["target"], JIRA_SERVER);
        assert!(jira["checked_at"].is_u64());
        assert!(!jira["actions"].as_array().unwrap().is_empty());
        assert_eq!(fixture.calls("jira").len(), 1);
        assert!(fixture.calls("gh").is_empty());
    }
}

#[test]
fn jira_success_and_empty_searches_never_claim_independent_authentication() {
    let fixture = Fixture::new();
    for mode in ["ready", "empty", "envelope", "no-results"] {
        fixture.mode("jira", mode);
        let report = report(fixture.doctor().args(["--check", "jira"]), 0);
        let jira = provider(&report, ReferenceKind::JiraIssue);
        assert_eq!(jira["state"], "access_verified", "mode {mode}");
        assert_ne!(jira["state"], "ready");
        let summary = jira["summary"].as_str().unwrap().to_ascii_lowercase();
        assert!(summary.contains("authentication"));
        assert!(summary.contains("not independently verified"));
        assert_eq!(jira["target"], JIRA_SERVER);
        assert!(jira["checked_at"].is_u64());
    }
    assert_eq!(fixture.calls("jira").len(), 4);
    assert!(
        fixture
            .calls("jira")
            .iter()
            .all(|call| call[1..] == ["issue", "list", "--raw", "--paginate", "1"])
    );
    assert!(fixture.calls("gh").is_empty());
    let output = output(
        fixture
            .command()
            .args(["doctor", "--root"])
            .arg(&fixture.repo)
            .args(["--check", "jira", "--no-color"]),
        0,
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Search works"));
}

#[test]
fn retry_rereads_jira_metadata_and_replaces_a_previous_authentication_failure() {
    let fixture = Fixture::new();
    fixture.mode("jira", "auth");
    let failed = report(fixture.doctor().args(["--check", "jira"]), 1);
    assert_eq!(
        provider(&failed, ReferenceKind::JiraIssue)["state"],
        "auth_failed"
    );
    fixture.write_jira_config("https://new-jira.corp.example/other/jira");
    fixture.mode("jira", "empty");
    let recovered = report(fixture.doctor().args(["--check", "jira"]), 0);
    let jira = provider(&recovered, ReferenceKind::JiraIssue);
    assert_eq!(jira["state"], "access_verified");
    assert_eq!(jira["target"], "https://new-jira.corp.example/other/jira");
    assert!(
        jira["checked_at"].as_u64().unwrap()
            >= provider(&failed, ReferenceKind::JiraIssue)["checked_at"]
                .as_u64()
                .unwrap()
    );
    assert!(!lines(&jira["actions"]).contains("jira init"));
    assert_eq!(fixture.calls("jira").len(), 2);
    assert!(fixture.calls("gh").is_empty());
}

#[test]
fn jira_metadata_uses_only_the_selected_config_and_supported_server_override() {
    let fixture = Fixture::new();
    fs::write(&fixture.jira_config, format!("server: [{CONFIG_SECRET}\n")).unwrap();
    let selected = fixture.cwd.join("selected-jira.yml");
    fs::write(
        &selected,
        jira_config_text("https://selected.corp.example/jira"),
    )
    .unwrap();
    let report = report(
        fixture
            .doctor()
            .env("JIRA_CONFIG_FILE", "selected-jira.yml")
            .env("JIRA_SERVER", "https://override.corp.example:8443/jira")
            .env("JIRA_PROJECT.KEY", "OVERRIDE"),
        0,
    );
    let jira = provider(&report, ReferenceKind::JiraIssue);
    assert_eq!(jira["state"], "not_checked");
    assert_eq!(jira["target"], "https://override.corp.example:8443/jira");
    let details = lines(&jira["details"]);
    assert!(details.contains(selected.to_str().unwrap()));
    assert!(details.contains("Configured project: OVERRIDE"));
    assert!(!details.contains(fixture.jira_config.to_str().unwrap()));
    fixture.assert_no_probes();
}

#[test]
fn jira_default_metadata_prefers_xdg_and_falls_back_to_the_isolated_home() {
    let fixture = Fixture::new();
    let home_config = fixture.home.join(".config/.jira/.config.yml");
    fs::create_dir_all(home_config.parent().unwrap()).unwrap();
    fs::write(
        &home_config,
        jira_config_text("https://home-jira.corp.example/jira"),
    )
    .unwrap();
    let xdg = report(&mut fixture.doctor(), 0);
    assert_eq!(
        provider(&xdg, ReferenceKind::JiraIssue)["target"],
        JIRA_SERVER
    );
    let home = report(fixture.doctor().env_remove("XDG_CONFIG_HOME"), 0);
    let jira = provider(&home, ReferenceKind::JiraIssue);
    assert_eq!(jira["target"], "https://home-jira.corp.example/jira");
    assert_eq!(jira["state"], "not_checked");
    assert!(lines(&jira["details"]).contains(home_config.to_str().unwrap()));
    fixture.assert_no_probes();
}

#[test]
fn unsafe_jira_urls_and_malformed_active_config_are_not_exposed_or_probed() {
    let fixture = Fixture::new();
    for server in [
        format!("https://user:{JIRA_SECRET}@jira.corp.example/jira"),
        format!("https://jira.corp.example/jira?token={JIRA_SECRET}"),
        format!("https://jira.corp.example/jira#{JIRA_SECRET}"),
        format!("ftp://jira.corp.example/{JIRA_SECRET}"),
    ] {
        fixture.write_jira_config(&server);
        let report = report(fixture.doctor().args(["--check", "jira"]), 1);
        let jira = provider(&report, ReferenceKind::JiraIssue);
        assert_eq!(jira["state"], "not_configured");
        assert!(jira["target"].is_null());
    }
    fs::write(&fixture.jira_config, format!("server: [{CONFIG_SECRET}\n")).unwrap();
    let report = report(fixture.doctor().args(["--check", "jira"]), 1);
    assert_eq!(
        provider(&report, ReferenceKind::JiraIssue)["state"],
        "not_configured"
    );
    assert!(
        provider(&report, ReferenceKind::JiraIssue)["summary"]
            .as_str()
            .unwrap()
            .contains("parsed safely")
    );
    fixture.assert_no_probes();
}
