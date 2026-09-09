use super::*;
use serde::Deserialize;
use serde_json::Value;

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

pub(super) fn inventory(health: &mut ProviderHealth, repo: &Repository, config: &Config) {
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
        set_failure(health, HealthState::NotConfigured);
        health.summary = "GH_HOST or GH_REPO is not valid host/repository metadata".into();
    }
    if metadata.host.is_none() {
        health
            .details
            .push("Target host will be resolved by gh during an explicit check".into());
    }
    external_inventory(
        health,
        &config.providers.github.command,
        Some(&repo.invocation_root),
        config.providers.github.enabled,
    );
}

pub(super) fn check(
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
