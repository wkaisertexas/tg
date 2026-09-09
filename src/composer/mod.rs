use crate::config::Config;
use crate::language;
use crate::references::activation::lower_snapshot;
use crate::references::model::{
    CompletionActivation, FileOrigin, GenerationId, LoweredReference, QueryRequest, QueryScope,
    ReferenceCandidate, ReferenceId, ReferenceKind, ReferenceTarget, ResolvedReference, TextRange,
};
use crate::references::model::{SharedProvider, char_to_byte};
use crate::references::{CancellationFlag, ReferenceProvider};
use crate::repository::Repository;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::Path;

type Providers = HashMap<ReferenceKind, SharedProvider>;

/// Compatibility wrapper using the compiled configuration defaults.
pub fn resolve_prompt(search_root: &Path, prompt: &str) -> Result<String> {
    let repository = Repository::discover(search_root)?;
    resolve_prompt_with_config(&repository, &Config::default(), prompt)
}

/// Resolves only exact, deterministic references and lowers them atomically.
///
/// Fuzzy candidates and ambiguous exact candidates are intentionally rejected:
/// headless operation has no interface in which to ask the user to choose.
pub fn resolve_prompt_with_config(
    repository: &Repository,
    config: &Config,
    prompt: &str,
) -> Result<String> {
    let activations = scan_activations(prompt, config, &repository.search_root)?;
    let providers = providers_for(repository, config, &activations)?;
    let cancellation = CancellationFlag::default();
    let mut references = Vec::with_capacity(activations.len());
    for (index, mut activation) in activations.into_iter().enumerate() {
        let provider = providers
            .get(&activation.kind)
            .with_context(|| format!("{} provider is disabled", kind_name(activation.kind)))?;
        let target = loop {
            let request = QueryRequest {
                generation: GenerationId(1),
                query: activation.query.clone(),
                scope: activation.scope.clone(),
                // Exact filtering occurs after the provider's normal deterministic
                // ordering. External providers still enforce their configured cap.
                limit: usize::MAX,
                typed_leader: activation.typed_leader.clone(),
            };
            let candidates = provider
                .query(request, &cancellation)
                .with_context(|| format!("could not query {}", kind_name(activation.kind)))?;
            if let Some(target) = select_exact(
                provider.as_ref(),
                &activation,
                candidates,
                config.providers.jira.key_prefix.as_deref(),
            )? {
                break target;
            }
            if activation.kind == ReferenceKind::Symbol
                && trim_symbol_sentence_suffix(&mut activation)
            {
                continue;
            }
            bail!(
                "no exact {} reference matches `{}`",
                kind_name(activation.kind),
                activation.query
            );
        };
        let friendly_text = char_slice(
            prompt,
            activation.replacement_range.start,
            activation.replacement_range.end,
        )
        .context("reference range is outside the prompt")?
        .to_owned();
        references.push(ResolvedReference {
            id: ReferenceId(index as u64 + 1),
            range: activation.replacement_range,
            friendly_text,
            target,
        });
    }

    let snapshot = lower_snapshot(prompt, &references, &config.leaders, |reference| {
        let kind = reference.target.kind();
        let provider = providers
            .get(&kind)
            .with_context(|| format!("{} provider is unavailable", kind_name(kind)))?;
        let validated = provider.validate(&reference.target)?;
        let replacement = provider.lower(&validated)?;
        Ok(LoweredReference {
            replacement,
            refreshed_target: validated,
        })
    })?;
    Ok(snapshot.text)
}

fn providers_for(
    repository: &Repository,
    config: &Config,
    activations: &[CompletionActivation],
) -> Result<Providers> {
    let kinds = ReferenceKind::ALL.into_iter().filter(|kind| {
        activations
            .iter()
            .any(|activation| activation.kind == *kind)
    });
    Ok(crate::references::configured(repository, config, kinds)?
        .into_iter()
        .map(|provider| (provider.kind(), provider))
        .collect())
}

fn select_exact(
    provider: &dyn ReferenceProvider,
    activation: &CompletionActivation,
    candidates: Vec<ReferenceCandidate>,
    jira_prefix: Option<&str>,
) -> Result<Option<ReferenceTarget>> {
    let mut exact = Vec::new();
    for candidate in candidates {
        let target = provider.resolve(&candidate.id)?;
        let matches = match &target {
            ReferenceTarget::File(_) => {
                candidate.friendly_text
                    == format!("{}{}", activation.typed_leader, activation.query)
            }
            ReferenceTarget::Symbol(symbol) => {
                symbol_name_matches(&symbol.identity.leaf_name, &activation.query)
                    || symbol_name_matches(&symbol.identity.qualified_name, &activation.query)
            }
            ReferenceTarget::Skill(skill) => skill.name == activation.query,
            ReferenceTarget::ExternalUrl(url) => match url.kind {
                ReferenceKind::GitHubIssue | ReferenceKind::GitHubPullRequest => {
                    candidate.friendly_text
                        == format!("{}{}", activation.typed_leader, activation.query)
                }
                ReferenceKind::JiraIssue => jira_key_for(&activation.query, jira_prefix)
                    .is_some_and(|expected| candidate.display.primary == expected),
                _ => false,
            },
        };
        if matches {
            exact.push(target);
        }
    }

    if activation.kind == ReferenceKind::Symbol && exact.len() > 1 {
        let qualified: Vec<_> = exact
            .iter()
            .filter(|target| {
                matches!(
                    target,
                    ReferenceTarget::Symbol(symbol)
                        if symbol_name_matches(
                            &symbol.identity.qualified_name,
                            &activation.query,
                        )
                )
            })
            .cloned()
            .collect();
        if !qualified.is_empty() {
            exact = qualified;
        }
    }
    if activation.kind == ReferenceKind::Symbol && exact.len() > 1 {
        let best_priority = exact
            .iter()
            .filter_map(|target| match target {
                ReferenceTarget::Symbol(symbol) => {
                    Some(symbol_kind_priority(&symbol.identity.kind))
                }
                _ => None,
            })
            .min()
            .expect("symbol candidates have priorities");
        let preferred: Vec<_> = exact
            .iter()
            .filter(|target| {
                matches!(
                    target,
                    ReferenceTarget::Symbol(symbol)
                        if symbol_kind_priority(&symbol.identity.kind) == best_priority
                )
            })
            .cloned()
            .collect();
        if preferred.len() == 1 {
            return Ok(Some(
                preferred.into_iter().next().expect("one preferred symbol"),
            ));
        }
        exact = preferred;
        let definitions: Vec<_> = exact
            .iter()
            .filter(|target| {
                matches!(target, ReferenceTarget::Symbol(symbol) if symbol.identity.is_definition)
            })
            .cloned()
            .collect();
        if definitions.len() == 1 {
            return Ok(Some(
                definitions.into_iter().next().expect("one definition"),
            ));
        }
    }
    match exact.len() {
        1 => Ok(Some(exact.pop().expect("one exact candidate"))),
        0 => Ok(None),
        count => bail!(
            "{} reference `{}` is ambiguous ({count} exact matches)",
            kind_name(activation.kind),
            activation.query
        ),
    }
}

fn trim_symbol_sentence_suffix(activation: &mut CompletionActivation) -> bool {
    if !activation
        .query
        .chars()
        .next_back()
        .is_some_and(|character| matches!(character, '.' | '!' | '?' | ')' | ']' | '}' | '>'))
    {
        return false;
    }
    activation.query.pop();
    activation.replacement_range.end = activation.replacement_range.end.saturating_sub(1);
    true
}

fn symbol_kind_priority(kind: &str) -> u8 {
    match kind {
        "class" | "struct" | "enum" | "interface" | "trait" | "type" | "namespace" | "module"
        | "heading" => 0,
        "function" | "method" | "constructor" => 1,
        _ => 2,
    }
}

fn symbol_name_matches(name: &str, query: &str) -> bool {
    let has_operator_punctuation = |value: &str| {
        value
            .chars()
            .any(|character| matches!(character, '!' | '?' | '[' | ']' | '=' | '<' | '>'))
    };
    if has_operator_punctuation(name) || has_operator_punctuation(query) {
        name.replace("::", ".").to_lowercase() == query.replace("::", ".").to_lowercase()
    } else {
        language::names_equivalent(name, query)
    }
}

fn jira_key_for(query: &str, prefix: Option<&str>) -> Option<String> {
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
    project
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && project
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
        && !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
}

fn scan_activations(
    prompt: &str,
    config: &Config,
    search_root: &Path,
) -> Result<Vec<CompletionActivation>> {
    let leaders = sorted_leaders(config);
    let characters: Vec<(usize, char)> = prompt.char_indices().collect();
    let mut activations = Vec::new();
    let mut character_index = 0;
    while character_index < characters.len() {
        let (start_byte, _) = characters[character_index];
        if !is_boundary(prompt, start_byte) {
            character_index += 1;
            continue;
        }
        let Some((leader, kind)) = leaders
            .iter()
            .find(|(leader, _)| prompt[start_byte..].starts_with(leader))
        else {
            character_index += 1;
            continue;
        };
        let leader_chars = leader.chars().count();
        let mut end_character = character_index + leader_chars;
        while end_character < characters.len() {
            let end_byte = characters[end_character].0;
            let token = &prompt[start_byte..end_byte];
            let symbol_stage = *kind == ReferenceKind::Symbol
                || matches!(kind, ReferenceKind::GitFile | ReferenceKind::BroadFile)
                    && token[leader.len()..].contains(&config.leaders.symbols);
            if is_headless_terminator(characters[end_character].1, symbol_stage) {
                break;
            }
            end_character += 1;
        }
        if end_character == character_index + leader_chars {
            character_index += leader_chars;
            continue;
        }
        let token_end_byte = characters
            .get(end_character)
            .map_or(prompt.len(), |(byte, _)| *byte);
        let token = &prompt[start_byte..token_end_byte];
        let mut activation = headless_activation(
            *kind,
            leader,
            &token[leader.len()..],
            TextRange {
                start: character_index,
                end: end_character,
            },
            config,
            search_root,
        );
        normalize_sentence_period(&mut activation, search_root);
        activations.push(activation);
        character_index = end_character;
    }
    Ok(activations)
}

fn headless_activation(
    kind: ReferenceKind,
    leader: &str,
    query: &str,
    replacement_range: TextRange,
    config: &Config,
    search_root: &Path,
) -> CompletionActivation {
    if matches!(kind, ReferenceKind::GitFile | ReferenceKind::BroadFile)
        && let Some((relative_path, symbol_query)) = query.split_once(&config.leaders.symbols)
        && !relative_path.is_empty()
    {
        return CompletionActivation {
            kind: ReferenceKind::Symbol,
            replacement_range,
            query: symbol_query.to_owned(),
            scope: QueryScope::File {
                path: search_root.join(relative_path),
                origin: if kind == ReferenceKind::GitFile {
                    FileOrigin::GitAware
                } else {
                    FileOrigin::Broad
                },
            },
            typed_leader: leader.to_owned(),
        };
    }
    CompletionActivation {
        kind,
        replacement_range,
        query: query.to_owned(),
        scope: QueryScope::Repository,
        typed_leader: if kind == ReferenceKind::Symbol {
            String::new()
        } else {
            leader.to_owned()
        },
    }
}

fn normalize_sentence_period(activation: &mut CompletionActivation, search_root: &Path) {
    if !activation.query.ends_with('.') || activation.kind == ReferenceKind::Symbol {
        return;
    }
    if matches!(
        activation.kind,
        ReferenceKind::GitFile | ReferenceKind::BroadFile
    ) && search_root.join(&activation.query).is_file()
    {
        return;
    }
    activation.query.pop();
    activation.replacement_range.end = activation.replacement_range.end.saturating_sub(1);
}

fn sorted_leaders(config: &Config) -> Vec<(String, ReferenceKind)> {
    let leaders = &config.leaders;
    let mut values = vec![
        (leaders.files.clone(), ReferenceKind::GitFile),
        (leaders.broad_files.clone(), ReferenceKind::BroadFile),
        (leaders.symbols.clone(), ReferenceKind::Symbol),
        (leaders.skills.clone(), ReferenceKind::Skill),
        (leaders.github_issues.clone(), ReferenceKind::GitHubIssue),
        (
            leaders.github_pull_requests.clone(),
            ReferenceKind::GitHubPullRequest,
        ),
        (leaders.jira_issues.clone(), ReferenceKind::JiraIssue),
    ];
    values.sort_by_key(|(leader, _)| std::cmp::Reverse((leader.chars().count(), leader.len())));
    values
}

fn is_boundary(text: &str, byte: usize) -> bool {
    byte == 0
        || text[..byte].chars().next_back().is_some_and(|character| {
            character.is_whitespace() || matches!(character, '(' | '[' | '{' | '<' | '"' | '\'')
        })
}

fn is_headless_terminator(character: char, symbol_stage: bool) -> bool {
    character.is_whitespace()
        || if symbol_stage {
            matches!(character, ',' | ';' | '"' | '\'')
        } else {
            matches!(
                character,
                ',' | ';' | '!' | '?' | ')' | ']' | '}' | '>' | '"' | '\''
            )
        }
}

fn char_slice(text: &str, start: usize, end: usize) -> Option<&str> {
    let start = char_to_byte(text, start)?;
    let end = char_to_byte(text, end)?;
    text.get(start..end)
}

fn kind_name(kind: ReferenceKind) -> &'static str {
    match kind {
        ReferenceKind::GitFile => "file",
        ReferenceKind::BroadFile => "broad file",
        ReferenceKind::Symbol => "symbol",
        ReferenceKind::Skill => "skill",
        ReferenceKind::GitHubIssue => "GitHub issue",
        ReferenceKind::GitHubPullRequest => "GitHub pull request",
        ReferenceKind::JiraIssue => "Jira issue",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    fn executable(path: &Path, source: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, source).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn leaves_plain_text_untouched() {
        assert_eq!(resolve_prompt(Path::new("."), "hello").unwrap(), "hello");
    }

    #[test]
    fn custom_leaders_and_escapes_use_the_shared_activation_contract() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("notes.txt"), "notes").unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        let mut config = Config::default();
        config.leaders.files = "※".into();
        config.normalize_and_validate().unwrap();
        assert_eq!(
            resolve_prompt_with_config(&repository, &config, "Read ※notes.txt and \\※literal",)
                .unwrap(),
            "Read notes.txt and ※literal"
        );
    }

    #[test]
    fn fuzzy_and_ambiguous_matches_fail_without_returning_partial_output() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("first.txt"), "first").unwrap();
        fs::write(root.path().join("second.txt"), "second").unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        let prompt = "@first.txt @secon";
        let error = resolve_prompt_with_config(&repository, &Config::default(), prompt)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no exact"), "{error}");
        assert_eq!(prompt, "@first.txt @secon");

        for directory in ["one", "two"] {
            let skill = root.path().join(format!(".agents/skills/{directory}"));
            fs::create_dir_all(&skill).unwrap();
            fs::write(
                skill.join("SKILL.md"),
                "---\nname: duplicate-headless-skill\ndescription: duplicate\n---\n",
            )
            .unwrap();
        }
        let error = resolve_prompt_with_config(
            &repository,
            &Config::default(),
            "$duplicate-headless-skill",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("ambiguous"), "{error}");
    }

    #[test]
    fn unique_definition_preserves_legacy_exact_symbol_resolution() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("duplicate.cpp"),
            "void render();\nvoid render() { }\n",
        )
        .unwrap();
        assert_eq!(
            resolve_prompt(root.path(), "Use @duplicate.cpp::render.").unwrap(),
            "Use duplicate.cpp::2:6 render."
        );
    }

    #[test]
    fn standalone_repository_symbol_resolution_requires_one_exact_definition() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("model.rs"),
            "pub fn headless_unique() {}\n",
        )
        .unwrap();
        let lowered = resolve_prompt(root.path(), "Use ::headless_unique").unwrap();
        assert!(
            lowered.starts_with("Use model.rs::1:"),
            "unexpected lowering: {lowered}"
        );
        assert!(lowered.ends_with(" headless_unique"));
    }

    #[test]
    fn ruby_predicate_bang_and_operator_names_remain_part_of_symbol_tokens() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("ops.rb"),
            "class Ops\n  def danger!; end\n  def ready?; end\n  def [](key); end\n  def []=(key, value); end\n  def <=>(other); end\nend\n",
        )
        .unwrap();
        let lowered = resolve_prompt(
            root.path(),
            "Use @ops.rb::danger! @ops.rb::ready? @ops.rb::[] @ops.rb::[]= @ops.rb::<=>.",
        )
        .unwrap();
        for name in ["danger!", "ready?", "[]", "[]=", "<=>"] {
            assert!(lowered.contains(&format!(" {name}")), "{lowered}");
        }
        assert!(lowered.ends_with(" <=>."), "{lowered}");
    }

    #[test]
    fn an_exact_skill_uses_its_configured_mention_lowering() {
        let root = tempfile::tempdir().unwrap();
        let skill = root.path().join(".agents/skills/headless-exact");
        fs::create_dir_all(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: headless-exact\ndescription: exact skill\n---\n",
        )
        .unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        let mut config = Config::default();
        config.skills.mention = "invoke:${name}".into();
        config.normalize_and_validate().unwrap();
        assert_eq!(
            resolve_prompt_with_config(&repository, &config, "Use $headless-exact").unwrap(),
            "Use invoke:headless-exact"
        );
    }

    #[test]
    fn disabled_external_provider_is_an_atomic_error() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("notes.txt"), "notes").unwrap();
        let repository = Repository::discover(root.path()).unwrap();
        let mut config = Config::default();
        config.providers.github.enabled = false;
        let prompt = "@notes.txt #42";
        let error = resolve_prompt_with_config(&repository, &config, prompt)
            .unwrap_err()
            .to_string();
        assert!(error.contains("disabled"), "{error}");
        assert_eq!(prompt, "@notes.txt #42");
    }

    #[cfg(unix)]
    #[test]
    fn exact_github_and_jira_tokens_lower_through_current_providers() {
        let root = tempfile::tempdir().unwrap();
        let gh = root.path().join("fake-gh");
        executable(
            &gh,
            r#"#!/bin/sh
if [ "$1" = "issue" ]; then
  printf '%s' '[{"number":42,"title":"Issue","url":"https://git.corp/a/b/issues/42","state":"OPEN","labels":[],"updatedAt":"2026-08-17T10:00:00Z"}]'
else
  printf '%s' '[{"number":7,"title":"PR","url":"https://git.corp/a/b/pull/7","state":"OPEN","isDraft":false,"updatedAt":"2026-08-17T10:00:00Z"}]'
fi
"#,
        );
        let jira = root.path().join("fake-jira");
        executable(
            &jira,
            r#"#!/bin/sh
printf '%s' '{"key":"G5-123","fields":{"summary":"Jira","status":{"name":"Open"},"updated":"2026-08-17"},"self":"https://jira.corp/rest/api/2/issue/G5-123"}'
"#,
        );
        let repository = Repository::discover(root.path()).unwrap();
        let mut config = Config::default();
        config.providers.github.command = gh;
        config.providers.jira.command = jira;
        config.providers.jira.key_prefix = Some("G5".into());

        assert_eq!(
            resolve_prompt_with_config(&repository, &config, "Fix #42, review !7 and &123.")
                .unwrap(),
            "Fix https://git.corp/a/b/issues/42, review https://git.corp/a/b/pull/7 and https://jira.corp/browse/G5-123."
        );
    }
}
