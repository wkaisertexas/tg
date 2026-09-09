use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() || *character == '\n')
        .collect()
}

pub(super) fn now_seconds() -> u64 {
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
