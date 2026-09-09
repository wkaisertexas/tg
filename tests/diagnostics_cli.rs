#![cfg(unix)]

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;
use tscodeselection::references::model::ReferenceKind;

const JIRA_SERVER: &str = "https://jira.corp.example/jira";
const GH_HOST: &str = "ghe.corp.example";
const GH_REPOSITORY: &str = "ghe.corp.example/team/project";
const JIRA_SECRET: &str = "fixture-jira-secret-DO-NOT-USE";
const GH_SECRET: &str = "ghp_fixture_only_DO_NOT_USE_123456789";
const CONFIG_SECRET: &str = "fixture-config-password-DO-NOT-DISPLAY";
const BODY_SECRET: &str = "fixture-service-body-DO-NOT-DISPLAY";

const KINDS: [ReferenceKind; 7] = [
    ReferenceKind::GitFile,
    ReferenceKind::BroadFile,
    ReferenceKind::Symbol,
    ReferenceKind::Skill,
    ReferenceKind::GitHubIssue,
    ReferenceKind::GitHubPullRequest,
    ReferenceKind::JiraIssue,
];

const FAKE_JIRA: &str = r##"#!/bin/sh
printf '%s\t' "$PWD" "$@" >> "$0.calls"
printf '\n' >> "$0.calls"
if [ "$*" != 'issue list --raw --paginate 1' ]; then
    printf '%s\n' 'unknown command: unexpected diagnostic operation' >&2
    exit 90
fi
IFS= read -r mode < "$0.mode"
case "$mode" in
    auth)
        printf '%s\n' 'HTTP 401 Unauthorized: invalid token' 'Authorization: Bearer fixture-jira-secret-DO-NOT-USE' 'fixture-service-body-DO-NOT-DISPLAY' >&2
        exit 1 ;;
    denied)
        printf '%s\n' 'HTTP 403 Forbidden: token lacks project permission' 'fixture-jira-secret-DO-NOT-USE' >&2
        exit 1 ;;
    tls)
        printf '%s\n' 'x509: certificate signed by unknown authority' 'fixture-jira-secret-DO-NOT-USE' >&2
        exit 1 ;;
    dns)
        printf '%s\n' 'dial tcp: lookup jira.corp.example: no such host' 'fixture-service-body-DO-NOT-DISPLAY' >&2
        exit 1 ;;
    timeout)
        exec /bin/sleep 3 ;;
    malformed)
        printf '%s\n' '{"issues":[fixture-service-body-DO-NOT-DISPLAY'
        exit 0 ;;
    unsupported)
        printf '%s\n' 'unknown flag: --raw' 'fixture-jira-secret-DO-NOT-USE' >&2
        exit 1 ;;
    empty)
        printf '%s\n' '[]' ;;
    envelope)
        printf '%s\n' '{"issues":[]}' ;;
    no-results)
        printf '%s\n' 'No result found for given query in project "OPS"' >&2
        exit 1 ;;
    ready)
        printf '%s\n' '[{"key":"OPS-1","self":"https://jira.corp.example/jira/rest/api/2/issue/1","fields":{"summary":"fixture-service-body-DO-NOT-DISPLAY"}}]' ;;
    *)
        printf '%s\n' 'unknown command: fixture mode' >&2
        exit 90 ;;
esac
"##;

const FAKE_GH: &str = r##"#!/bin/sh
printf '%s\t' "$PWD" "$@" >> "$0.calls"
printf '\n' >> "$0.calls"
if [ "$GH_PROMPT_DISABLED" != 1 ] || [ "$NO_COLOR" != 1 ] || [ "$CLICOLOR" != 0 ]; then
    printf '%s\n' 'diagnostic process settings differ from provider queries' >&2
    exit 90
fi
IFS= read -r mode < "$0.mode"
case "$1 $2" in
    'api user')
        if [ "$#" -ne 4 ] || [ "$3" != '--hostname' ] || [ "$4" != 'ghe.corp.example' ]; then
            printf '%s\n' 'wrong identity host or unexpected operation' >&2
            exit 90
        fi
        case "$mode" in
            anonymous) printf '%s\n' '{"login":null,"id":0}' ;;
            auth)
                printf '%s\n' 'HTTP 401: Bad credentials' 'Authorization: Bearer ghp_fixture_only_DO_NOT_USE_123456789' 'fixture-service-body-DO-NOT-DISPLAY' >&2
                exit 1 ;;
            *) printf '%s\n' '{"login":"fixture-user","id":42}' ;;
        esac ;;
    'repo view')
        if [ "$mode" = repo-denied ]; then
            printf '%s\n' 'HTTP 403: repository access forbidden' 'ghp_fixture_only_DO_NOT_USE_123456789' >&2
            exit 1
        fi
        printf '%s\n' '{"nameWithOwner":"team/project","url":"https://ghe.corp.example/team/project"}' ;;
    'issue list'|'pr list')
        if [ "$3" != '--search' ] || [ "$4" != '' ] || [ "$5" != '--limit' ] || [ "$6" != 1 ] || [ "$7" != '--json' ] || [ "$8" != 'number,url' ]; then
            printf '%s\n' 'unexpected diagnostic list flags' >&2
            exit 90
        fi
        printf '%s\n' '[]' ;;
    *)
        printf '%s\n' 'unknown command: unexpected diagnostic operation' >&2
        exit 90 ;;
esac
"##;

struct Fixture {
    _temp: TempDir,
    repo: PathBuf,
    cwd: PathBuf,
    home: PathBuf,
    xdg: PathBuf,
    bin: PathBuf,
    config: PathBuf,
    jira_config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let cwd = base.join("working directory");
        let home = base.join("home");
        let xdg = base.join("xdg");
        let bin = base.join("bin space");
        for path in [
            repo.join(".git"),
            cwd.clone(),
            home.join(".config"),
            xdg.join("tg"),
            xdg.join(".jira"),
            xdg.join("gh"),
            xdg.join("codex"),
            bin.clone(),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        let fixture = Self {
            config: xdg.join("tg/config.toml"),
            jira_config: xdg.join(".jira/.config.yml"),
            _temp: temp,
            repo,
            cwd,
            home,
            xdg,
            bin,
        };
        executable(&fixture.bin.join("jira"), FAKE_JIRA);
        executable(&fixture.bin.join("gh"), FAKE_GH);
        executable(
            &fixture.bin.join("git"),
            "#!/bin/sh\nprintf '%s\\n' unexpected-git-call >> \"$0.calls\"\nexit 90\n",
        );
        fixture.mode("jira", "ready");
        fixture.mode("gh", "ready");
        fixture.write_config(true, true, 1_500, "");
        fixture.write_jira_config(JIRA_SERVER);
        fixture
    }

    fn config_text(&self, github: bool, jira: bool, timeout_ms: u64, extra: &str) -> String {
        format!(
            "[providers.github]\nenabled={github}\ncommand={}\ntimeout_ms={timeout_ms}\n\
             [providers.jira]\nenabled={jira}\ncommand={}\nkey_prefix='OPS'\ntimeout_ms={timeout_ms}\n{extra}",
            quoted_path(&self.bin.join("gh")),
            quoted_path(&self.bin.join("jira")),
        )
    }

    fn write_config(&self, github: bool, jira: bool, timeout_ms: u64, extra: &str) {
        fs::write(
            &self.config,
            self.config_text(github, jira, timeout_ms, extra),
        )
        .unwrap();
    }

    fn write_jira_config(&self, server: &str) {
        fs::write(&self.jira_config, jira_config_text(server)).unwrap();
    }

    fn mode(&self, provider: &str, mode: &str) {
        fs::write(
            self.bin.join(format!("{provider}.mode")),
            format!("{mode}\n"),
        )
        .unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tg"));
        command
            .env_clear()
            .current_dir(&self.cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg)
            .env("GH_CONFIG_DIR", self.xdg.join("gh"))
            .env("CODEX_HOME", self.xdg.join("codex"))
            .env("PATH", &self.bin)
            .env("GH_HOST", GH_HOST)
            .env("GH_REPO", GH_REPOSITORY)
            .env("GH_TOKEN", GH_SECRET)
            .env("JIRA_API_TOKEN", JIRA_SECRET)
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .timeout(Duration::from_secs(8));
        command
    }

    fn doctor(&self) -> Command {
        let mut command = self.command();
        command.args(["doctor", "--json", "--root"]).arg(&self.repo);
        command
    }

    fn calls(&self, provider: &str) -> Vec<Vec<String>> {
        fs::read_to_string(self.bin.join(format!("{provider}.calls")))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                line.trim_end_matches('\t')
                    .split('\t')
                    .map(str::to_owned)
                    .collect()
            })
            .collect()
    }

    fn assert_no_probes(&self) {
        for provider in ["gh", "jira", "git"] {
            assert!(
                self.calls(provider).is_empty(),
                "unexpected {provider} probe"
            );
        }
    }
}

fn executable(path: &Path, source: &str) {
    fs::write(path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn quoted_path(path: &Path) -> String {
    serde_json::to_string(path.to_str().unwrap()).unwrap()
}

fn jira_config_text(server: &str) -> String {
    format!(
        "server: {}\nproject:\n  key: OPS\nauth_type: bearer\n\
         login: fixture-private-login@example.invalid\npassword: {CONFIG_SECRET}\napi_token: {JIRA_SECRET}\n",
        serde_json::to_string(server).unwrap(),
    )
}

fn assert_safe(output: &Output) {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    for secret in [
        JIRA_SECRET,
        GH_SECRET,
        CONFIG_SECRET,
        BODY_SECRET,
        "fixture-private-login@example.invalid",
    ] {
        assert!(
            !text.contains(secret),
            "diagnostics leaked fixture-only private data"
        );
    }
}

fn output(command: &mut Command, code: i32) -> Output {
    let output = command.assert().code(code).get_output().clone();
    assert_safe(&output);
    output
}

fn report(command: &mut Command, code: i32) -> Value {
    let output = output(command, code);
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn provider(report: &Value, kind: ReferenceKind) -> &Value {
    let kind = serde_json::to_value(kind).unwrap();
    report["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["kind"] == kind)
        .unwrap()
}

fn lines(value: &Value) -> String {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn inventory_lists_every_custom_leader_without_any_provider_process() {
    let fixture = Fixture::new();
    fixture.write_config(
        true,
        true,
        1_500,
        "[leaders]\nfiles='@@'\nbroad_files='%%'\nsymbols=':::'\nskills='$$'\ngithub_issues='##'\ngithub_pull_requests='!!'\njira_issues='&&'\n",
    );
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
            "1".into(),
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

#[test]
fn github_enterprise_identity_repository_and_empty_lists_succeed_on_the_same_host() {
    let fixture = Fixture::new();
    let report = report(
        fixture
            .doctor()
            .env("GH_HOST", "github.com")
            .args(["--check", "github"]),
        0,
    );
    for kind in [ReferenceKind::GitHubIssue, ReferenceKind::GitHubPullRequest] {
        let github = provider(&report, kind);
        assert_eq!(github["state"], "ready");
        assert_eq!(github["target"], "https://ghe.corp.example/team/project");
        assert!(github["checked_at"].is_u64());
        assert!(lines(&github["details"]).contains("authenticated identity validated"));
        assert!(github["actions"].as_array().unwrap().is_empty());
    }
    let calls = fixture.calls("gh");
    assert_eq!(calls.len(), 6);
    for (sequence, subject) in calls.chunks(3).zip(["issue", "pr"]) {
        assert!(
            sequence
                .iter()
                .all(|call| call[0] == fixture.repo.to_str().unwrap())
        );
        assert_eq!(sequence[0][1..], ["api", "user", "--hostname", GH_HOST]);
        assert_eq!(
            sequence[1][1..],
            [
                "repo",
                "view",
                "--json",
                "nameWithOwner,url",
                "--",
                GH_REPOSITORY
            ]
        );
        assert_eq!(
            sequence[2][1..],
            [
                subject,
                "list",
                "--search",
                "",
                "--limit",
                "1",
                "--json",
                "number,url"
            ]
        );
    }
    assert!(fixture.calls("jira").is_empty());
    assert!(fixture.calls("git").is_empty());
}

#[test]
fn github_cannot_use_a_public_empty_listing_as_authentication_proof() {
    for (mode, state) in [("anonymous", "failed"), ("auth", "auth_failed")] {
        let fixture = Fixture::new();
        fixture.mode("gh", mode);
        let report = report(fixture.doctor().args(["--check", "github"]), 1);
        for kind in [ReferenceKind::GitHubIssue, ReferenceKind::GitHubPullRequest] {
            assert_eq!(provider(&report, kind)["state"], state);
            assert_ne!(provider(&report, kind)["state"], "ready");
        }
        let calls = fixture.calls("gh");
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|call| call[1..] == ["api", "user", "--hostname", GH_HOST])
        );
        assert!(fixture.calls("jira").is_empty());
    }
}

#[test]
fn github_repository_permissions_are_checked_after_identity_validation() {
    let fixture = Fixture::new();
    fixture.mode("gh", "repo-denied");
    let report = report(fixture.doctor().args(["--check", "github"]), 1);
    for kind in [ReferenceKind::GitHubIssue, ReferenceKind::GitHubPullRequest] {
        let github = provider(&report, kind);
        assert_eq!(github["state"], "access_denied");
        assert!(lines(&github["details"]).contains("authenticated identity validated"));
    }
    let calls = fixture.calls("gh");
    assert_eq!(calls.len(), 4);
    assert_eq!(
        calls
            .iter()
            .map(|call| call[1].as_str())
            .collect::<Vec<_>>(),
        ["api", "repo", "api", "repo"]
    );
    assert!(fixture.calls("jira").is_empty());
}

#[test]
fn github_remote_discovery_determines_the_enterprise_identity_host_without_account_sweeps() {
    let fixture = Fixture::new();
    let report = report(
        fixture
            .doctor()
            .env_remove("GH_HOST")
            .env_remove("GH_REPO")
            .args(["--check", "github"]),
        0,
    );
    assert_eq!(
        provider(&report, ReferenceKind::GitHubIssue)["state"],
        "ready"
    );
    assert_eq!(
        provider(&report, ReferenceKind::GitHubPullRequest)["state"],
        "ready"
    );
    let calls = fixture.calls("gh");
    assert_eq!(calls.len(), 8);
    for sequence in calls.chunks(4) {
        assert_eq!(
            sequence[0][1..],
            ["repo", "view", "--json", "nameWithOwner,url"]
        );
        assert_eq!(sequence[1][1..], ["api", "user", "--hostname", GH_HOST]);
        assert_eq!(sequence[2][1], "repo");
    }
    assert!(calls.iter().all(|call| {
        !call
            .iter()
            .any(|argument| argument == "auth" || argument == "token")
    }));
    assert!(fixture.calls("jira").is_empty());
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
