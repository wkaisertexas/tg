use super::*;
use serde::Deserialize;
use serde_json::Value;
use std::fs::File;
use std::io::Read;

const METADATA_LIMIT: u64 = 256 * 1024;

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

pub(super) fn inventory(health: &mut ProviderHealth, config: &Config) {
    if let Some(prefix) = &config.providers.jira.key_prefix {
        health.example = format!("{}{prefix}-123", health.leader);
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
        set_failure(health, state);
        health.summary = summary.into();
    }
    health.details.push(
        "Metadata reads only the selected config; wrapper-supplied overrides cannot be inferred"
            .into(),
    );
    external_inventory(
        health,
        &config.providers.jira.command,
        cwd.as_deref(),
        config.providers.jira.enabled,
    );
}

pub(super) fn check(health: &mut ProviderHealth, config: &Config, cancellation: &CancellationFlag) {
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
