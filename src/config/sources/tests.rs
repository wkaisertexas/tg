use super::*;

fn inputs(temp: &tempfile::TempDir) -> ConfigInputs {
    let mut inputs = ConfigInputs::new(temp.path().to_path_buf());
    inputs.home_dir = Some(temp.path().join("home"));
    inputs.repository_root = Some(temp.path().join("repo"));
    fs::create_dir_all(inputs.home_dir.as_ref().unwrap()).unwrap();
    fs::create_dir_all(inputs.repository_root.as_ref().unwrap()).unwrap();
    inputs
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn layers_defaults_user_project_environment_and_cli() {
    let temp = tempfile::tempdir().unwrap();
    let mut inputs = inputs(&temp);
    let user = inputs
        .home_dir
        .as_ref()
        .unwrap()
        .join(".config/tg/config.toml");
    write(
        &user,
        "[search]\nlimit = 20\nbroad_excludes = [\"user\"]\n[tokens]\ndecimals = 2\n",
    );
    write(
        &inputs.repository_root.as_ref().unwrap().join(".tg.toml"),
        "[search]\nlimit = 30\nbroad_excludes = [\"project\"]\n[ui]\npreview = \"automatic\"\n",
    );
    inputs
        .environment
        .insert("TG_TOKENIZER".into(), "gpt-4o".into());
    inputs.environment.insert("NO_COLOR".into(), "1".into());
    inputs.cli.no_preview = true;
    let loaded = load(&inputs).unwrap();
    assert_eq!(loaded.config.search.limit, 30);
    assert_eq!(loaded.config.search.broad_excludes, ["project"]);
    assert_eq!(loaded.config.tokens.decimals, 2);
    assert_eq!(loaded.config.ui.preview, PreviewMode::Disabled);
    assert_eq!(loaded.config.ui.color, ColorMode::Never);
    assert_eq!(loaded.sources.len(), 4);
}

#[test]
fn xdg_path_is_used_and_cli_path_has_priority_over_tg_config() {
    let temp = tempfile::tempdir().unwrap();
    let mut inputs = inputs(&temp);
    inputs
        .environment
        .insert("XDG_CONFIG_HOME".into(), "xdg".into());
    write(
        &temp.path().join("xdg/tg/config.toml"),
        "[search]\nlimit = 7\n",
    );
    assert_eq!(load(&inputs).unwrap().config.search.limit, 7);
    write(&temp.path().join("env.toml"), "[search]\nlimit = 8\n");
    write(&temp.path().join("cli.toml"), "[search]\nlimit = 9\n");
    inputs
        .environment
        .insert("TG_CONFIG".into(), "env.toml".into());
    inputs.cli.config_path = Some("cli.toml".into());
    assert_eq!(load(&inputs).unwrap().config.search.limit, 9);
}

#[test]
fn explicit_missing_and_invalid_files_name_the_path() {
    let temp = tempfile::tempdir().unwrap();
    let mut inputs = inputs(&temp);
    inputs.cli.config_path = Some("missing.toml".into());
    let error = load(&inputs).unwrap_err().to_string();
    assert!(error.contains("missing.toml"), "{error}");
    let bad = temp.path().join("bad.toml");
    write(&bad, "[search]\nunknown = 1\n");
    inputs.cli.config_path = Some(bad.clone());
    let error = load(&inputs).unwrap_err().to_string();
    assert!(error.contains(&bad.display().to_string()), "{error}");
    assert!(error.contains("search.unknown"), "{error}");
}

#[test]
fn each_file_is_strict_and_validated_before_later_layers() {
    let temp = tempfile::tempdir().unwrap();
    let mut inputs = inputs(&temp);
    let user = temp.path().join("user.toml");
    write(&user, "version = 2\n");
    write(
        &inputs.repository_root.as_ref().unwrap().join(".tg.toml"),
        "version = 1\n",
    );
    inputs.cli.config_path = Some(user.clone());
    let error = load(&inputs).unwrap_err().to_string();
    assert!(error.contains(&user.display().to_string()), "{error}");
    assert!(
        error.contains("unsupported configuration version"),
        "{error}"
    );
    write(&user, "[providers.github]\nmystery = true\n");
    let error = load(&inputs).unwrap_err().to_string();
    assert!(error.contains("providers.github.mystery"), "{error}");
}

#[test]
fn final_validation_reports_the_source_that_set_the_value() {
    let temp = tempfile::tempdir().unwrap();
    let mut inputs = inputs(&temp);
    let user = temp.path().join("user.toml");
    write(&user, "[search]\nlimit = 0\n");
    inputs.cli.config_path = Some(user.clone());
    let error = load(&inputs).unwrap_err().to_string();
    assert!(error.contains(&user.display().to_string()), "{error}");
    assert!(error.contains("search.limit"), "{error}");
    write(&user, "");
    inputs
        .environment
        .insert("TG_TOKENIZER".into(), "unknown".into());
    let error = load(&inputs).unwrap_err().to_string();
    assert!(
        error.starts_with("environment: tokens.tokenizer:"),
        "{error}"
    );
}

#[test]
fn project_cannot_select_commands_or_prompt_policy() {
    let temp = tempfile::tempdir().unwrap();
    let inputs = inputs(&temp);
    let project = inputs.repository_root.as_ref().unwrap().join(".tg.toml");
    write(&project, "[providers.github]\ncommand = \"evil\"\n");
    assert!(
        load(&inputs)
            .unwrap_err()
            .to_string()
            .contains("providers.github.command")
    );
    write(&project, "[skills]\nmention = \"evil ${name}\"\n");
    assert!(
        load(&inputs)
            .unwrap_err()
            .to_string()
            .contains("skills.mention")
    );
}

#[test]
fn project_skill_roots_must_remain_contained() {
    let temp = tempfile::tempdir().unwrap();
    let inputs = inputs(&temp);
    let repo = inputs.repository_root.as_ref().unwrap();
    let project = repo.join(".tg.toml");
    write(&project, "[[skills.roots]]\npath = \"../outside\"\n");
    assert!(load(&inputs).unwrap_err().to_string().contains("contained"));
    write(
        &project,
        "[[skills.roots]]\npath = \"skills\"\ncontained = false\n",
    );
    assert!(
        load(&inputs)
            .unwrap_err()
            .to_string()
            .contains("skills.roots[0].contained")
    );
    fs::create_dir_all(repo.join(".agents/skills")).unwrap();
    write(&project, "[[skills.roots]]\npath = \".agents/skills\"\n");
    let root = &load(&inputs).unwrap().config.skills.roots[0];
    assert_eq!(root.path, repo.join(".agents/skills"));
    assert_eq!(root.scope, "repository");
    assert!(root.contained);
    assert!(!root.walk_ancestors);
}

#[cfg(unix)]
#[test]
fn project_skill_root_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let inputs = inputs(&temp);
    let repo = inputs.repository_root.as_ref().unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, repo.join("linked")).unwrap();
    write(
        &repo.join(".tg.toml"),
        "[[skills.roots]]\npath = \"linked\"\n",
    );
    assert!(load(&inputs).unwrap_err().to_string().contains("outside"));
    write(
        &repo.join(".tg.toml"),
        "[[skills.roots]]\npath = \"linked/not-created-yet\"\n",
    );
    assert!(load(&inputs).unwrap_err().to_string().contains("outside"));
}

#[test]
fn project_uses_an_explicit_safe_leaf_allowlist() {
    let temp = tempfile::tempdir().unwrap();
    let inputs = inputs(&temp);
    let project = inputs.repository_root.as_ref().unwrap().join(".tg.toml");
    for (text, key) in [
        ("[editor]\ncopy_command=':evil'\n", "editor"),
        ("[ui]\npreview_toggle='ctrl-x'\n", "ui.preview_toggle"),
        ("[tokens]\ndecimals=2\n", "tokens.decimals"),
        (
            "[providers.github]\nenabled=false\n",
            "providers.github.enabled",
        ),
        ("[providers.jira]\nlimit=2\n", "providers.jira.limit"),
    ] {
        write(&project, text);
        let error = load(&inputs).unwrap_err().to_string();
        assert!(error.contains(key), "{error}");
    }
    write(
        &project,
        "[leaders]\nfiles='@@'\n[ui]\npreview='disabled'\n[search]\nlimit=2\n[tokens]\ntokenizer='gpt-4o'\n[providers.jira]\nkey_prefix='g5'\n",
    );
    load(&inputs).unwrap();
}

#[test]
fn type_errors_and_leader_collisions_name_the_setting_source() {
    let temp = tempfile::tempdir().unwrap();
    let inputs = inputs(&temp);
    let project = inputs.repository_root.as_ref().unwrap().join(".tg.toml");
    write(&project, "[search]\nlimit='many'\n");
    let error = load(&inputs).unwrap_err().to_string();
    assert!(error.contains(&project.display().to_string()), "{error}");
    assert!(error.contains("search.limit"), "{error}");
    write(&project, "[leaders]\nsymbols='@'\n");
    let error = load(&inputs).unwrap_err().to_string();
    assert!(error.contains(&project.display().to_string()), "{error}");
    assert!(error.contains("leaders.symbols"), "{error}");
}

#[test]
fn project_can_be_disabled_and_environment_values_are_validated() {
    let temp = tempfile::tempdir().unwrap();
    let mut inputs = inputs(&temp);
    write(
        &inputs.repository_root.as_ref().unwrap().join(".tg.toml"),
        "[search]\nlimit = 4\n",
    );
    inputs
        .environment
        .insert("TG_NO_PROJECT_CONFIG".into(), "true".into());
    assert_eq!(load(&inputs).unwrap().config.search.limit, 100);
    inputs
        .environment
        .insert("TG_NO_PROJECT_CONFIG".into(), "maybe".into());
    assert!(
        load(&inputs)
            .unwrap_err()
            .to_string()
            .contains("TG_NO_PROJECT_CONFIG")
    );
}

#[test]
fn trusted_roots_expand_home_and_arrays_replace() {
    let temp = tempfile::tempdir().unwrap();
    let mut inputs = inputs(&temp);
    let user = temp.path().join("user.toml");
    write(
        &user,
        "[[skills.roots]]\npath = \"~/skills\"\n[[skills.roots]]\npath = \"relative\"\n",
    );
    inputs.cli.config_path = Some(user.clone());
    let loaded = load(&inputs).unwrap();
    assert_eq!(
        loaded.config.skills.roots[0].path,
        inputs.home_dir.unwrap().join("skills")
    );
    assert_eq!(
        loaded.config.skills.roots[1].path,
        inputs.repository_root.unwrap().join("relative")
    );
}

#[test]
fn final_normalization_applies_after_all_layers() {
    let temp = tempfile::tempdir().unwrap();
    let mut inputs = inputs(&temp);
    let user = temp.path().join("user.toml");
    write(&user, "[providers.jira]\nkey_prefix = \"g5-\"\n");
    inputs.cli.config_path = Some(user);
    assert_eq!(
        load(&inputs)
            .unwrap()
            .config
            .providers
            .jira
            .key_prefix
            .as_deref(),
        Some("G5")
    );
}
