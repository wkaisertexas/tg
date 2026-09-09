use super::file::FileProvider;
use super::github::GithubProvider;
use super::jira::JiraProvider;
use super::model::{ReferenceKind, SharedProvider};
use super::skill::SkillProvider;
use super::symbol::SymbolProvider;
use crate::config::Config;
use crate::repository::Repository;
use anyhow::Result;
use std::sync::Arc;

pub(crate) fn configured(
    repo: &Repository,
    config: &Config,
    kinds: impl IntoIterator<Item = ReferenceKind>,
) -> Result<Vec<SharedProvider>> {
    let mut providers = Vec::new();
    for kind in kinds {
        let provider: SharedProvider = match kind {
            ReferenceKind::GitFile | ReferenceKind::BroadFile => {
                let leader = if kind == ReferenceKind::GitFile {
                    &config.leaders.files
                } else {
                    &config.leaders.broad_files
                };
                Arc::new(FileProvider::with_broad_excludes(
                    &repo.search_root,
                    kind,
                    leader,
                    &config.search.broad_excludes,
                )?)
            }
            ReferenceKind::Symbol => Arc::new(SymbolProvider::with_broad_excludes(
                &repo.search_root,
                &config.leaders.files,
                &config.search.broad_excludes,
            )?),
            ReferenceKind::Skill => Arc::new(SkillProvider::new(
                &repo.search_root,
                &repo.invocation_root,
                &config.skills,
                &config.leaders.skills,
            )?),
            ReferenceKind::GitHubIssue if config.providers.github.enabled => {
                Arc::new(GithubProvider::issues(
                    &repo.invocation_root,
                    &config.leaders.github_issues,
                    &config.providers.github,
                ))
            }
            ReferenceKind::GitHubPullRequest if config.providers.github.enabled => {
                Arc::new(GithubProvider::pull_requests(
                    &repo.invocation_root,
                    &config.leaders.github_pull_requests,
                    &config.providers.github,
                ))
            }
            ReferenceKind::JiraIssue if config.providers.jira.enabled => Arc::new(
                JiraProvider::new(&config.providers.jira, &config.leaders.jira_issues)?,
            ),
            _ => continue,
        };
        providers.push(provider);
    }
    Ok(providers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unused_providers_are_not_constructed() {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::discover(root.path()).unwrap();
        let mut config = Config::default();
        config.providers.jira.command.clear();
        assert!(configured(&repo, &config, []).unwrap().is_empty());
        assert!(configured(&repo, &config, [ReferenceKind::JiraIssue]).is_err());
    }

    #[test]
    fn disabled_remote_providers_are_omitted_without_validating_commands() {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::discover(root.path()).unwrap();
        let mut config = Config::default();
        config.providers.github.enabled = false;
        config.providers.jira.enabled = false;
        config.providers.github.command.clear();
        config.providers.jira.command.clear();
        let providers = configured(
            &repo,
            &config,
            [
                ReferenceKind::GitHubIssue,
                ReferenceKind::GitHubPullRequest,
                ReferenceKind::JiraIssue,
            ],
        )
        .unwrap();
        assert!(providers.is_empty());
    }
}
