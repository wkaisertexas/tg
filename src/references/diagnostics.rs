mod failure;
mod github;
mod jira;

use super::CancellationFlag;
use super::model::ReferenceKind;
use super::process::{self, ProcessError, ProcessRequest};
use crate::config::Config;
use crate::repository::Repository;
pub(crate) use failure::classify_failure;
use failure::set_failure;
use reqwest::Url;
use serde::Serialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OUTPUT_LIMIT: usize = 1024 * 1024;

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
    ReferenceKind::ALL
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
                github::check(&mut health, repo, config, cancellation)
            }
            ReferenceKind::JiraIssue => jira::check(&mut health, config, cancellation),
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
            github::inventory(&mut health, repo, config)
        }
        ReferenceKind::JiraIssue => jira::inventory(&mut health, config),
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

fn metadata_env(name: &str) -> Option<String> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
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

fn malformed_output(health: &mut ProviderHealth) {
    set_failure(health, HealthState::Failed);
    health.summary =
        "The provider returned malformed or unexpected output; no service body was retained".into();
    health.actions = vec!["Verify the configured executable is the supported gh or ankitpokhrel/jira-cli version; update it and retry".into()];
}
