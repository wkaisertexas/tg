use super::CancellationFlag;
use super::model::ReferenceKind;
use super::process::{self, ProcessError, ProcessRequest};
use crate::config::Config;
use crate::repository::Repository;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OUTPUT_LIMIT: usize = 1024 * 1024;
const METADATA_LIMIT: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Ready,
    AccessVerified,
    NotChecked,
    Disabled,
    MissingExecutable,
    NotConfigured,
    Checking,
    AuthFailed,
    AccessDenied,
    ConnectionFailed,
    TimedOut,
    Unsupported,
    Failed,
}

impl HealthState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::AccessVerified => "Search works",
            Self::NotChecked => "Not checked",
            Self::Disabled => "Disabled",
            Self::MissingExecutable => "Missing executable",
            Self::NotConfigured => "Not configured",
            Self::Checking => "Checking",
            Self::AuthFailed => "Authentication failed",
            Self::AccessDenied => "Access denied",
            Self::ConnectionFailed => "Connection failed",
            Self::TimedOut => "Timed out",
            Self::Unsupported => "Unsupported",
            Self::Failed => "Failed",
        }
    }

    pub fn is_problem(self) -> bool {
        !matches!(
            self,
            Self::Ready | Self::AccessVerified | Self::NotChecked | Self::Disabled | Self::Checking
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealth {
    pub kind: ReferenceKind,
    pub leader: String,
    pub name: String,
    pub example: String,
    pub state: HealthState,
    pub summary: String,
    pub target: Option<String>,
    pub details: Vec<String>,
    pub actions: Vec<String>,
    pub checked_at: Option<u64>,
}

pub fn inventory(repo: &Repository, config: &Config) -> Vec<ProviderHealth> {
    [
        ReferenceKind::GitFile,
        ReferenceKind::BroadFile,
        ReferenceKind::Symbol,
        ReferenceKind::Skill,
        ReferenceKind::GitHubIssue,
        ReferenceKind::GitHubPullRequest,
        ReferenceKind::JiraIssue,
    ]
    .into_iter()
    .map(|kind| local_health(repo, config, kind))
    .collect()
}

pub fn check(
    repo: &Repository,
    config: &Config,
    kind: ReferenceKind,
    cancellation: &CancellationFlag,
) -> ProviderHealth {
    let mut health = local_health(repo, config, kind);
    if cancellation.is_cancelled() && health.state != HealthState::Disabled {
        set_failure(&mut health, HealthState::NotChecked);
    } else if !matches!(
        health.state,
        HealthState::Disabled | HealthState::MissingExecutable
    ) {
        match kind {
            ReferenceKind::GitHubIssue | ReferenceKind::GitHubPullRequest => {
                check_github(&mut health, repo, config, cancellation)
            }
            ReferenceKind::JiraIssue => check_jira(&mut health, config, cancellation),
            _ => {}
        }
    }
    if cancellation.is_cancelled() && health.state != HealthState::Disabled {
        health
            .details
            .retain(|detail| !detail.starts_with("Check: "));
        set_failure(&mut health, HealthState::NotChecked);
    }
    health.checked_at = Some(now());
    health
}

pub fn record_failure(health: &mut ProviderHealth, message: &str) {
    health
        .details
        .retain(|detail| !detail.starts_with("Check: "));
    set_failure(health, classify_failure(message));
    health.checked_at = Some(now());
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn local_health(repo: &Repository, config: &Config, kind: ReferenceKind) -> ProviderHealth {
    let (leader, name, suffix) = match kind {
        ReferenceKind::GitFile => (&config.leaders.files, "Files", "src/main.rs"),
        ReferenceKind::BroadFile => (&config.leaders.broad_files, "Broad files", "notes.txt"),
        ReferenceKind::Symbol => (&config.leaders.symbols, "Symbols", "main"),
        ReferenceKind::Skill => (&config.leaders.skills, "Skills", "review"),
        ReferenceKind::GitHubIssue => (&config.leaders.github_issues, "GitHub issues", "123"),
        ReferenceKind::GitHubPullRequest => (
            &config.leaders.github_pull_requests,
            "GitHub pull requests",
            "123",
        ),
        ReferenceKind::JiraIssue => (&config.leaders.jira_issues, "Jira issues", "PROJ-123"),
    };
    let mut health = ProviderHealth {
        kind,
        leader: leader.clone(),
        name: name.into(),
        example: format!("{leader}{suffix}"),
        state: HealthState::NotChecked,
        summary: "Authentication and access have not been checked".into(),
        target: None,
        details: Vec::new(),
        actions: Vec::new(),
        checked_at: None,
    };
    match kind {
        ReferenceKind::GitFile
        | ReferenceKind::BroadFile
        | ReferenceKind::Symbol
        | ReferenceKind::Skill => {
            health.target = Some(display_path(&repo.search_root));
            health
                .details
                .push(format!("Search root: {}", display_path(&repo.search_root)));
            if !repo.search_root.is_dir() {
                set_failure(&mut health, HealthState::NotConfigured);
                health.summary = "The local search root is unavailable".into();
                health.actions = vec!["Open tg with an existing, readable root directory".into()];
            } else if kind == ReferenceKind::Skill {
                health.state = HealthState::Ready;
                health.summary =
                    "Local discovery; matches depend on installed skills, not service credentials"
                        .into();
                health.actions.push("Add a SKILL.md under .agents/skills/NAME, or configure skills.roots in tg user settings".into());
                health
                    .details
                    .push("No service authentication or external executable is required".into());
                for root in &config.skills.roots {
                    health.details.push(format!(
                        "Configured skill root: {}",
                        display_path(&repo.search_root.join(&root.path))
                    ));
                }
            } else {
                health.state = HealthState::Ready;
                health.summary = match kind {
                    ReferenceKind::GitFile if repo.git_aware => "Local Git-aware file discovery",
                    ReferenceKind::GitFile => "Local file discovery; no Git worktree was detected",
                    ReferenceKind::BroadFile => "Local file discovery including ignored files, subject to configured excludes",
                    _ => "Built-in symbol indexing; individual language support depends on the file",
                }.into();
            }
        }
        ReferenceKind::GitHubIssue | ReferenceKind::GitHubPullRequest => {
            let metadata = github_metadata();
            health.target = metadata.target();
            health
                .details
                .push("Host/repository metadata is not proof of authentication".into());
            if let Some(host) = &metadata.host {
                health
                    .details
                    .push(format!("Configured host (unverified): {host}"));
            }
            if let Some(repository) = &metadata.repository {
                health.details.push(format!("GH_REPO: {repository}"));
            }
            if metadata.invalid {
                set_failure(&mut health, HealthState::NotConfigured);
                health.summary = "GH_HOST or GH_REPO is not valid host/repository metadata".into();
            }
            if metadata.host.is_none() {
                health
                    .details
                    .push("Target host will be resolved by gh during an explicit check".into());
            }
            external_inventory(
                &mut health,
                &config.providers.github.command,
                Some(&repo.invocation_root),
                config.providers.github.enabled,
            );
        }
        ReferenceKind::JiraIssue => {
            if let Some(prefix) = &config.providers.jira.key_prefix {
                health.example = format!("{leader}{prefix}-123");
                health.details.push(format!(
                    "Numeric issue key prefix: {prefix} (does not select the Jira search project)"
                ));
            }
            let cwd = std::env::current_dir().ok();
            let metadata = jira_metadata(cwd.as_deref());
            if let Some(path) = &metadata.path {
                health
                    .details
                    .push(format!("Active Jira config path: {}", display_path(path)));
            }
            health.target = metadata.server.clone();
            if let Some(server) = &metadata.server {
                health
                    .details
                    .push(format!("Configured server (unverified): {server}"));
            }
            if let Some(project) = &metadata.project {
                health
                    .details
                    .push(format!("Configured project: {project}"));
            }
            if let Some(auth_type) = &metadata.auth_type {
                health.details.push(format!(
                    "Authentication mode: {auth_type}; credentials remain owned by jira"
                ));
            }
            if let Some((state, summary)) = metadata.problem {
                set_failure(&mut health, state);
                health.summary = summary.into();
            }
            health.details.push("Metadata reads only the selected config; wrapper-supplied overrides cannot be inferred".into());
            external_inventory(
                &mut health,
                &config.providers.jira.command,
                cwd.as_deref(),
                config.providers.jira.enabled,
            );
        }
    }
    health
}

fn external_inventory(
    health: &mut ProviderHealth,
    command: &Path,
    cwd: Option<&Path>,
    enabled: bool,
) {
    health
        .details
        .push(format!("Configured executable: {}", display_path(command)));
    if let Some(cwd) = cwd {
        health
            .details
            .push(format!("Working directory: {}", display_path(cwd)));
        if let Some(resolved) = resolve_executable(command, cwd) {
            health
                .details
                .push(format!("Resolved executable: {}", display_path(&resolved)));
        } else if enabled {
            set_failure(health, HealthState::MissingExecutable);
        }
    } else if enabled {
        set_failure(health, HealthState::Failed);
        health.summary = "The provider working directory is unavailable".into();
    }
    if !enabled {
        health.state = HealthState::Disabled;
        health.summary = "Disabled by the effective tg configuration".into();
        let provider = if health.kind == ReferenceKind::JiraIssue {
            "jira"
        } else {
            "github"
        };
        health.actions = vec![format!(
            "Set providers.{provider}.enabled = true in your tg configuration"
        )];
    } else if health.state == HealthState::NotChecked {
        health
            .actions
            .push("Run an explicit provider check to validate connectivity and access".into());
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .filter(|c| !c.is_control())
        .take(1024)
        .collect()
}

fn resolve_executable(command: &Path, cwd: &Path) -> Option<PathBuf> {
    if command.as_os_str().is_empty() {
        return None;
    }
    if command.is_absolute()
        || command.components().count() > 1
        || command.as_os_str().as_encoded_bytes().contains(&b'/')
    {
        let path = cwd.join(command);
        return executable_file(&path).then_some(path);
    }
    let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin"));
    std::env::split_paths(&path)
        .map(|directory| cwd.join(directory).join(command))
        .find(|candidate| executable_file(candidate))
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[derive(Default)]
struct GithubMetadata {
    host: Option<String>,
    repository: Option<String>,
    invalid: bool,
}

impl GithubMetadata {
    fn target(&self) -> Option<String> {
        match (&self.host, &self.repository) {
            (Some(host), Some(repository)) => Some(format!("{host}/{repository}")),
            (Some(host), None) => Some(host.clone()),
            (None, repository) => repository.clone(),
        }
    }
}

fn metadata_env(name: &str) -> Option<String> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
}

fn github_metadata() -> GithubMetadata {
    let host = metadata_env("GH_HOST");
    let mut metadata = GithubMetadata {
        invalid: host
            .as_deref()
            .is_some_and(|value| valid_host(value).is_none()),
        host: host.as_deref().and_then(valid_host),
        repository: None,
    };
    if let Some(repository) = metadata_env("GH_REPO") {
        if let Some((host, name)) = parse_repository(&repository) {
            metadata.host = host.or(metadata.host);
            metadata.repository = Some(name);
        } else {
            metadata.invalid = true;
        }
    }
    metadata
}

fn valid_host(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > 253
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return None;
    }
    if value
        .split('.')
        .any(|part| part.is_empty() || part.starts_with('-') || part.ends_with('-'))
    {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn parse_repository(value: &str) -> Option<(Option<String>, String)> {
    if value.contains("://") {
        let url = safe_base_url(value)?;
        let host = valid_host(url.host_str()?)?;
        let path = url.path().trim_matches('/');
        let (_, name) = parse_repository(path)?;
        if path.split('/').count() != 2 || url.port().is_some() {
            return None;
        }
        return Some((Some(host), name));
    }
    let parts: Vec<_> = value.split('/').collect();
    let (host, owner, name) = match parts.as_slice() {
        [owner, name] => (None, *owner, *name),
        [host, owner, name] => (Some(valid_host(host)?), *owner, *name),
        _ => return None,
    };
    let name = name.strip_suffix(".git").unwrap_or(name);
    (safe_name(owner) && safe_name(name)).then(|| (host, format!("{owner}/{name}")))
}

fn safe_base_url(value: &str) -> Option<Url> {
    if value.len() > 2048
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
        || value.contains('\\')
    {
        return None;
    }
    let (_, rest) = value.split_once("://")?;
    let authority = rest.split('/').next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let url = Url::parse(value).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none())
    .then_some(url)
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct JiraConfigMetadata {
    server: Option<String>,
    project: JiraProjectMetadata,
    auth_type: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct JiraProjectMetadata {
    key: Option<String>,
}

#[derive(Default)]
struct JiraMetadata {
    path: Option<PathBuf>,
    server: Option<String>,
    project: Option<String>,
    auth_type: Option<String>,
    problem: Option<(HealthState, &'static str)>,
}

fn jira_config_path(cwd: &Path) -> Option<(PathBuf, bool)> {
    if let Some(path) = metadata_env("JIRA_CONFIG_FILE") {
        return Some((cwd.join(path), true));
    }
    let home = metadata_env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| metadata_env("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    let directory = cwd.join(home).join(".jira");
    for extension in [
        "json",
        "toml",
        "yaml",
        "yml",
        "properties",
        "props",
        "prop",
        "hcl",
        "tfvars",
        "dotenv",
        "env",
        "ini",
        "",
    ] {
        let filename = if extension.is_empty() {
            ".config".into()
        } else {
            format!(".config.{extension}")
        };
        let path = directory.join(filename);
        if path.metadata().is_ok_and(|metadata| !metadata.is_dir()) {
            return Some((path, false));
        }
    }
    Some((directory.join(".config.yml"), false))
}

fn jira_metadata(cwd: Option<&Path>) -> JiraMetadata {
    let mut metadata = JiraMetadata::default();
    let mut parsed = JiraConfigMetadata::default();
    match cwd.and_then(jira_config_path) {
        Some((path, explicit)) => {
            metadata.path = Some(path.clone());
            match read_jira_metadata(&path, explicit) {
                Ok(config) => parsed = config,
                Err(problem) => metadata.problem = Some(problem),
            }
        }
        None => {
            metadata.problem = Some((
                HealthState::NotConfigured,
                "The active Jira config path could not be determined",
            ))
        }
    }
    let server = metadata_env("JIRA_SERVER").or(parsed.server);
    metadata.server = server
        .as_deref()
        .and_then(safe_base_url)
        .map(|url| url.as_str().trim_end_matches('/').to_owned());
    let project = metadata_env("JIRA_PROJECT.KEY").or(parsed.project.key);
    metadata.project = project.filter(|key| safe_name(key));
    metadata.auth_type = metadata_env("JIRA_AUTH_TYPE")
        .or(parsed.auth_type)
        .filter(|mode| matches!(mode.as_str(), "basic" | "bearer" | "mtls" | "cf-access"));
    if metadata.problem.is_none() && metadata.server.is_none() {
        metadata.problem = Some((
            HealthState::NotConfigured,
            "Jira server metadata is missing or is not a safe HTTP(S) base URL",
        ));
    }
    if metadata.problem.is_none() && metadata.project.is_none() {
        metadata.problem = Some((
            HealthState::NotConfigured,
            "No valid Jira project key is configured",
        ));
    }
    metadata
}

fn read_jira_metadata(
    path: &Path,
    explicit: bool,
) -> Result<JiraConfigMetadata, (HealthState, &'static str)> {
    let missing = (
        HealthState::NotConfigured,
        "The active Jira config file is missing or unreadable",
    );
    let metadata = path.metadata().map_err(|_| missing)?;
    if !metadata.is_file() || metadata.len() > METADATA_LIMIT {
        return Err((
            HealthState::Failed,
            "The active Jira config is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| missing)?
        .take(METADATA_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| missing)?;
    if bytes.len() as u64 > METADATA_LIMIT {
        return Err((
            HealthState::Failed,
            "The active Jira config exceeds the metadata size limit",
        ));
    }
    let malformed = (
        HealthState::NotConfigured,
        "The active Jira config metadata could not be parsed safely",
    );
    if !explicit {
        return serde_yaml::from_slice(&bytes).map_err(|_| malformed);
    }
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("yml" | "yaml") => serde_yaml::from_slice(&bytes).map_err(|_| malformed),
        Some("json") => serde_json::from_slice(&bytes).map_err(|_| malformed),
        Some("toml") => toml::from_str(std::str::from_utf8(&bytes).map_err(|_| malformed)?)
            .map_err(|_| malformed),
        _ => Err((
            HealthState::Unsupported,
            "The active Jira config format is not supported by the metadata reader",
        )),
    }
}

struct Probe<'a> {
    executable: &'a Path,
    cwd: &'a Path,
    deadline: Instant,
    github: bool,
    cancellation: &'a CancellationFlag,
}

impl Probe<'_> {
    fn run(&self, arguments: &[&str]) -> Result<Vec<u8>, ProcessError> {
        if self.cancellation.is_cancelled() {
            return Err(ProcessError::Cancelled);
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ProcessError::TimedOut(Duration::ZERO));
        }
        process::run(
            ProcessRequest {
                executable: self.executable.to_path_buf(),
                args: arguments
                    .iter()
                    .map(|argument| OsString::from(*argument))
                    .collect(),
                cwd: self.cwd.to_path_buf(),
                timeout: remaining,
                output_limit: OUTPUT_LIMIT,
                env: if self.github {
                    vec![
                        ("GH_PROMPT_DISABLED".into(), "1".into()),
                        ("NO_COLOR".into(), "1".into()),
                        ("CLICOLOR".into(), "0".into()),
                    ]
                } else {
                    Vec::new()
                },
            },
            self.cancellation,
        )
        .map(|output| output.stdout)
    }
}

fn probe_failure(health: &mut ProviderHealth, error: ProcessError) {
    let state = match error {
        ProcessError::MissingExecutable(_) => HealthState::MissingExecutable,
        ProcessError::TimedOut(_) => HealthState::TimedOut,
        ProcessError::Cancelled => HealthState::NotChecked,
        ProcessError::Failed { stderr, .. } => classify_failure(&stderr),
        _ => HealthState::Failed,
    };
    set_failure(health, state);
}

fn check_github(
    health: &mut ProviderHealth,
    repo: &Repository,
    config: &Config,
    cancellation: &CancellationFlag,
) {
    let metadata = github_metadata();
    if metadata.invalid {
        return;
    }
    let probe = Probe {
        executable: &config.providers.github.command,
        cwd: &repo.invocation_root,
        deadline: Instant::now()
            + Duration::from_millis(config.providers.github.timeout_ms.clamp(1, 30_000)),
        github: true,
        cancellation,
    };
    let repository_argument = metadata.repository.as_ref().map(|repository| {
        metadata
            .host
            .as_ref()
            .map_or_else(|| repository.clone(), |host| format!("{host}/{repository}"))
    });
    let mut view_args = vec!["repo", "view", "--json", "nameWithOwner,url"];
    if let Some(repository) = repository_argument.as_deref() {
        view_args.extend(["--", repository]);
    }
    let mut host = metadata.host.clone();
    if host.is_none() || metadata.repository.is_none() {
        let bytes = match probe.run(&view_args) {
            Ok(bytes) => bytes,
            Err(error) => {
                probe_failure(health, error);
                health.details.push("Check: gh could not resolve the effective repository; no other host was probed".into());
                return;
            }
        };
        let Some((resolved_host, name, url)) = github_repository(&bytes) else {
            malformed_output(health);
            return;
        };
        health.target = Some(url);
        health.details.push(format!(
            "Check: gh resolved repository {name} on {resolved_host}; identity is not yet validated"
        ));
        host = Some(resolved_host);
    }
    let Some(host) = host else { return };
    let identity = match probe.run(&["api", "user", "--hostname", &host]) {
        Ok(bytes) => bytes,
        Err(error) => {
            probe_failure(health, error);
            return;
        }
    };
    let authenticated = serde_json::from_slice::<Value>(&identity)
        .ok()
        .is_some_and(|value| {
            value
                .get("login")
                .and_then(Value::as_str)
                .is_some_and(|login| !login.is_empty())
                && value
                    .get("id")
                    .and_then(Value::as_u64)
                    .is_some_and(|id| id > 0)
        });
    if !authenticated {
        malformed_output(health);
        return;
    }
    health.details.push(format!(
        "Check: authenticated identity validated by gh api user on {host}"
    ));
    let bytes = match probe.run(&view_args) {
        Ok(bytes) => bytes,
        Err(error) => {
            probe_failure(health, error);
            return;
        }
    };
    let Some((resolved_host, name, url)) = github_repository(&bytes) else {
        malformed_output(health);
        return;
    };
    if resolved_host != host {
        set_failure(health, HealthState::Failed);
        health.summary =
            "The resolved GitHub host changed during the check; retry before trusting the result"
                .into();
        return;
    }
    health.target = Some(url);
    health
        .details
        .push(format!("Check: repository access validated for {name}"));
    let subject = if health.kind == ReferenceKind::GitHubIssue {
        "issue"
    } else {
        "pr"
    };
    let bytes = match probe.run(&[
        subject,
        "list",
        "--search",
        "",
        "--limit",
        "1",
        "--json",
        "number,url",
    ]) {
        Ok(bytes) => bytes,
        Err(error) => {
            probe_failure(health, error);
            return;
        }
    };
    let valid = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .is_some_and(|value| {
            value.as_array().is_some_and(|items| {
                items.len() <= 1
                    && items.iter().all(|item| {
                        item.get("number")
                            .and_then(Value::as_u64)
                            .is_some_and(|number| number > 0)
                            && item
                                .get("url")
                                .and_then(Value::as_str)
                                .and_then(safe_base_url)
                                .is_some_and(|url| {
                                    url.host_str() == Some(host.as_str())
                                        && url.path().starts_with(&format!("/{name}/"))
                                })
                    })
            })
        });
    if !valid {
        malformed_output(health);
        return;
    }
    health.state = HealthState::Ready;
    health.summary = "Authentication, repository access, and provider listing validated".into();
    health.actions.clear();
}

fn github_repository(bytes: &[u8]) -> Option<(String, String, String)> {
    #[derive(Deserialize)]
    struct RepositoryResponse {
        #[serde(rename = "nameWithOwner")]
        name: String,
        url: String,
    }
    let value: RepositoryResponse = serde_json::from_slice(bytes).ok()?;
    let (host, name) = parse_repository(&value.url)?;
    if name != value.name {
        return None;
    }
    Some((host?, name, safe_base_url(&value.url)?.to_string()))
}

fn check_jira(health: &mut ProviderHealth, config: &Config, cancellation: &CancellationFlag) {
    if health.state.is_problem() {
        return;
    }
    let Ok(cwd) = std::env::current_dir() else {
        set_failure(health, HealthState::Failed);
        return;
    };
    let probe = Probe {
        executable: &config.providers.jira.command,
        cwd: &cwd,
        deadline: Instant::now()
            + Duration::from_millis(config.providers.jira.timeout_ms.clamp(1, 30_000)),
        github: false,
        cancellation,
    };
    health.details.push("Check: authentication is not independently verified; jira me only prints the locally configured login".into());
    let bytes = match probe.run(&["issue", "list", "--raw", "--paginate", "1"]) {
        Ok(bytes) => bytes,
        Err(ProcessError::Failed { stderr, .. })
            if classify_failure(&stderr) == HealthState::Failed
                && stderr
                    .to_ascii_lowercase()
                    .contains("no result found for given query in project") =>
        {
            jira_access_succeeded(health);
            health.summary = "Jira search completed with no visible issues; authentication and project permissions are not independently verified".into();
            return;
        }
        Err(error) => {
            probe_failure(health, error);
            return;
        }
    };
    let valid = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .is_some_and(|value| {
            let issues = value
                .as_array()
                .or_else(|| value.get("issues").and_then(Value::as_array));
            issues.is_some_and(|issues| {
                issues.len() <= 1
                    && issues.iter().all(|issue| {
                        issue
                            .get("key")
                            .and_then(Value::as_str)
                            .is_some_and(safe_name)
                            && issue.get("fields").is_some_and(Value::is_object)
                    })
            })
        });
    if !valid {
        malformed_output(health);
        return;
    }
    health.details.push("Check: a bounded issue list succeeded; public or empty results are not authentication proof".into());
    jira_access_succeeded(health);
}

fn jira_access_succeeded(health: &mut ProviderHealth) {
    health.state = HealthState::AccessVerified;
    health.summary =
        "Jira search access works. Authentication is not independently verified by this CLI".into();
    health.actions = vec!["Continue using Jira references; use a known restricted issue to confirm the account has the access you need".into(), "A public or empty search does not prove an API token is valid; tg does not read or manage credentials".into()];
}

fn malformed_output(health: &mut ProviderHealth) {
    set_failure(health, HealthState::Failed);
    health.summary =
        "The provider returned malformed or unexpected output; no service body was retained".into();
    health.actions = vec!["Verify the configured executable is the supported gh or ankitpokhrel/jira-cli version; update it and retry".into()];
}

pub(crate) fn classify_failure(message: &str) -> HealthState {
    let lower = message.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
    let status = |code: &str| {
        [
            "http ",
            "http/1.1 ",
            "http/2 ",
            "http error ",
            "status ",
            "status: ",
            "status code: ",
            "status code ",
        ]
        .iter()
        .any(|prefix| lower.contains(&format!("{prefix}{code}")))
    };
    if lower.starts_with("jira authentication failed") {
        HealthState::AuthFailed
    } else if has(&["cancelled", "canceled"]) {
        HealthState::NotChecked
    } else if has(&["timed out", "timeout", "deadline exceeded"]) {
        HealthState::TimedOut
    } else if has(&[
        "unknown command",
        "unknown flag",
        "unknown shorthand",
        "unsupported",
        "not implemented",
        "unrecognized option",
    ]) {
        HealthState::Unsupported
    } else if has(&[
        "malformed",
        "invalid json",
        "invalid character",
        "unexpected token",
        "json parse",
        "syntax error",
        "output exceeded",
    ]) {
        HealthState::Failed
    } else if has(&[
        "x509",
        "tls",
        "certificate",
        "no such host",
        "dns",
        "dial tcp",
        "connection refused",
        "connection reset",
        "network is unreachable",
        "network unreachable",
        "could not resolve host",
        "failed to connect",
        "error connecting",
        "proxyconnect",
        "connection failed",
    ]) {
        HealthState::ConnectionFailed
    } else if status("403")
        || has(&[
            "forbidden",
            "access denied",
            "permission denied",
            "insufficient scope",
            "resource not accessible",
            "saml",
            "sso",
            "rate limit",
        ])
    {
        HealthState::AccessDenied
    } else if status("401")
        || has(&[
            "unauthorized",
            "unauthorised",
            "bad credentials",
            "authentication failed",
            "authentication required",
            "authentication error",
            "requires authentication",
            "not authenticated",
            "not logged",
            "gh auth login",
            "invalid token",
            "token is invalid",
            "expired token",
            "token expired",
            "token has expired",
            "needs a jira api token",
            "missing api token",
            "api token is required",
            "jira_api_token is not set",
            "authorization:",
        ])
    {
        HealthState::AuthFailed
    } else if has(&[
        "missing configuration",
        "config file",
        "configuration file",
        "not configured",
        "jira init",
        "not a git repository",
        "no git remotes",
        "no default remote",
        "none of the git remotes",
        "could not determine repository",
        "repository not configured",
    ]) {
        HealthState::NotConfigured
    } else if has(&[
        "jira unavailable",
        "no such file or directory",
        "command not found",
    ]) || (has(&["executable", "cannot run"])
        && has(&["not found", "does not exist", "cannot be found"]))
    {
        HealthState::MissingExecutable
    } else if status("404")
        || has(&[
            "not found",
            "does not exist",
            "cannot be found",
            "could not resolve to a repository",
        ])
    {
        HealthState::AccessDenied
    } else {
        HealthState::Failed
    }
}

fn set_failure(health: &mut ProviderHealth, state: HealthState) {
    health.state = state;
    health.summary = match state {
        HealthState::MissingExecutable => "The configured provider executable is missing or is not executable",
        HealthState::NotConfigured => "The provider's server, repository, or project configuration is incomplete",
        HealthState::AuthFailed => "Provider authentication failed or credentials are unavailable",
        HealthState::AccessDenied => "Provider access was denied; permissions, SSO, rate limits, or target availability may be responsible",
        HealthState::ConnectionFailed => "The provider could not connect; check DNS, TLS trust, VPN, and proxy settings",
        HealthState::TimedOut => "The read-only provider check timed out",
        HealthState::Unsupported => "The configured CLI cannot perform the requested diagnostic operation",
        HealthState::NotChecked => "The provider check was cancelled; authentication and access are not validated",
        _ => "The provider command failed; raw output was withheld for safety",
    }.into();
    if matches!(
        health.kind,
        ReferenceKind::GitFile
            | ReferenceKind::BroadFile
            | ReferenceKind::Symbol
            | ReferenceKind::Skill
    ) {
        health.actions = vec!["Check the local search root, file permissions, and the relevant tg discovery settings; retry the reference query".into()];
        return;
    }
    let jira = health.kind == ReferenceKind::JiraIssue;
    health.actions = match state {
        HealthState::MissingExecutable => vec![if jira {
            "Install ankitpokhrel/jira-cli and set providers.jira.command to its executable path"
        } else {
            "Install GitHub CLI (gh) and set providers.github.command to its executable path"
        }.into()],
        HealthState::NotConfigured | HealthState::AuthFailed if jira => vec![
            "Run `jira init` to configure the server and project using the same JIRA_CONFIG_FILE selection".into(),
            "For on-premises PAT authentication use JIRA_AUTH_TYPE=bearer; for basic auth use the server-required login/password (Cloud uses an API token); jira owns credential storage".into(),
            "For mTLS, select Local and mtls in `jira init`, and configure trusted CA and client certificates; do not disable TLS verification".into(),
            "After updating environment credentials, restart tg from that environment and retry".into(),
        ],
        HealthState::NotConfigured | HealthState::AuthFailed => {
            let host = health.target.as_deref().and_then(|target| {
                if target.contains("://") { safe_base_url(target).and_then(|url| url.host_str().and_then(valid_host)) }
                else if matches!(target.split('/').count(), 1 | 3) { target.split('/').next().and_then(valid_host) }
                else { None }
            });
            let login = host.map_or_else(
                || "Resolve the intended GitHub host, then run `gh auth login --hostname HOST` for that host only".into(),
                |host| format!("Run `gh auth login --hostname {host}` for the intended repository host"),
            );
            vec![login, "Check the repository's gh default remote and GH_HOST/GH_REPO selection; use HOST/OWNER/REPO to make Enterprise targets explicit".into(), "Restart tg after updating environment credentials, then retry".into()]
        }
        HealthState::AccessDenied => vec![if jira {
            "Confirm the configured project exists and the jira account has Browse Projects permission; verify server policy with an administrator"
        } else {
            "Confirm repository access, issue/PR read permissions, organization SSO authorization, and rate limits for the selected host"
        }.into()],
        HealthState::ConnectionFailed => vec!["Check the configured host, VPN, DNS, proxy, and trusted CA/client certificates; do not disable TLS verification".into()],
        HealthState::TimedOut => vec!["Check connectivity and retry; adjust the provider timeout_ms if the server is slow (diagnostics are capped at 30 seconds)".into()],
        HealthState::Unsupported if jira => vec!["The documented `jira me` command prints configured login locally; it does not call an authenticated identity endpoint".into(), "Validate your account and Browse Projects access with your Jira administrator; public issue listings alone cannot establish authentication".into(), "Use a supported ankitpokhrel/jira-cli release for `jira issue list --raw --paginate 1`".into()],
        HealthState::Unsupported => vec!["Update GitHub CLI to a version supporting `gh api user`, `gh repo view --json`, and issue/PR list JSON output".into()],
        HealthState::NotChecked => vec!["Retry the provider check when ready".into()],
        _ => vec!["Verify the configured provider executable and configuration, then retry; avoid debug output when sharing diagnostics".into()],
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_numbers_are_not_http_status_codes() {
        assert_eq!(
            classify_failure("Jira issue OPS-401 not found"),
            HealthState::AccessDenied
        );
        assert_eq!(classify_failure("HTTP 401"), HealthState::AuthFailed);
        assert_eq!(
            classify_failure("status code: 403"),
            HealthState::AccessDenied
        );
        assert_eq!(classify_failure("HTTP/2 401"), HealthState::AuthFailed);
    }
}
