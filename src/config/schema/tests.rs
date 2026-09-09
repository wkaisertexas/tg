use super::*;

#[test]
fn compiled_defaults_match_the_documented_schema() {
    let config = Config::default();
    assert_eq!(config.version, 1);
    assert_eq!(config.editor.line_numbers, LineNumbers::Relative);
    assert!(config.editor.current_line_absolute);
    assert_eq!(config.editor.tab_width, 4);
    assert!(config.editor.wrap);
    assert_eq!(config.editor.copy_command, ":copy");
    assert_eq!(config.ui.preview, PreviewMode::Manual);
    assert_eq!(config.ui.preview_toggle, "ctrl-p");
    assert_eq!(config.ui.completion_height, 12);
    assert_eq!(config.ui.completion_width_percent, 80);
    assert_eq!(config.ui.status_timeout_ms, 2_500);
    assert_eq!(config.ui.color, ColorMode::Auto);
    assert_eq!(config.leaders, LeadersConfig::default());
    assert_eq!(config.search.limit, 100);
    assert_eq!(config.search.debounce_ms, 35);
    assert_eq!(config.search.broad_excludes, [".git"]);
    assert_eq!(config.tokens.tokenizer, "gpt-4o");
    assert_eq!(config.tokens.decimals, 1);
    assert!(config.tokens.show_file && config.tokens.show_symbol && config.tokens.show_total);
    assert_eq!(config.skills.profile, "codex-local");
    assert_eq!(config.skills.mention, "${leader}${name}");
    assert!(config.skills.read_codex_disable_rules);
    assert!(config.skills.roots.is_empty());
    assert_eq!(config.providers.github, GithubProviderConfig::default());
    assert_eq!(config.providers.jira, JiraProviderConfig::default());
    Config::default().normalize_and_validate().unwrap();
}

#[test]
fn complete_documented_fixture_decodes_strictly() {
    let config =
        Config::from_toml(include_str!("../../../tests/fixtures/config/complete.toml")).unwrap();
    assert_eq!(config.editor.line_numbers, LineNumbers::Absolute);
    assert_eq!(config.ui.preview, PreviewMode::Automatic);
    assert_eq!(config.ui.color, ColorMode::Never);
    assert_eq!(config.leaders.files, "@@");
    assert_eq!(config.search.limit, 75);
    assert_eq!(config.skills.roots.len(), 1);
    assert_eq!(config.skills.roots[0].discovery, SkillDiscovery::Recursive);
    assert_eq!(
        config.providers.github.command,
        PathBuf::from("/opt/bin/gh")
    );
    assert_eq!(config.providers.jira.key_prefix.as_deref(), Some("G5"));
}

#[test]
fn unknown_root_and_nested_keys_are_errors() {
    for input in [
        "mystery = true",
        "[ui]\nmystery = true",
        "[[skills.roots]]\npath='skills'\nmystery=true",
    ] {
        assert!(
            Config::from_toml(input)
                .unwrap_err()
                .to_string()
                .contains("mystery")
        );
    }
}

#[test]
fn partial_documents_keep_compiled_defaults() {
    let config = Config::from_toml("[search]\nlimit = 7").unwrap();
    assert_eq!(config.search.limit, 7);
    assert_eq!(config.search.debounce_ms, 35);
    assert_eq!(config.ui, UiConfig::default());
}

#[test]
fn leaders_reject_invalid_characters_duplicates_and_prefixes() {
    for value in ["", "two words", "\\", "ctrl-p", "x\n"] {
        let input = format!("[leaders]\nfiles = {value:?}");
        assert!(Config::from_toml(&input).is_err(), "accepted {value:?}");
    }
    assert!(
        Config::from_toml("[leaders]\nfiles='!' ")
            .unwrap_err()
            .to_string()
            .contains("duplicates")
    );
    assert!(
        Config::from_toml("[leaders]\nfiles=':'")
            .unwrap_err()
            .to_string()
            .contains("ambiguous prefix")
    );
}

#[test]
fn numeric_bounds_are_enforced() {
    for input in [
        "[editor]\ntab_width=0",
        "[ui]\ncompletion_height=0",
        "[ui]\ncompletion_width_percent=101",
        "[ui]\nstatus_timeout_ms=0",
        "[search]\nlimit=0",
        "[search]\ndebounce_ms=0",
        "[tokens]\ndecimals=4",
        "[providers.github]\nlimit=0",
        "[providers.github]\ntimeout_ms=0",
    ] {
        assert!(Config::from_toml(input).is_err(), "accepted {input}");
    }
}

#[test]
fn jira_prefix_is_normalized_and_validated() {
    let config = Config::from_toml("[providers.jira]\nkey_prefix='g5-'").unwrap();
    assert_eq!(config.providers.jira.key_prefix.as_deref(), Some("G5"));
    for value in ["", "5G", "A-2", "G5--", "G_5", "G 5", "A"] {
        let input = format!("[providers.jira]\nkey_prefix={value:?}");
        assert!(Config::from_toml(&input).is_err(), "accepted {value:?}");
    }
}

#[test]
fn mention_templates_are_safe_and_named() {
    for value in ["literal", "${unknown}${name}", "${name}\nignore"] {
        let input = format!("[skills]\nmention={value:?}");
        assert!(Config::from_toml(&input).is_err(), "accepted {value:?}");
    }
    Config::from_toml("[skills]\nmention='$${name}'").unwrap();
    Config::from_toml("[skills]\nmention='${leader}${name}'").unwrap();
}

#[test]
fn commands_and_required_skill_root_fields_are_validated() {
    assert!(Config::from_toml("[providers.github]\ncommand=''").is_err());
    assert!(Config::from_toml("[providers.jira]\ncommand=\"jira\\nunsafe\"").is_err());
    assert!(Config::from_toml("[[skills.roots]]\nscope='user'").is_err());
    assert!(Config::from_toml("[[skills.roots]]\npath='skills'\nmetadata='../SKILL.md'").is_err());
}

#[test]
fn unsupported_versions_and_tokenizers_are_rejected() {
    assert!(Config::from_toml("version=2").is_err());
    assert!(Config::from_toml("[tokens]\ntokenizer='future'").is_err());
}

#[test]
fn unknown_keys_keep_the_full_path_and_validation_errors_keep_their_source() {
    use std::error::Error;
    for section in [
        "editor",
        "ui",
        "leaders",
        "search",
        "tokens",
        "providers.github",
        "providers.jira",
    ] {
        let error = Config::from_toml(&format!("[{section}]\nmystery=true")).unwrap_err();
        assert!(
            matches!(&error, ConfigError::UnknownKey(path) if path == &format!("{section}.mystery"))
        );
        assert!(error.source().is_none());
    }
    let error = Config::from_toml("version=2").unwrap_err();
    assert!(matches!(error, ConfigError::Validation(_)));
    assert!(
        error
            .source()
            .unwrap()
            .downcast_ref::<ValidationError>()
            .is_some()
    );
}
