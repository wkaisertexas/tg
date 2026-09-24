use super::*;
use crate::config::LeadersConfig;
use crate::references::activation::{detect_activation, lower_snapshot};
use crate::references::model::{FileOrigin, QueryScope};

#[test]
fn complete_references_share_boundaries_scopes_and_character_ranges() {
    let root = tempfile::tempdir().unwrap();
    let config = Config {
        leaders: LeadersConfig {
            files: "※f".into(),
            broad_files: "※a".into(),
            symbols: ":::".into(),
            skills: "§".into(),
            github_issues: "##".into(),
            github_pull_requests: "!!".into(),
            jira_issues: "&&".into(),
        },
        ..Config::default()
    };
    let cases = [
        (
            "※fsrc/λ.rs",
            ReferenceKind::GitFile,
            "src/λ.rs",
            QueryScope::Repository,
            "※f",
        ),
        (
            "※asrc/λ.rs",
            ReferenceKind::BroadFile,
            "src/λ.rs",
            QueryScope::Repository,
            "※a",
        ),
        (
            ":::render",
            ReferenceKind::Symbol,
            "render",
            QueryScope::Repository,
            "",
        ),
        (
            "§review",
            ReferenceKind::Skill,
            "review",
            QueryScope::Repository,
            "§",
        ),
        (
            "##123",
            ReferenceKind::GitHubIssue,
            "123",
            QueryScope::Repository,
            "##",
        ),
        (
            "!!42",
            ReferenceKind::GitHubPullRequest,
            "42",
            QueryScope::Repository,
            "!!",
        ),
        (
            "&&OPS-7",
            ReferenceKind::JiraIssue,
            "OPS-7",
            QueryScope::Repository,
            "&&",
        ),
        (
            "※fsrc/λ.rs:::render",
            ReferenceKind::Symbol,
            "render",
            QueryScope::File {
                path: root.path().join("src/λ.rs"),
                origin: FileOrigin::GitAware,
            },
            "※f",
        ),
        (
            "※asrc/λ.rs:::render",
            ReferenceKind::Symbol,
            "render",
            QueryScope::File {
                path: root.path().join("src/λ.rs"),
                origin: FileOrigin::Broad,
            },
            "※a",
        ),
        (
            "※f:::render",
            ReferenceKind::GitFile,
            ":::render",
            QueryScope::Repository,
            "※f",
        ),
    ];
    for prefix in ["", "λ ", "λ\n", "λ (", "λ [", "λ {", "λ <", "λ \"", "λ '"] {
        for (token, kind, query, scope, leader) in &cases {
            let prompt = format!("{prefix}{token}");
            let live = detect_activation(
                &prompt,
                prompt.chars().count(),
                &config.leaders,
                root.path(),
            )
            .unwrap()
            .unwrap();
            let mut headless = scan_activations(&prompt, &config, root.path()).unwrap();
            assert_eq!(headless.len(), 1, "{prompt}");
            for activation in [live, headless.remove(0)] {
                assert_eq!(activation.kind, *kind, "{prompt}");
                assert_eq!(activation.query, *query, "{prompt}");
                assert_eq!(activation.scope, *scope, "{prompt}");
                assert_eq!(activation.typed_leader, *leader, "{prompt}");
                assert_eq!(
                    activation.replacement_range,
                    TextRange::new(prefix.chars().count(), prompt.chars().count()).unwrap()
                );
            }
            let escaped = format!("{prefix}\\{token}");
            assert!(
                detect_activation(
                    &escaped,
                    escaped.chars().count(),
                    &config.leaders,
                    root.path()
                )
                .unwrap()
                .is_none()
            );
            assert!(
                scan_activations(&escaped, &config, root.path())
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                lower_snapshot(&escaped, &[], &config.leaders, |_| unreachable!())
                    .unwrap()
                    .text,
                prompt
            );
        }
    }
}

#[test]
fn scanners_preserve_their_incomplete_query_and_punctuation_policies() {
    let root = tempfile::tempdir().unwrap();
    let config = Config::default();
    for (prompt, live_query, headless_query) in [
        ("email@example.test", None, None),
        ("word@file", None, None),
        ("/@file", None, None),
        (".@file", None, None),
        ("\\@file", None, None),
        ("@", Some(""), None),
        ("::", Some(""), None),
        ("@file::", Some(""), Some("")),
        ("@file,", Some("file,"), Some("file")),
        ("@file!", Some("file!"), Some("file")),
        ("@file?", Some("file?"), Some("file")),
        ("@file)", None, Some("file")),
        ("@file.", Some("file."), Some("file")),
        ("@ops.rb::ready?", Some("ready?"), Some("ready?")),
        ("@ops.rb::danger!", Some("danger!"), Some("danger!")),
        ("@ops.rb::[]", None, Some("[]")),
        ("@ops.rb::<=>.", None, Some("<=>.")),
    ] {
        let live = detect_activation(prompt, prompt.chars().count(), &config.leaders, root.path())
            .unwrap();
        assert_eq!(
            live.as_ref().map(|activation| activation.query.as_str()),
            live_query,
            "{prompt}"
        );
        let headless = scan_activations(prompt, &config, root.path()).unwrap();
        assert_eq!(
            headless.len(),
            usize::from(headless_query.is_some()),
            "{prompt}"
        );
        assert_eq!(
            headless.first().map(|activation| activation.query.as_str()),
            headless_query,
            "{prompt}"
        );
    }
    fs::write(root.path().join("file."), "fixture").unwrap();
    let existing = scan_activations("@file.", &config, root.path()).unwrap();
    assert_eq!(existing[0].query, "file.");
    assert_eq!(existing[0].replacement_range.end, 6);
    let punctuation = scan_activations("@file..", &config, root.path()).unwrap();
    assert_eq!(punctuation[0].query, "file.");
    assert_eq!(punctuation[0].replacement_range.end, 6);
}

#[test]
fn headless_jira_keys_are_ascii_explicit_or_prefix_expanded() {
    for (query, prefix, expected) in [
        (" OPS-123 ", None, Some("OPS-123")),
        ("other-007", Some("G5"), Some("other-007")),
        ("A-0", None, Some("A-0")),
        ("123", None, None),
        (" 00123 ", Some("G5"), Some("G5-00123")),
        (" ", Some("G5"), None),
        ("OPS-١", Some("G5"), None),
        ("Å-1", None, None),
        ("5G-2", None, None),
        ("A_B-2", None, None),
        ("AB--2", None, None),
        ("AB-2-extra", None, None),
        ("text with spaces", Some("G5"), None),
    ] {
        assert_eq!(
            jira_key_for(query, prefix).as_deref(),
            expected,
            "{query:?}"
        );
    }
}
