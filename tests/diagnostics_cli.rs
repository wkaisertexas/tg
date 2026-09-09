#![cfg(unix)]

mod diagnostics_cli {
    mod github;
    mod jira;
    mod local;
    use super::*;
}

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
            quoted_path(&self.bin.join("jira"))
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
        serde_json::to_string(server).unwrap()
    )
}
fn assert_safe(output: &Output) {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
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
