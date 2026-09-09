use super::*;

#[test]
fn inventory_lists_every_custom_leader_without_any_provider_process() {
    let fixture = Fixture::new();
    fixture.write_config(true, true, 1_500, "[leaders]\nfiles='@@'\nbroad_files='%%'\nsymbols=':::'\nskills='$$'\ngithub_issues='##'\ngithub_pull_requests='!!'\njira_issues='&&'\n");
    let report = report(&mut fixture.doctor(), 0);
    let providers = report["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 7);
    for ((health, kind), leader) in providers
        .iter()
        .zip(KINDS)
        .zip(["@@", "%%", ":::", "$$", "##", "!!", "&&"])
    {
        assert_eq!(health["kind"], serde_json::to_value(kind).unwrap());
        assert_eq!(health["leader"], leader);
        assert!(health["example"].as_str().unwrap().starts_with(leader));
        assert!(health["checked_at"].is_null());
    }
    for kind in [
        ReferenceKind::GitHubIssue,
        ReferenceKind::GitHubPullRequest,
        ReferenceKind::JiraIssue,
    ] {
        assert_eq!(provider(&report, kind)["state"], "not_checked");
    }
    let jira = provider(&report, ReferenceKind::JiraIssue);
    assert_eq!(jira["target"], JIRA_SERVER);
    assert_eq!(jira["example"], "&&OPS-123");
    let details = lines(&jira["details"]);
    assert!(details.contains(fixture.jira_config.to_str().unwrap()));
    assert!(details.contains(fixture.bin.join("jira").to_str().unwrap()));
    assert!(details.contains(&format!("Working directory: {}", fixture.cwd.display())));
    assert!(details.contains("Configured project: OPS"));
    let github = provider(&report, ReferenceKind::GitHubIssue);
    assert!(
        lines(&github["details"])
            .contains(&format!("Working directory: {}", fixture.repo.display()))
    );
    fixture.assert_no_probes();
}

#[test]
fn relative_executables_resolve_against_each_providers_actual_working_directory() {
    let fixture = Fixture::new();
    for (cwd, name, script) in [
        (&fixture.repo, "gh", FAKE_GH),
        (&fixture.cwd, "jira", FAKE_JIRA),
    ] {
        fs::create_dir(cwd.join("tools")).unwrap();
        executable(&cwd.join("tools").join(name), script);
        fs::write(cwd.join("tools").join(format!("{name}.mode")), "ready\n").unwrap();
    }
    let config = fixture
        .config_text(true, true, 1_500, "")
        .replace(&quoted_path(&fixture.bin.join("gh")), "'./tools/gh'")
        .replace(&quoted_path(&fixture.bin.join("jira")), "'./tools/jira'");
    fs::write(&fixture.config, config).unwrap();
    let inventory = report(&mut fixture.doctor(), 0);
    for (kind, path) in [
        (ReferenceKind::GitHubIssue, fixture.repo.join("./tools/gh")),
        (ReferenceKind::JiraIssue, fixture.cwd.join("./tools/jira")),
    ] {
        let health = provider(&inventory, kind);
        assert_eq!(health["state"], "not_checked");
        assert!(
            lines(&health["details"]).contains(&format!("Resolved executable: {}", path.display()))
        );
    }
    assert!(!fixture.repo.join("tools/gh.calls").exists());
    assert!(!fixture.cwd.join("tools/jira.calls").exists());
    let checked = report(fixture.doctor().args(["--check", "all"]), 0);
    assert_eq!(
        provider(&checked, ReferenceKind::GitHubIssue)["state"],
        "ready"
    );
    assert_eq!(
        provider(&checked, ReferenceKind::JiraIssue)["state"],
        "access_verified"
    );
    assert_eq!(
        fs::read_to_string(fixture.repo.join("tools/gh.calls"))
            .unwrap()
            .lines()
            .count(),
        6
    );
    assert_eq!(
        fs::read_to_string(fixture.cwd.join("tools/jira.calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    fixture.assert_no_probes();
}

#[test]
fn non_tty_setup_prints_helpful_local_report_without_terminal_control_or_checks() {
    let fixture = Fixture::new();
    let output = output(
        fixture
            .command()
            .args(["setup", "--root"])
            .arg(&fixture.repo)
            .arg("--no-color"),
        0,
    );
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "References & connections",
        "Files",
        "Broad files",
        "Symbols",
        "Skills",
        "GitHub issues",
        "GitHub pull requests",
        "Jira issues",
        "No network checks run",
        "tg doctor --check jira",
        "tg setup",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert!(!text.contains('\u{1b}'));
    assert!(output.stderr.is_empty());
    fixture.assert_no_probes();
}

#[test]
fn configuration_sources_and_post_subcommand_global_overrides_are_reported() {
    let fixture = Fixture::new();
    fixture.write_config(true, true, 1_500, "[leaders]\nfiles='@@'\n");
    fs::write(fixture.repo.join(".tg.toml"), "[leaders]\nfiles='~'\n").unwrap();
    let inherited = report(
        fixture
            .doctor()
            .env("TG_JIRA_COMMAND", fixture.bin.join("jira"))
            .arg("--no-color"),
        0,
    );
    assert_eq!(provider(&inherited, ReferenceKind::GitFile)["leader"], "~");
    let sources = lines(&inherited["configuration"]);
    for expected in [
        "Loaded user configuration",
        "Loaded project configuration",
        "Loaded environment",
        "Loaded command line",
        "leaders.files: project configuration",
        "providers.jira.command: environment",
    ] {
        assert!(sources.contains(expected), "missing {expected}: {sources}");
    }
    assert!(sources.contains(fixture.config.to_str().unwrap()));
    assert!(sources.contains(fixture.repo.join(".tg.toml").to_str().unwrap()));
    let override_path = fixture.cwd.join("override.toml");
    fs::write(
        &override_path,
        fixture.config_text(true, true, 1_500, "[leaders]\nfiles='~~'\n"),
    )
    .unwrap();
    let overridden = report(
        fixture
            .doctor()
            .env(
                "TG_CONFIG",
                fixture.cwd.join("nonexistent-environment-config.toml"),
            )
            .args([
                "--config",
                "override.toml",
                "--no-project-config",
                "--no-color",
            ]),
        0,
    );
    assert_eq!(
        provider(&overridden, ReferenceKind::GitFile)["leader"],
        "~~"
    );
    let sources = lines(&overridden["configuration"]);
    assert!(sources.contains(override_path.to_str().unwrap()));
    assert!(!sources.contains("Loaded project configuration"));
    fixture.assert_no_probes();
}

#[test]
fn configuration_errors_exit_before_printing_json_or_probing() {
    let fixture = Fixture::new();
    fs::write(&fixture.config, "[search]\nunknown=true\n").unwrap();
    let invalid = output(fixture.doctor().args(["--check", "all"]), 2);
    assert!(invalid.stdout.is_empty());
    let error = String::from_utf8(invalid.stderr).unwrap();
    assert!(error.contains("search.unknown"));
    assert!(error.contains(fixture.config.to_str().unwrap()));
    assert!(error.contains("could not load configuration"));
    let missing = fixture.cwd.join("missing.toml");
    let invalid = output(fixture.doctor().arg("--config").arg(&missing), 2);
    assert!(invalid.stdout.is_empty());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains(missing.to_str().unwrap()));
    fixture.assert_no_probes();
}

#[test]
fn forbidden_project_provider_settings_can_be_bypassed_after_the_subcommand() {
    let fixture = Fixture::new();
    fs::write(
        fixture.repo.join(".tg.toml"),
        "[providers.jira]\ncommand='untrusted-command'\n",
    )
    .unwrap();
    let invalid = output(fixture.doctor().args(["--check", "jira"]), 2);
    assert!(invalid.stdout.is_empty());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("providers.jira.command"));
    let recovered = report(fixture.doctor().arg("--no-project-config"), 0);
    assert_eq!(
        provider(&recovered, ReferenceKind::JiraIssue)["state"],
        "not_checked"
    );
    fixture.assert_no_probes();
}

#[test]
fn missing_cli_is_inventory_information_but_fails_an_explicit_check() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.bin.join("jira")).unwrap();
    let inventory = report(&mut fixture.doctor(), 0);
    assert_eq!(
        provider(&inventory, ReferenceKind::JiraIssue)["state"],
        "missing_executable"
    );
    let checked = report(fixture.doctor().args(["--check", "jira"]), 1);
    assert_eq!(
        provider(&checked, ReferenceKind::JiraIssue)["state"],
        "missing_executable"
    );
    assert!(provider(&checked, ReferenceKind::JiraIssue)["checked_at"].is_u64());
    fixture.assert_no_probes();
}

#[test]
fn disabled_providers_stay_visible_and_are_not_probed_even_when_binaries_are_absent() {
    let fixture = Fixture::new();
    fixture.write_config(false, false, 1_500, "");
    fs::remove_file(fixture.bin.join("gh")).unwrap();
    fs::remove_file(fixture.bin.join("jira")).unwrap();
    let report = report(fixture.doctor().args(["--check", "all"]), 0);
    assert_eq!(report["providers"].as_array().unwrap().len(), 7);
    for kind in [
        ReferenceKind::GitHubIssue,
        ReferenceKind::GitHubPullRequest,
        ReferenceKind::JiraIssue,
    ] {
        let health = provider(&report, kind);
        assert_eq!(health["state"], "disabled");
        assert!(health["checked_at"].is_null());
        assert!(!health["leader"].as_str().unwrap().is_empty());
    }
    fixture.assert_no_probes();
}

#[test]
fn check_without_a_scope_checks_all_external_providers_and_preserves_jira_search_only_status() {
    let fixture = Fixture::new();
    let report = report(fixture.doctor().arg("--check"), 0);
    assert_eq!(
        provider(&report, ReferenceKind::GitHubIssue)["state"],
        "ready"
    );
    assert_eq!(
        provider(&report, ReferenceKind::GitHubPullRequest)["state"],
        "ready"
    );
    assert_eq!(
        provider(&report, ReferenceKind::JiraIssue)["state"],
        "access_verified"
    );
    assert_eq!(fixture.calls("gh").len(), 6);
    assert_eq!(fixture.calls("jira").len(), 1);
    assert!(fixture.calls("git").is_empty());
}
