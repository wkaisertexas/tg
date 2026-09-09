use super::*;

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

pub(super) fn set_failure(health: &mut ProviderHealth, state: HealthState) {
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
