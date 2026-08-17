//! Jira issue completion through the configured `jira` executable.

use super::model::{
    CandidateDisplay, CandidateId, ContextCost, ExternalUrlTarget, Preview, PreviewLine,
    QueryRequest, QueryScope, ReferenceCandidate, ReferenceKind, ReferenceTarget, ValidatedTarget,
};
use super::{CancellationFlag, ReferenceProvider};
use crate::config::JiraProviderConfig;
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_OUTPUT_CAP: usize = 1024 * 1024;
const PREVIEW_BODY_LIMIT: usize = 2_000;

#[derive(Debug, Clone)]
struct JiraIssue {
    key: String,
    summary: String,
    status: String,
    assignee: String,
    updated: String,
    body: String,
    url: String,
}

#[derive(Debug)]
pub struct JiraProvider {
    command: PathBuf,
    key_prefix: Option<String>,
    leader: String,
    limit: usize,
    timeout: Duration,
    output_cap: usize,
    issues: Mutex<HashMap<String, JiraIssue>>,
}

impl JiraProvider {
    pub fn new(config: &JiraProviderConfig, leader: impl Into<String>) -> Result<Self> {
        ensure!(
            !config.command.as_os_str().is_empty(),
            "Jira command is empty"
        );
        ensure!(config.limit > 0, "Jira result limit must be positive");
        ensure!(config.timeout_ms > 0, "Jira timeout must be positive");
        Ok(Self {
            command: config.command.clone(),
            key_prefix: config.key_prefix.clone(),
            leader: leader.into(),
            limit: config.limit,
            timeout: Duration::from_millis(config.timeout_ms),
            output_cap: DEFAULT_OUTPUT_CAP,
            issues: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    fn with_output_cap(mut self, output_cap: usize) -> Self {
        self.output_cap = output_cap;
        self
    }

    fn normalized_query(&self, query: &str) -> QueryKind {
        let query = query.trim();
        if !query.is_empty()
            && query.bytes().all(|byte| byte.is_ascii_digit())
            && let Some(prefix) = &self.key_prefix
        {
            return QueryKind::Key(format!("{prefix}-{query}"));
        }
        if is_issue_key(query) {
            QueryKind::Key(query.to_owned())
        } else {
            QueryKind::Search(query.to_owned())
        }
    }

    fn execute_query(
        &self,
        query: &QueryKind,
        cancellation: &CancellationFlag,
    ) -> Result<Vec<JiraIssue>> {
        let arguments = match query {
            QueryKind::Key(key) => vec!["issue".into(), "view".into(), key.clone(), "--raw".into()],
            QueryKind::Search(text) => vec![
                "issue".into(),
                "list".into(),
                "--raw".into(),
                "--jql".into(),
                search_jql(text),
            ],
        };
        let output = match run_bounded(
            &self.command,
            &arguments,
            self.timeout,
            self.output_cap,
            cancellation,
        ) {
            Ok(output) => output,
            Err(_) if cancellation.is_cancelled() => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        if cancellation.is_cancelled() {
            return Ok(Vec::new());
        }
        if !output.status.success() {
            bail!(normalize_cli_error(&output.stderr));
        }
        let value: Value =
            serde_json::from_slice(&output.stdout).context("Jira returned malformed raw JSON")?;
        parse_issues(&value)
    }

    fn remember(&self, issue: JiraIssue) -> Result<()> {
        self.issues
            .lock()
            .map_err(|_| anyhow::anyhow!("Jira issue cache is unavailable"))?
            .insert(issue.key.clone(), issue);
        Ok(())
    }
}

impl ReferenceProvider for JiraProvider {
    fn kind(&self) -> ReferenceKind {
        ReferenceKind::JiraIssue
    }

    fn query(
        &self,
        request: QueryRequest,
        cancellation: &CancellationFlag,
    ) -> Result<Vec<ReferenceCandidate>> {
        ensure!(
            request.scope == QueryScope::Repository,
            "Jira provider only supports repository queries"
        );
        if cancellation.is_cancelled() {
            return Ok(Vec::new());
        }
        let normalized = self.normalized_query(&request.query);
        let mut issues = self.execute_query(&normalized, cancellation)?;
        if cancellation.is_cancelled() {
            return Ok(Vec::new());
        }
        rank_issues(&mut issues, normalized.search_text());
        let limit = request.limit.min(self.limit);
        issues
            .into_iter()
            .take(limit)
            .map(|issue| {
                self.remember(issue.clone())?;
                Ok(ReferenceCandidate {
                    id: CandidateId {
                        provider: ReferenceKind::JiraIssue,
                        opaque: issue.key.clone(),
                    },
                    generation: request.generation,
                    kind: ReferenceKind::JiraIssue,
                    friendly_text: format!("{}{}", self.leader, issue.key),
                    display: CandidateDisplay {
                        primary: issue.key.clone(),
                        secondary: Some(issue_metadata(&issue)),
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
        ensure!(
            id.provider == ReferenceKind::JiraIssue,
            "candidate belongs to another provider"
        );
        let issues = self
            .issues
            .lock()
            .map_err(|_| anyhow::anyhow!("Jira issue cache is unavailable"))?;
        let issue = issues
            .get(&id.opaque)
            .context("Jira candidate is no longer available")?;
        Ok(ReferenceTarget::ExternalUrl(ExternalUrlTarget {
            kind: ReferenceKind::JiraIssue,
            url: issue.url.clone(),
        }))
    }

    fn validate(&self, target: &ReferenceTarget) -> Result<ValidatedTarget> {
        let ReferenceTarget::ExternalUrl(url) = target else {
            bail!("Jira provider requires a URL target")
        };
        ensure!(
            url.kind == ReferenceKind::JiraIssue,
            "URL target is not a Jira issue"
        );
        Ok(ValidatedTarget {
            target: target.clone(),
            context_cost: ContextCost::None,
        })
    }

    fn lower(&self, target: &ValidatedTarget) -> Result<String> {
        let ReferenceTarget::ExternalUrl(url) = &target.target else {
            bail!("Jira provider requires a URL target")
        };
        ensure!(
            url.kind == ReferenceKind::JiraIssue,
            "URL target is not a Jira issue"
        );
        Ok(url.url.clone())
    }

    fn preview(&self, target: &ReferenceTarget) -> Result<Option<Preview>> {
        let ReferenceTarget::ExternalUrl(url) = target else {
            bail!("Jira provider requires a URL target")
        };
        ensure!(
            url.kind == ReferenceKind::JiraIssue,
            "URL target is not a Jira issue"
        );
        let issues = self
            .issues
            .lock()
            .map_err(|_| anyhow::anyhow!("Jira issue cache is unavailable"))?;
        let Some(issue) = issues.values().find(|issue| issue.url == url.url) else {
            return Ok(None);
        };
        let mut lines = vec![
            PreviewLine {
                number: None,
                text: issue.summary.clone(),
            },
            PreviewLine {
                number: None,
                text: issue_metadata(issue),
            },
        ];
        lines.extend(issue.body.lines().map(|line| PreviewLine {
            number: None,
            text: line.to_owned(),
        }));
        Ok(Some(Preview {
            title: Some(issue.key.clone()),
            lines,
            highlighted_lines: None,
        }))
    }

    fn context_cost(&self, _target: &ReferenceTarget) -> Result<ContextCost> {
        Ok(ContextCost::None)
    }
}

#[derive(Debug)]
enum QueryKind {
    Key(String),
    Search(String),
}

impl QueryKind {
    fn search_text(&self) -> &str {
        match self {
            Self::Key(key) | Self::Search(key) => key,
        }
    }
}

fn is_issue_key(query: &str) -> bool {
    let Some((project, number)) = query.rsplit_once('-') else {
        return false;
    };
    let mut project_chars = project.chars();
    project_chars
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        && project_chars.all(|character| character.is_ascii_alphanumeric())
        && !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
}

fn search_jql(text: &str) -> String {
    if text.is_empty() {
        "ORDER BY updated DESC".to_owned()
    } else {
        let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
        format!("text ~ \"{escaped}\" ORDER BY updated DESC")
    }
}

fn parse_issues(value: &Value) -> Result<Vec<JiraIssue>> {
    let fallback_base = discover_base_url(value);
    let values: Vec<&Value> = if let Some(issues) = value.get("issues").and_then(Value::as_array) {
        issues.iter().collect()
    } else if let Some(issues) = value.get("data").and_then(Value::as_array) {
        issues.iter().collect()
    } else if let Some(values) = value.as_array() {
        values.iter().collect()
    } else if let Some(issue) = value.get("issue") {
        vec![issue]
    } else if value.get("key").is_some() {
        vec![value]
    } else {
        bail!("Jira raw JSON did not contain issues")
    };
    values
        .into_iter()
        .map(|issue| parse_issue(issue, fallback_base.as_deref()))
        .collect()
}

fn parse_issue(value: &Value, fallback_base: Option<&str>) -> Result<JiraIssue> {
    let fields = value.get("fields").unwrap_or(value);
    let key = string_at(value, &["key", "issueKey"])
        .or_else(|| string_at(fields, &["key", "issueKey"]))
        .context("Jira issue is missing its key")?;
    let summary = string_at(fields, &["summary", "title"]).unwrap_or_default();
    let status = nested_string(fields.get("status"), &["name", "value"]);
    let assignee = nested_string(
        fields.get("assignee"),
        &["displayName", "name", "emailAddress"],
    );
    let updated = string_at(fields, &["updated", "updatedAt"]).unwrap_or_default();
    let body = plain_text(fields.get("description"))
        .chars()
        .take(PREVIEW_BODY_LIMIT)
        .collect();
    let base = discover_base_url(value)
        .or_else(|| fallback_base.map(str::to_owned))
        .context("Jira raw JSON did not expose a base URL")?;
    Ok(JiraIssue {
        url: format!("{}/browse/{key}", base.trim_end_matches('/')),
        key,
        summary,
        status,
        assignee,
        updated,
        body,
    })
}

fn string_at(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
}

fn nested_string(value: Option<&Value>, keys: &[&str]) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(value) => string_at(value, keys).unwrap_or_default(),
        None => String::new(),
    }
}

fn plain_text(value: Option<&Value>) -> String {
    fn collect(value: &Value, out: &mut String) {
        match value {
            Value::String(text) => out.push_str(text),
            Value::Array(values) => values.iter().for_each(|value| collect(value, out)),
            Value::Object(values) => {
                if let Some(text) = values.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                } else if let Some(content) = values.get("content") {
                    collect(content, out);
                }
                if matches!(
                    values.get("type").and_then(Value::as_str),
                    Some("paragraph" | "heading")
                ) && !out.ends_with('\n')
                {
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
    let mut output = String::new();
    if let Some(value) = value {
        collect(value, &mut output);
    }
    output.trim().to_owned()
}

fn discover_base_url(value: &Value) -> Option<String> {
    for key in ["baseUrl", "base_url", "server", "site"] {
        if let Some(url) = value.get(key).and_then(Value::as_str) {
            return Some(url.trim_end_matches('/').to_owned());
        }
    }
    for container in ["config", "serverInfo", "_links"] {
        if let Some(nested) = value.get(container) {
            for key in ["baseUrl", "base", "server", "site"] {
                if let Some(url) = nested.get(key).and_then(Value::as_str) {
                    return Some(url.trim_end_matches('/').to_owned());
                }
            }
        }
    }
    let self_url = value
        .get("self")
        .and_then(Value::as_str)
        .or_else(|| value.get("url").and_then(Value::as_str))?;
    for marker in ["/rest/api/", "/rest/agile/"] {
        if let Some(index) = self_url.find(marker) {
            return Some(self_url[..index].trim_end_matches('/').to_owned());
        }
    }
    self_url
        .find("/browse/")
        .map(|index| self_url[..index].trim_end_matches('/').to_owned())
}

fn issue_metadata(issue: &JiraIssue) -> String {
    [
        issue.summary.as_str(),
        issue.status.as_str(),
        issue.assignee.as_str(),
        issue.updated.as_str(),
    ]
    .into_iter()
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>()
    .join(" · ")
}

fn rank_issues(issues: &mut [JiraIssue], query: &str) {
    let query = query.to_ascii_lowercase();
    issues.sort_by(|left, right| {
        issue_score(right, &query)
            .cmp(&issue_score(left, &query))
            .then_with(|| right.updated.cmp(&left.updated))
            .then_with(|| left.key.cmp(&right.key))
    });
}

fn issue_score(issue: &JiraIssue, query: &str) -> u8 {
    let key = issue.key.to_ascii_lowercase();
    let summary = issue.summary.to_ascii_lowercase();
    if key == query {
        6
    } else if key.starts_with(query) {
        5
    } else if summary == query {
        4
    } else if summary.starts_with(query) {
        3
    } else if key.contains(query) {
        2
    } else if summary.contains(query) {
        1
    } else {
        0
    }
}

struct ProcessOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_bounded(
    executable: &Path,
    arguments: &[String],
    timeout: Duration,
    cap: usize,
    cancellation: &CancellationFlag,
) -> Result<ProcessOutput> {
    if cancellation.is_cancelled() {
        bail!("Jira request was cancelled")
    }
    let mut child = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Jira unavailable: cannot run {}", executable.display()))?;
    let stdout = child.stdout.take().context("cannot capture Jira output")?;
    let stderr = child.stderr.take().context("cannot capture Jira errors")?;
    let stdout_reader = thread::spawn(move || read_capped(stdout, cap));
    let stderr_reader = thread::spawn(move || read_capped(stderr, cap));
    let started = Instant::now();
    let status = loop {
        if cancellation.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            bail!("Jira request was cancelled")
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            bail!("Jira request timed out")
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let (stdout, stdout_exceeded) = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("Jira output reader failed"))??;
    let (stderr, stderr_exceeded) = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("Jira error reader failed"))??;
    ensure!(
        !stdout_exceeded && !stderr_exceeded,
        "Jira output exceeded the configured limit"
    );
    Ok(ProcessOutput {
        status,
        stdout,
        stderr,
    })
}

fn read_capped(mut reader: impl Read, cap: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut stored = Vec::with_capacity(cap.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut exceeded = false;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = cap.saturating_sub(stored.len());
        stored.extend_from_slice(&buffer[..count.min(remaining)]);
        exceeded |= count > remaining;
    }
    Ok((stored, exceeded))
}

fn normalize_cli_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lower = text.to_ascii_lowercase();
    if [
        "authorization",
        "bearer",
        "token",
        "api_token",
        "api token",
        "password",
        "unauthorized",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return "Jira authentication failed; run `jira init`".to_owned();
    }
    let concise: String = text
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .take(512)
        .collect();
    if concise.trim().is_empty() {
        "Jira command failed".to_owned()
    } else {
        format!("Jira command failed: {}", concise.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::model::GenerationId;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn request(query: &str) -> QueryRequest {
        QueryRequest {
            generation: GenerationId(7),
            query: query.into(),
            scope: QueryScope::Repository,
            limit: 20,
            typed_leader: "&".into(),
        }
    }

    fn fake_jira(timeout_ms: u64) -> (tempfile::TempDir, JiraProvider) {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("jira");
        let script = r##"#!/bin/sh
printf '%s\n' "$@" > "$0.args"
case "$*" in
  *SLOW*) exec sleep 2 ;;
  *MALFORMED*) printf 'not json'; exit 0 ;;
  *AUTH*) printf 'Authorization: Bearer super-secret-token\n' >&2; exit 1 ;;
  *OVERSIZED*) i=0; while [ "$i" -lt 100 ]; do printf 'xxxxxxxxxx'; i=$((i+1)); done; exit 0 ;;
  *'issue view'*)
    key=$3
    printf '{"key":"%s","self":"https://cloud.example/rest/api/3/issue/%s","fields":{"summary":"Exact issue","status":{"name":"Open"},"assignee":{"displayName":"Ada"},"updated":"2026-01-02","description":{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"Cloud body"}]}]}}}' "$key" "$key"
    ;;
  *)
    printf '%s' '{"issues":[{"key":"OPS-20","self":"https://jira.local/context/rest/api/2/issue/OPS-20","fields":{"summary":"fix parser","status":{"name":"Doing"},"assignee":{"name":"sam"},"updated":"2026-02-02","description":"On-prem body"}},{"key":"OPS-2","self":"https://jira.local/context/rest/api/2/issue/OPS-2","fields":{"summary":"Parser","status":"Open","updated":"2026-02-01"}}]}'
    ;;
esac
"##;
        fs::write(&executable, script).unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).unwrap();
        let config = JiraProviderConfig {
            command: executable,
            key_prefix: Some("G5".into()),
            timeout_ms,
            ..JiraProviderConfig::default()
        };
        (temp, JiraProvider::new(&config, "&").unwrap())
    }

    #[test]
    fn numeric_and_explicit_keys_use_direct_raw_lookup_and_lower_cloud_urls() {
        let (temp, provider) = fake_jira(500);
        let candidates = provider
            .query(request("42"), &CancellationFlag::default())
            .unwrap();
        assert_eq!(candidates[0].friendly_text, "&G5-42");
        assert_eq!(
            fs::read_to_string(temp.path().join("jira.args")).unwrap(),
            "issue\nview\nG5-42\n--raw\n"
        );
        let target = provider.resolve(&candidates[0].id).unwrap();
        let validated = provider.validate(&target).unwrap();
        assert_eq!(
            provider.lower(&validated).unwrap(),
            "https://cloud.example/browse/G5-42"
        );
        let explicit = provider
            .query(request("Other-7"), &CancellationFlag::default())
            .unwrap();
        assert_eq!(explicit[0].friendly_text, "&Other-7");
    }

    #[test]
    fn text_and_empty_queries_use_distinct_jql_arguments_and_rank_exactly() {
        let (temp, provider) = fake_jira(500);
        let candidates = provider
            .query(request("Parser"), &CancellationFlag::default())
            .unwrap();
        assert_eq!(candidates[0].display.primary, "OPS-2");
        assert_eq!(candidates[1].display.primary, "OPS-20");
        assert!(
            candidates[1]
                .display
                .secondary
                .as_deref()
                .unwrap()
                .contains("sam")
        );
        let arguments = fs::read_to_string(temp.path().join("jira.args")).unwrap();
        assert!(arguments.contains("issue\nlist\n--raw\n--jql\n"));
        assert!(arguments.contains("text ~ \"Parser\" ORDER BY updated DESC"));
        provider
            .query(request(""), &CancellationFlag::default())
            .unwrap();
        let arguments = fs::read_to_string(temp.path().join("jira.args")).unwrap();
        assert!(arguments.ends_with("--jql\nORDER BY updated DESC\n"));
    }

    #[test]
    fn on_prem_urls_metadata_and_preview_are_supported() {
        let (_temp, provider) = fake_jira(500);
        let candidate = provider
            .query(request("fix"), &CancellationFlag::default())
            .unwrap()
            .remove(0);
        let target = provider.resolve(&candidate.id).unwrap();
        let ReferenceTarget::ExternalUrl(url) = &target else {
            panic!()
        };
        assert_eq!(url.url, "https://jira.local/context/browse/OPS-20");
        let preview = provider.preview(&target).unwrap().unwrap();
        assert_eq!(preview.title.as_deref(), Some("OPS-20"));
        assert!(preview.lines.iter().any(|line| line.text == "On-prem body"));
        assert_eq!(provider.context_cost(&target).unwrap(), ContextCost::None);
    }

    #[test]
    fn missing_timeout_cancellation_malformed_oversized_and_auth_are_safe() {
        let config = JiraProviderConfig {
            command: PathBuf::from("/definitely/missing/tg-jira"),
            ..JiraProviderConfig::default()
        };
        let missing = JiraProvider::new(&config, "&").unwrap();
        assert!(
            missing
                .query(request("x"), &CancellationFlag::default())
                .unwrap_err()
                .to_string()
                .contains("Jira unavailable")
        );

        let (_timeout_temp, timeout) = fake_jira(20);
        assert!(
            timeout
                .query(request("SLOW"), &CancellationFlag::default())
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        let cancelled = CancellationFlag::default();
        cancelled.cancel();
        assert!(
            timeout
                .query(request("anything"), &cancelled)
                .unwrap()
                .is_empty()
        );
        let (_cancel_temp, cancellable) = fake_jira(500);
        let active_cancel = CancellationFlag::default();
        let cancel_from_thread = active_cancel.clone();
        let cancel_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancel_from_thread.cancel();
        });
        assert!(
            cancellable
                .query(request("SLOW"), &active_cancel)
                .unwrap()
                .is_empty()
        );
        cancel_thread.join().unwrap();

        let (_malformed_temp, malformed) = fake_jira(500);
        assert!(
            malformed
                .query(request("MALFORMED"), &CancellationFlag::default())
                .unwrap_err()
                .to_string()
                .contains("malformed raw JSON")
        );
        assert!(
            malformed
                .query(request("AUTH"), &CancellationFlag::default())
                .unwrap_err()
                .to_string()
                .contains("jira init")
        );
        assert!(
            !malformed
                .query(request("AUTH"), &CancellationFlag::default())
                .unwrap_err()
                .to_string()
                .contains("super-secret-token")
        );

        let (_oversized_temp, oversized) = fake_jira(500);
        let oversized = oversized.with_output_cap(32);
        assert!(
            oversized
                .query(request("OVERSIZED"), &CancellationFlag::default())
                .unwrap_err()
                .to_string()
                .contains("exceeded")
        );
    }

    #[test]
    fn parses_wrapped_server_metadata_and_escapes_untrusted_jql_text() {
        let wrapped: Value = serde_json::json!({
            "serverInfo": { "baseUrl": "https://jira.example/context/" },
            "issues": [{
                "key": "SAFE-1",
                "fields": { "summary": "Safe" }
            }]
        });
        let issues = parse_issues(&wrapped).unwrap();
        assert_eq!(issues[0].url, "https://jira.example/context/browse/SAFE-1");
        assert_eq!(
            search_jql("quote \" and \\ slash"),
            "text ~ \"quote \\\" and \\\\ slash\" ORDER BY updated DESC"
        );
    }
}
