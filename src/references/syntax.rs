use super::model::{
    CompletionActivation, FileOrigin, QueryScope, ReferenceKind, TextRange, char_to_byte,
};
use crate::config::LeadersConfig;
use std::path::Path;

/// Opening delimiters which permit a leader to begin a reference token.
const OPENING_DELIMITERS: &[char] = &['(', '[', '{', '<', '"', '\''];

pub(crate) fn sorted_leaders(leaders: &LeadersConfig) -> [(&str, ReferenceKind); 7] {
    let mut values = [
        (leaders.files.as_str(), ReferenceKind::GitFile),
        (leaders.broad_files.as_str(), ReferenceKind::BroadFile),
        (leaders.symbols.as_str(), ReferenceKind::Symbol),
        (leaders.skills.as_str(), ReferenceKind::Skill),
        (leaders.github_issues.as_str(), ReferenceKind::GitHubIssue),
        (
            leaders.github_pull_requests.as_str(),
            ReferenceKind::GitHubPullRequest,
        ),
        (leaders.jira_issues.as_str(), ReferenceKind::JiraIssue),
    ];
    values.sort_by_key(|(leader, _)| std::cmp::Reverse((leader.chars().count(), leader.len())));
    values
}

pub(crate) fn is_token_boundary(text: &str, byte: usize) -> bool {
    byte == 0
        || text[..byte].chars().next_back().is_some_and(|character| {
            character.is_whitespace() || OPENING_DELIMITERS.contains(&character)
        })
}

pub(crate) fn activation_for(
    kind: ReferenceKind,
    typed_leader: &str,
    query: &str,
    replacement_range: TextRange,
    leaders: &LeadersConfig,
    search_root: &Path,
) -> CompletionActivation {
    if matches!(kind, ReferenceKind::GitFile | ReferenceKind::BroadFile)
        && let Some((relative_path, symbol_query)) = query.split_once(&leaders.symbols)
        && !relative_path.is_empty()
    {
        return CompletionActivation {
            kind: ReferenceKind::Symbol,
            replacement_range,
            query: symbol_query.into(),
            scope: QueryScope::File {
                path: search_root.join(relative_path),
                origin: if kind == ReferenceKind::GitFile {
                    FileOrigin::GitAware
                } else {
                    FileOrigin::Broad
                },
            },
            // Symbol candidates need the original file leader so accepting a
            // broad-file symbol remains `%path::name` rather than `@path::name`.
            typed_leader: typed_leader.into(),
        };
    }
    CompletionActivation {
        kind,
        replacement_range,
        query: query.into(),
        scope: QueryScope::Repository,
        // Standalone symbol results lower through their containing file and
        // therefore use the SymbolProvider's configured file leader.
        typed_leader: if kind == ReferenceKind::Symbol {
            String::new()
        } else {
            typed_leader.into()
        },
    }
}

pub(crate) fn jira_key_for(query: &str, prefix: Option<&str>) -> Option<String> {
    // Jira direct-key queries return exactly one candidate. This helper keeps
    // text searches interactive-only without depending on provider internals.
    let query = query.trim();
    if is_jira_key(query) {
        Some(query.to_owned())
    } else if !query.is_empty() && query.bytes().all(|byte| byte.is_ascii_digit()) {
        prefix.map(|prefix| format!("{prefix}-{query}"))
    } else {
        None
    }
}

fn is_jira_key(query: &str) -> bool {
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

pub(crate) fn char_slice(text: &str, range: TextRange) -> Option<&str> {
    let start = char_to_byte(text, range.start)?;
    let end = char_to_byte(text, range.end)?;
    text.get(start..end)
}
