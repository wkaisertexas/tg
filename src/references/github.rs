use super::model::{
    CandidateDisplay, CandidateId, ContextCost, ExternalUrlTarget, Preview, PreviewLine,
    QueryRequest, QueryScope, ReferenceCandidate, ReferenceKind, ReferenceTarget, ValidatedTarget,
};
use super::process::{ProcessRequest, run};
use super::{CancellationFlag, ReferenceProvider};
use crate::config::GithubProviderConfig;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Debug)]
pub struct GithubProvider {
    executable: PathBuf,
    cwd: PathBuf,
    kind: ReferenceKind,
    leader: String,
    limit: usize,
    timeout: Duration,
    output_limit: usize,
    items_by_url: Mutex<HashMap<String, GithubItem>>,
}

impl GithubProvider {
    pub fn issues(
        cwd: impl Into<PathBuf>,
        leader: impl Into<String>,
        config: &GithubProviderConfig,
    ) -> Self {
        Self::new(cwd, ReferenceKind::GitHubIssue, leader, config)
    }

    pub fn pull_requests(
        cwd: impl Into<PathBuf>,
        leader: impl Into<String>,
        config: &GithubProviderConfig,
    ) -> Self {
        Self::new(cwd, ReferenceKind::GitHubPullRequest, leader, config)
    }

    fn new(
        cwd: impl Into<PathBuf>,
        kind: ReferenceKind,
        leader: impl Into<String>,
        config: &GithubProviderConfig,
    ) -> Self {
        debug_assert!(matches!(
            kind,
            ReferenceKind::GitHubIssue | ReferenceKind::GitHubPullRequest
        ));
        Self {
            executable: config.command.clone(),
            cwd: cwd.into(),
            kind,
            leader: leader.into(),
            limit: config.limit,
            timeout: Duration::from_millis(config.timeout_ms),
            output_limit: DEFAULT_OUTPUT_LIMIT,
            items_by_url: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    fn with_output_limit(mut self, output_limit: usize) -> Self {
        self.output_limit = output_limit;
        self
    }

    fn command_args(&self, query: &str, limit: usize) -> Vec<OsString> {
        let (subject, fields) = match self.kind {
            ReferenceKind::GitHubIssue => ("issue", "number,title,url,state,labels,updatedAt"),
            ReferenceKind::GitHubPullRequest => ("pr", "number,title,url,state,isDraft,updatedAt"),
            _ => unreachable!("constructor restricts GitHub kinds"),
        };
        [
            subject.to_owned(),
            "list".to_owned(),
            "--search".to_owned(),
            query.to_owned(),
            "--limit".to_owned(),
            limit.to_string(),
            "--json".to_owned(),
            fields.to_owned(),
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    fn decode_candidate(&self, id: &CandidateId) -> Result<String> {
        anyhow::ensure!(
            id.provider == self.kind,
            "candidate belongs to another provider"
        );
        validate_url(&id.opaque)?;
        Ok(id.opaque.clone())
    }

    fn target(&self, url: String) -> ReferenceTarget {
        ReferenceTarget::ExternalUrl(ExternalUrlTarget {
            kind: self.kind,
            url,
        })
    }
}

impl ReferenceProvider for GithubProvider {
    fn kind(&self) -> ReferenceKind {
        self.kind
    }

    fn query(
        &self,
        request: QueryRequest,
        cancellation: &CancellationFlag,
    ) -> Result<Vec<ReferenceCandidate>> {
        anyhow::ensure!(
            request.scope == QueryScope::Repository,
            "GitHub provider only supports repository queries"
        );
        if cancellation.is_cancelled() || request.limit == 0 {
            return Ok(Vec::new());
        }
        let limit = request.limit.min(self.limit);
        let output = run(
            ProcessRequest {
                executable: self.executable.clone(),
                args: self.command_args(&request.query, limit),
                cwd: self.cwd.clone(),
                timeout: self.timeout,
                output_limit: self.output_limit,
                env: vec![
                    ("GH_PROMPT_DISABLED".into(), "1".into()),
                    ("NO_COLOR".into(), "1".into()),
                    ("CLICOLOR".into(), "0".into()),
                ],
            },
            cancellation,
        )
        .map_err(anyhow::Error::new)?;
        if cancellation.is_cancelled() {
            return Ok(Vec::new());
        }
        let mut items: Vec<GithubItem> = serde_json::from_slice(&output.stdout)
            .context("gh returned malformed JSON for GitHub search")?;
        anyhow::ensure!(
            items.iter().all(|item| item.number > 0),
            "gh returned an invalid GitHub item number"
        );
        for item in &items {
            validate_url(&item.url)?;
        }

        if let Ok(exact_number) = request.query.trim().parse::<u64>() {
            items.sort_by_key(|item| item.number != exact_number);
        }
        items.truncate(limit);
        let mut cache = self.items_by_url.lock().expect("GitHub item cache lock");
        for item in &items {
            cache.insert(item.url.clone(), item.clone());
        }

        items
            .into_iter()
            .map(|item| {
                Ok(ReferenceCandidate {
                    id: CandidateId {
                        provider: self.kind,
                        opaque: item.url.clone(),
                    },
                    generation: request.generation,
                    kind: self.kind,
                    friendly_text: format!("{}{}", self.leader, item.number),
                    display: CandidateDisplay {
                        primary: format!("#{} {}", item.number, item.title),
                        secondary: Some(item.secondary()),
                        ..CandidateDisplay::default()
                    },
                    context_cost: ContextCost::None,
                    file_context_cost: None,
                    source_version: None,
                    token_source: None,
                })
            })
            .collect()
    }

    fn resolve(&self, id: &CandidateId) -> Result<ReferenceTarget> {
        Ok(self.target(self.decode_candidate(id)?))
    }

    fn validate(&self, target: &ReferenceTarget) -> Result<ValidatedTarget> {
        let ReferenceTarget::ExternalUrl(external) = target else {
            bail!("GitHub provider cannot validate this target")
        };
        anyhow::ensure!(
            external.kind == self.kind,
            "GitHub target has the wrong kind"
        );
        validate_url(&external.url)?;
        Ok(ValidatedTarget {
            target: target.clone(),
            context_cost: ContextCost::None,
        })
    }

    fn lower(&self, target: &ValidatedTarget) -> Result<String> {
        let ReferenceTarget::ExternalUrl(external) = &target.target else {
            bail!("GitHub provider cannot lower this target")
        };
        anyhow::ensure!(
            external.kind == self.kind,
            "GitHub target has the wrong kind"
        );
        validate_url(&external.url)?;
        Ok(external.url.clone())
    }

    fn preview(&self, target: &ReferenceTarget) -> Result<Option<Preview>> {
        let ReferenceTarget::ExternalUrl(external) = target else {
            bail!("GitHub provider cannot preview this target")
        };
        anyhow::ensure!(
            external.kind == self.kind,
            "GitHub target has the wrong kind"
        );
        validate_url(&external.url)?;
        let cache = self.items_by_url.lock().expect("GitHub item cache lock");
        let Some(item) = cache.get(&external.url) else {
            return Ok(Some(Preview {
                title: Some(external.url.clone()),
                lines: Vec::new(),
                highlighted_lines: None,
            }));
        };
        Ok(Some(Preview {
            title: Some(format!("#{} {}", item.number, item.title)),
            lines: vec![
                PreviewLine {
                    number: None,
                    text: item.secondary(),
                },
                PreviewLine {
                    number: None,
                    text: item.url.clone(),
                },
            ],
            highlighted_lines: None,
        }))
    }

    fn context_cost(&self, target: &ReferenceTarget) -> Result<ContextCost> {
        let ReferenceTarget::ExternalUrl(external) = target else {
            bail!("GitHub provider cannot measure this target")
        };
        anyhow::ensure!(
            external.kind == self.kind,
            "GitHub target has the wrong kind"
        );
        Ok(ContextCost::None)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GithubItem {
    number: u64,
    title: String,
    url: String,
    state: String,
    #[serde(default)]
    labels: Vec<GithubLabel>,
    #[serde(default)]
    is_draft: bool,
    updated_at: String,
}

impl GithubItem {
    fn secondary(&self) -> String {
        let mut parts = vec![self.state.clone()];
        if self.is_draft {
            parts.push("draft".to_owned());
        }
        if !self.labels.is_empty() {
            parts.push(
                self.labels
                    .iter()
                    .map(|label| label.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        parts.push(format_update_age(&self.updated_at));
        parts.join(" · ")
    }
}

#[derive(Debug, Clone, Deserialize)]
struct GithubLabel {
    name: String,
}

fn validate_url(url: &str) -> Result<()> {
    anyhow::ensure!(
        (url.starts_with("https://") || url.starts_with("http://"))
            && url.split_once("://").is_some_and(|(_, rest)| {
                !rest.is_empty() && !rest.starts_with('/') && !rest.chars().any(char::is_whitespace)
            }),
        "gh returned an invalid GitHub URL"
    );
    Ok(())
}

fn format_update_age(updated_at: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64);
    let Some(updated) = parse_github_timestamp(updated_at) else {
        return format!("updated {updated_at}");
    };
    let elapsed = now.saturating_sub(updated).max(0) as u64;
    let age = match elapsed {
        0..=59 => "just now".to_owned(),
        60..=3_599 => format!("{}m ago", elapsed / 60),
        3_600..=86_399 => format!("{}h ago", elapsed / 3_600),
        _ => format!("{}d ago", elapsed / 86_400),
    };
    format!("updated {age}")
}

/// Parses the fixed-width RFC 3339 UTC form emitted by `gh` without adding a
/// date-time dependency solely for presentation metadata.
fn parse_github_timestamp(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || !value.ends_with('Z')
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| {
        std::str::from_utf8(&bytes[range]).ok()?.parse::<i64>().ok()
    };
    let year = number(0..4)?;
    let month = number(5..7)?;
    let day = number(8..10)?;
    let hour = number(11..13)?;
    let minute = number(14..16)?;
    let second = number(17..19)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::references::model::GenerationId;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;
    use tempfile::TempDir;

    struct FakeGh {
        root: TempDir,
        executable: PathBuf,
    }

    impl FakeGh {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let executable = root.path().join("fake-gh");
            fs::write(
                &executable,
                r#"#!/bin/sh
printf '%s\n' "$@" > args.txt
printf 'cwd=%s\nHOME=%s\nGH_HOST=%s\nGH_PROMPT_DISABLED=%s\nNO_COLOR=%s\nCLICOLOR=%s\n' \
  "$PWD" "${HOME-unset}" "${GH_HOST-unset}" "${GH_PROMPT_DISABLED-unset}" \
  "${NO_COLOR-unset}" "${CLICOLOR-unset}" > environment.txt
mode=$(cat mode.txt)
case "$mode" in
  success) cat response.json ;;
  failure) cat error.txt >&2; exit 4 ;;
  timeout) exec sleep 5 ;;
esac
"#,
            )
            .unwrap();
            let mut permissions = fs::metadata(&executable).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&executable, permissions).unwrap();
            fs::write(root.path().join("mode.txt"), "success").unwrap();
            Self { root, executable }
        }

        fn write_response(&self, response: impl AsRef<[u8]>) {
            fs::write(self.root.path().join("response.json"), response).unwrap();
        }

        fn provider(&self, kind: ReferenceKind, timeout_ms: u64) -> GithubProvider {
            let config = GithubProviderConfig {
                command: self.executable.clone(),
                limit: 50,
                timeout_ms,
                ..GithubProviderConfig::default()
            };
            match kind {
                ReferenceKind::GitHubIssue => {
                    GithubProvider::issues(self.root.path(), "#", &config)
                }
                ReferenceKind::GitHubPullRequest => {
                    GithubProvider::pull_requests(self.root.path(), "!", &config)
                }
                _ => unreachable!(),
            }
        }

        fn args(&self) -> Vec<String> {
            fs::read_to_string(self.root.path().join("args.txt"))
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn set_mode(&self, mode: &str) {
            fs::write(self.root.path().join("mode.txt"), mode).unwrap();
        }
    }

    fn request(query: &str, limit: usize) -> QueryRequest {
        QueryRequest {
            generation: GenerationId(17),
            query: query.to_owned(),
            scope: QueryScope::Repository,
            limit,
            typed_leader: String::new(),
        }
    }

    #[test]
    fn issue_query_uses_exact_arguments_cwd_inherited_environment_and_enterprise_url() {
        let fake = FakeGh::new();
        fake.write_response(
            r#"[
              {"number":12,"title":"Later","url":"https://git.corp/acme/app/issues/12","state":"OPEN","labels":[],"updatedAt":"2026-08-16T10:00:00Z"},
              {"number":7,"title":"Exact","url":"https://git.corp/acme/app/issues/7","state":"CLOSED","labels":[{"name":"bug"}],"updatedAt":"2026-08-17T10:00:00Z"}
            ]"#,
        );
        let provider = fake.provider(ReferenceKind::GitHubIssue, 1_000);
        let candidates = provider
            .query(request("7", 10), &CancellationFlag::default())
            .unwrap();

        assert_eq!(
            fake.args(),
            [
                "issue",
                "list",
                "--search",
                "7",
                "--limit",
                "10",
                "--json",
                "number,title,url,state,labels,updatedAt",
            ]
        );
        let environment = fs::read_to_string(fake.root.path().join("environment.txt")).unwrap();
        assert!(environment.contains(&format!(
            "cwd={}",
            fake.root.path().canonicalize().unwrap().display()
        )));
        assert!(environment.contains(&format!(
            "HOME={}",
            std::env::var("HOME").unwrap_or_else(|_| "unset".to_owned())
        )));
        assert!(environment.contains("GH_PROMPT_DISABLED=1"));
        assert!(environment.contains("NO_COLOR=1"));
        assert!(environment.contains("CLICOLOR=0"));
        assert_eq!(candidates[0].friendly_text, "#7");
        assert!(
            candidates[0]
                .display
                .secondary
                .as_deref()
                .unwrap()
                .contains("bug")
        );
        let target = provider.resolve(&candidates[0].id).unwrap();
        let validated = provider.validate(&target).unwrap();
        assert_eq!(
            provider.lower(&validated).unwrap(),
            "https://git.corp/acme/app/issues/7"
        );
        assert_eq!(provider.context_cost(&target).unwrap(), ContextCost::None);
        let preview = provider.preview(&target).unwrap().unwrap();
        assert_eq!(preview.title.as_deref(), Some("#7 Exact"));
    }

    #[test]
    fn pull_request_provider_uses_distinct_kind_fields_and_leader() {
        let fake = FakeGh::new();
        fake.write_response(
            r#"[{"number":31,"title":"Ship it","url":"https://github.example/team/repo/pull/31","state":"OPEN","isDraft":true,"updatedAt":"2026-08-17T12:00:00Z"}]"#,
        );
        let provider = fake.provider(ReferenceKind::GitHubPullRequest, 1_000);
        let candidates = provider
            .query(request("ship", 3), &CancellationFlag::default())
            .unwrap();
        assert_eq!(
            fake.args(),
            [
                "pr",
                "list",
                "--search",
                "ship",
                "--limit",
                "3",
                "--json",
                "number,title,url,state,isDraft,updatedAt",
            ]
        );
        assert_eq!(candidates[0].kind, ReferenceKind::GitHubPullRequest);
        assert_eq!(candidates[0].friendly_text, "!31");
        assert!(
            candidates[0]
                .display
                .secondary
                .as_deref()
                .unwrap()
                .contains("draft")
        );
    }

    #[test]
    fn missing_cli_and_malformed_json_have_actionable_diagnostics() {
        let fake = FakeGh::new();
        let config = GithubProviderConfig {
            command: fake.root.path().join("missing-gh"),
            ..GithubProviderConfig::default()
        };
        let missing = GithubProvider::issues(fake.root.path(), "#", &config)
            .query(request("x", 5), &CancellationFlag::default())
            .unwrap_err()
            .to_string();
        assert!(missing.contains("was not found"), "{missing}");

        fake.write_response("not json");
        let malformed = fake
            .provider(ReferenceKind::GitHubIssue, 1_000)
            .query(request("x", 5), &CancellationFlag::default())
            .unwrap_err()
            .to_string();
        assert!(malformed.contains("malformed JSON"), "{malformed}");
    }

    #[test]
    fn command_failure_preserves_auth_remediation_but_redacts_secrets() {
        let fake = FakeGh::new();
        fake.set_mode("failure");
        fs::write(
            fake.root.path().join("error.txt"),
            "authentication required; run gh auth login; token ghp_supersecretvalue123456",
        )
        .unwrap();
        let error = fake
            .provider(ReferenceKind::GitHubIssue, 1_000)
            .query(request("x", 5), &CancellationFlag::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("gh auth login"), "{error}");
        assert!(error.contains("[REDACTED]"), "{error}");
        assert!(!error.contains("supersecret"), "{error}");
    }

    #[test]
    fn oversized_output_is_bounded() {
        let fake = FakeGh::new();
        fake.write_response(vec![b'x'; 4_096]);
        let error = fake
            .provider(ReferenceKind::GitHubIssue, 1_000)
            .with_output_limit(128)
            .query(request("x", 5), &CancellationFlag::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("128-byte safety limit"), "{error}");
    }

    #[test]
    fn timeout_and_cancellation_kill_the_process_promptly() {
        let fake = FakeGh::new();
        fake.set_mode("timeout");
        let started = Instant::now();
        let timeout = fake
            .provider(ReferenceKind::GitHubIssue, 50)
            .query(request("x", 5), &CancellationFlag::default())
            .unwrap_err()
            .to_string();
        assert!(timeout.contains("timed out"), "{timeout}");
        assert!(started.elapsed() < Duration::from_secs(2));

        let cancellation = CancellationFlag::default();
        let trigger = cancellation.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            trigger.cancel();
        });
        let started = Instant::now();
        let cancelled = fake
            .provider(ReferenceKind::GitHubPullRequest, 5_000)
            .query(request("x", 5), &cancellation)
            .unwrap_err()
            .to_string();
        canceller.join().unwrap();
        assert!(cancelled.contains("cancelled"), "{cancelled}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn wrong_kind_and_invalid_urls_are_rejected() {
        let fake = FakeGh::new();
        let provider = fake.provider(ReferenceKind::GitHubIssue, 1_000);
        let wrong_kind = CandidateId {
            provider: ReferenceKind::GitHubPullRequest,
            opaque: "https://example.test/issues/1".to_owned(),
        };
        assert!(provider.resolve(&wrong_kind).is_err());
        let invalid = CandidateId {
            provider: ReferenceKind::GitHubIssue,
            opaque: "javascript:alert(1)".to_owned(),
        };
        assert!(provider.resolve(&invalid).is_err());
    }

    #[test]
    fn github_timestamps_are_rendered_as_compact_ages() {
        let updated = parse_github_timestamp("2026-08-17T10:00:00Z").unwrap();
        assert_eq!(days_from_civil(2026, 8, 17) * 86_400 + 10 * 3_600, updated);
        assert!(parse_github_timestamp("not-a-timestamp").is_none());
    }
}
