use super::model::{
    CompletionActivation, FileOrigin, LoweredReference, ReferenceKind, ResolvedReference, TextRange,
};
use crate::config::LeadersConfig;
use anyhow::{Context, Result, bail};
use std::path::Path;

/// Opening delimiters which permit a leader to begin a reference token.
const OPENING_DELIMITERS: &[char] = &['(', '[', '{', '<', '"', '\''];

/// Find the active reference query ending at `cursor`, which is measured in
/// Unicode scalar values rather than UTF-8 bytes.
pub fn detect_activation(
    text: &str,
    cursor: usize,
    leaders: &LeadersConfig,
    search_root: &Path,
) -> Result<Option<CompletionActivation>> {
    let cursor_byte = char_to_byte(text, cursor).context("cursor is outside the document")?;
    let prefix = &text[..cursor_byte];
    let leader_specs = sorted_leaders(leaders);
    let mut active = None;

    for (start_byte, _) in prefix.char_indices() {
        if !is_token_boundary(prefix, start_byte) {
            continue;
        }
        for (leader, kind) in &leader_specs {
            let after_leader = start_byte + leader.len();
            if after_leader > prefix.len() || !prefix[start_byte..].starts_with(leader) {
                continue;
            }
            let query = &prefix[after_leader..];
            if query.chars().any(is_token_terminator) {
                continue;
            }
            let replacement_range = TextRange::new(byte_to_char(text, start_byte), cursor)?;
            active = Some(activation_for(
                *kind,
                leader,
                query,
                replacement_range,
                leaders,
                search_root,
            )?);
            // Leaders are ordered longest-first, so the first match at a
            // given position is the only unambiguous one.
            break;
        }
    }
    Ok(active)
}

fn activation_for(
    kind: ReferenceKind,
    typed_leader: &str,
    query: &str,
    replacement_range: TextRange,
    leaders: &LeadersConfig,
    search_root: &Path,
) -> Result<CompletionActivation> {
    if matches!(kind, ReferenceKind::GitFile | ReferenceKind::BroadFile)
        && let Some((relative_path, symbol_query)) = query.split_once(&leaders.symbols)
        && !relative_path.is_empty()
    {
        return Ok(CompletionActivation {
            kind: ReferenceKind::Symbol,
            replacement_range,
            query: symbol_query.into(),
            scope: super::model::QueryScope::File {
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
        });
    }

    Ok(CompletionActivation {
        kind,
        replacement_range,
        query: query.into(),
        scope: super::model::QueryScope::Repository,
        // Standalone symbol results lower through their containing file and
        // therefore use the SymbolProvider's configured file leader.
        typed_leader: if kind == ReferenceKind::Symbol {
            String::new()
        } else {
            typed_leader.into()
        },
    })
}

/// A character-indexed replacement used at the editor boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharReplacement {
    pub range: TextRange,
    pub text: String,
}

/// The rendered prompt and refreshed reference metadata produced from the
/// same immutable friendly-document snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredSnapshot {
    pub text: String,
    pub references: Vec<ResolvedReference>,
}

/// Validate every structured reference, then render all lowered replacements
/// end-to-start. If any validation fails, no partially rendered snapshot is
/// returned.
pub fn lower_snapshot(
    text: &str,
    references: &[ResolvedReference],
    leaders: &LeadersConfig,
    mut validate_and_lower: impl FnMut(&ResolvedReference) -> Result<LoweredReference>,
) -> Result<LoweredSnapshot> {
    validate_reference_ranges(text, references)?;

    let lowered = references
        .iter()
        .map(&mut validate_and_lower)
        .collect::<Result<Vec<_>>>()?;
    let mut refreshed = Vec::with_capacity(references.len());
    let mut replacements = Vec::with_capacity(references.len());
    for (reference, lowered) in references.iter().zip(lowered) {
        replacements.push(CharReplacement {
            range: reference.range,
            text: lowered.replacement,
        });
        let mut reference = reference.clone();
        reference.target = lowered.refreshed_target.target;
        refreshed.push(reference);
    }

    replacements.extend(escaped_leader_replacements(text, references, leaders));
    Ok(LoweredSnapshot {
        text: apply_char_replacements(text, &replacements)?,
        references: refreshed,
    })
}

/// Apply non-overlapping character-indexed replacements to UTF-8 text.
pub fn apply_char_replacements(text: &str, replacements: &[CharReplacement]) -> Result<String> {
    let character_count = text.chars().count();
    let mut ordered = replacements.to_vec();
    ordered.sort_by_key(|replacement| (replacement.range.start, replacement.range.end));
    for replacement in &ordered {
        if replacement.range.end > character_count {
            bail!("replacement range is outside the document");
        }
    }
    for pair in ordered.windows(2) {
        if pair[0].range.end > pair[1].range.start {
            bail!("replacement ranges overlap");
        }
    }

    let mut output = text.to_owned();
    for replacement in ordered.into_iter().rev() {
        let start = char_to_byte(&output, replacement.range.start)
            .context("replacement start is outside the document")?;
        let end = char_to_byte(&output, replacement.range.end)
            .context("replacement end is outside the document")?;
        output.replace_range(start..end, &replacement.text);
    }
    Ok(output)
}

fn validate_reference_ranges(text: &str, references: &[ResolvedReference]) -> Result<()> {
    let mut ranges: Vec<_> = references.iter().collect();
    ranges.sort_by_key(|reference| (reference.range.start, reference.range.end));
    for reference in &ranges {
        let actual = char_slice(text, reference.range)
            .context("structured reference range is outside the document")?;
        anyhow::ensure!(
            actual == reference.friendly_text,
            "structured reference text is stale"
        );
    }
    for pair in ranges.windows(2) {
        anyhow::ensure!(
            pair[0].range.end <= pair[1].range.start,
            "structured reference ranges overlap"
        );
    }
    Ok(())
}

fn escaped_leader_replacements(
    text: &str,
    references: &[ResolvedReference],
    leaders: &LeadersConfig,
) -> Vec<CharReplacement> {
    let leader_specs = sorted_leaders(leaders);
    let mut replacements = Vec::new();
    for (slash_byte, character) in text.char_indices() {
        if character != '\\' || !is_token_boundary(text, slash_byte) {
            continue;
        }
        let after_slash = slash_byte + character.len_utf8();
        if leader_specs
            .iter()
            .any(|(leader, _)| text[after_slash..].starts_with(leader))
        {
            let slash_char = byte_to_char(text, slash_byte);
            if !references.iter().any(|reference| {
                reference.range.start <= slash_char && slash_char < reference.range.end
            }) {
                replacements.push(CharReplacement {
                    range: TextRange {
                        start: slash_char,
                        end: slash_char + 1,
                    },
                    text: String::new(),
                });
            }
        }
    }
    replacements
}

fn sorted_leaders(leaders: &LeadersConfig) -> Vec<(String, ReferenceKind)> {
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
    values.sort_by(|(left, _), (right, _)| {
        right
            .chars()
            .count()
            .cmp(&left.chars().count())
            .then_with(|| right.len().cmp(&left.len()))
    });
    values
}

fn is_token_boundary(text: &str, byte: usize) -> bool {
    byte == 0
        || text[..byte].chars().next_back().is_some_and(|character| {
            character.is_whitespace() || OPENING_DELIMITERS.contains(&character)
        })
}

fn is_token_terminator(character: char) -> bool {
    character.is_whitespace() || matches!(character, ')' | ']' | '}' | '>' | '"' | '\'')
}

fn char_to_byte(text: &str, character: usize) -> Option<usize> {
    if character == text.chars().count() {
        return Some(text.len());
    }
    text.char_indices().nth(character).map(|(byte, _)| byte)
}

fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte].chars().count()
}

fn char_slice(text: &str, range: TextRange) -> Option<&str> {
    let start = char_to_byte(text, range.start)?;
    let end = char_to_byte(text, range.end)?;
    text.get(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::model::{
        ContextCost, ReferenceId, ReferenceTarget, SkillTarget, ValidatedTarget,
    };
    use std::path::PathBuf;

    fn activation(
        text: &str,
        leaders: &LeadersConfig,
        root: &Path,
    ) -> Option<CompletionActivation> {
        detect_activation(text, text.chars().count(), leaders, root).unwrap()
    }

    #[test]
    fn leaders_activate_only_at_boundaries_and_longest_match_wins() {
        let root = tempfile::tempdir().unwrap();
        let leaders = LeadersConfig {
            files: ":".into(),
            symbols: "::".into(),
            ..LeadersConfig::default()
        };

        assert!(activation("email@example.com", &leaders, root.path()).is_none());
        assert!(activation("word:query", &leaders, root.path()).is_none());
        assert_eq!(
            activation("(::Widget", &leaders, root.path()).unwrap().kind,
            ReferenceKind::Symbol
        );
        assert_eq!(
            activation(" :file", &leaders, root.path()).unwrap().kind,
            ReferenceKind::GitFile
        );
    }

    #[test]
    fn custom_unicode_and_multichar_leaders_are_character_ranged() {
        let root = tempfile::tempdir().unwrap();
        let leaders = LeadersConfig {
            files: "※※".into(),
            ..LeadersConfig::default()
        };
        let found = activation("λ (※※src/lib.rs", &leaders, root.path()).unwrap();
        assert_eq!(found.kind, ReferenceKind::GitFile);
        assert_eq!(found.query, "src/lib.rs");
        assert_eq!(found.typed_leader, "※※");
        assert_eq!(found.replacement_range, TextRange { start: 3, end: 15 });
    }

    #[test]
    fn standalone_and_file_scoped_symbols_keep_scope_and_origin() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("ordinary.rs"), "fn ordinary() {}\n").unwrap();
        std::fs::write(root.path().join("ignored.rs"), "fn ignored() {}\n").unwrap();
        let leaders = LeadersConfig::default();

        let standalone = activation("::ordi", &leaders, root.path()).unwrap();
        assert_eq!(standalone.kind, ReferenceKind::Symbol);
        assert_eq!(
            standalone.scope,
            super::super::model::QueryScope::Repository
        );
        assert!(standalone.typed_leader.is_empty());

        for (text, expected_origin, expected_leader) in [
            ("@ordinary.rs::ord", FileOrigin::GitAware, "@"),
            ("%ignored.rs::ign", FileOrigin::Broad, "%"),
        ] {
            let found = activation(text, &leaders, root.path()).unwrap();
            assert_eq!(found.kind, ReferenceKind::Symbol);
            assert_eq!(found.query, &text[text.rfind("::").unwrap() + 2..]);
            assert_eq!(found.typed_leader, expected_leader);
            let super::super::model::QueryScope::File { origin, path } = found.scope else {
                panic!("expected file scope")
            };
            assert_eq!(origin, expected_origin);
            assert!(path.ends_with("ordinary.rs") || path.ends_with("ignored.rs"));
        }
    }

    #[test]
    fn escaped_leaders_do_not_activate_and_lose_one_escape_when_lowered() {
        let root = tempfile::tempdir().unwrap();
        let leaders = LeadersConfig::default();
        assert!(activation("\\@ordinary", &leaders, root.path()).is_none());
        assert!(activation("word\\@ordinary", &leaders, root.path()).is_none());

        let lowered = lower_snapshot(
            "\\@ordinary word\\@literal",
            &[],
            &leaders,
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(lowered.text, "@ordinary word\\@literal");
    }

    fn reference(id: u64, range: TextRange, friendly: &str) -> ResolvedReference {
        ResolvedReference {
            id: ReferenceId(id),
            range,
            friendly_text: friendly.into(),
            target: ReferenceTarget::Skill(SkillTarget {
                name: friendly.into(),
                mention: friendly.into(),
                source_path: PathBuf::from("SKILL.md"),
            }),
        }
    }

    #[test]
    fn unicode_multi_reference_lowering_validates_then_replaces_end_to_start() {
        let leaders = LeadersConfig::default();
        let text = "λ @one and #two plus \\%literal";
        let references = [
            reference(1, TextRange { start: 2, end: 6 }, "@one"),
            reference(2, TextRange { start: 11, end: 15 }, "#two"),
        ];
        let lowered = lower_snapshot(text, &references, &leaders, |reference| {
            let replacement = match reference.id.0 {
                1 => "src/λ.rs",
                2 => "https://example.test/issues/2",
                _ => unreachable!(),
            };
            Ok(LoweredReference {
                replacement: replacement.into(),
                refreshed_target: ValidatedTarget {
                    target: reference.target.clone(),
                    context_cost: ContextCost::None,
                },
            })
        })
        .unwrap();
        assert_eq!(
            lowered.text,
            "λ src/λ.rs and https://example.test/issues/2 plus %literal"
        );
        assert_eq!(lowered.references.len(), 2);
    }

    #[test]
    fn stale_or_overlapping_ranges_fail_without_calling_providers() {
        let leaders = LeadersConfig::default();
        let stale = [reference(1, TextRange { start: 0, end: 4 }, "@old")];
        let mut calls = 0;
        assert!(
            lower_snapshot("@new", &stale, &leaders, |_| {
                calls += 1;
                unreachable!()
            })
            .is_err()
        );
        assert_eq!(calls, 0);

        let overlap = [
            reference(1, TextRange { start: 0, end: 4 }, "@one"),
            reference(2, TextRange { start: 3, end: 7 }, "e #two"),
        ];
        assert!(lower_snapshot("@one #two", &overlap, &leaders, |_| unreachable!()).is_err());
    }

    #[test]
    fn provider_validation_errors_return_no_snapshot() {
        let leaders = LeadersConfig::default();
        let references = [
            reference(1, TextRange { start: 0, end: 4 }, "@one"),
            reference(2, TextRange { start: 5, end: 9 }, "#two"),
        ];
        let error = lower_snapshot("@one #two", &references, &leaders, |reference| {
            if reference.id.0 == 2 {
                bail!("selected target is stale")
            }
            Ok(LoweredReference {
                replacement: "one".into(),
                refreshed_target: ValidatedTarget {
                    target: reference.target.clone(),
                    context_cost: ContextCost::None,
                },
            })
        })
        .unwrap_err();
        assert!(error.to_string().contains("stale"));
    }

    #[test]
    fn generic_utf8_replacement_rejects_overlap_and_preserves_boundaries() {
        assert_eq!(
            apply_char_replacements(
                "aλb",
                &[CharReplacement {
                    range: TextRange { start: 1, end: 2 },
                    text: "世界".into(),
                }],
            )
            .unwrap(),
            "a世界b"
        );
        assert!(
            apply_char_replacements(
                "abcd",
                &[
                    CharReplacement {
                        range: TextRange { start: 0, end: 3 },
                        text: String::new(),
                    },
                    CharReplacement {
                        range: TextRange { start: 2, end: 4 },
                        text: String::new(),
                    },
                ],
            )
            .is_err()
        );
    }
}
