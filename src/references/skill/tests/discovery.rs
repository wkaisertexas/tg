use super::super::roots::collect_named_files;
use super::*;
use std::collections::HashSet;

#[test]
fn discovery_walkers_keep_their_distinct_pruning_rules() {
    let temp = tempfile::tempdir().unwrap();
    let outer = temp.path().join("outer");
    let inner = outer.join("inner");
    fs::create_dir_all(&inner).unwrap();
    for directory in [&outer, &inner] {
        fs::write(directory.join("SKILL.md"), "fixture").unwrap();
        fs::write(directory.join("plugin.json"), "{}").unwrap();
    }
    let mut skills = Vec::new();
    collect_named_files(
        temp.path(),
        "SKILL.md",
        true,
        &mut HashSet::new(),
        &mut skills,
    );
    assert_eq!(skills, [outer.join("SKILL.md")]);
    let mut manifests = Vec::new();
    collect_named_files(
        temp.path(),
        "plugin.json",
        false,
        &mut HashSet::new(),
        &mut manifests,
    );
    manifests.sort();
    let mut expected = vec![outer.join("plugin.json"), inner.join("plugin.json")];
    expected.sort();
    assert_eq!(manifests, expected);
}

#[cfg(unix)]
#[test]
fn discovery_walkers_deduplicate_aliases_and_stop_cycles_without_skill_boundaries() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let shared = temp.path().join("shared");
    fs::create_dir(&shared).unwrap();
    fs::write(shared.join("SKILL.md"), "fixture").unwrap();
    fs::write(shared.join("plugin.json"), "{}").unwrap();
    symlink(temp.path(), temp.path().join("cycle")).unwrap();
    symlink(&shared, temp.path().join("alias")).unwrap();
    let mut skills = Vec::new();
    collect_named_files(
        temp.path(),
        "SKILL.md",
        true,
        &mut HashSet::new(),
        &mut skills,
    );
    assert_eq!(skills.len(), 1);
    assert_eq!(
        skills[0].canonicalize().unwrap(),
        shared.join("SKILL.md").canonicalize().unwrap()
    );
    let mut manifests = Vec::new();
    collect_named_files(
        temp.path(),
        "plugin.json",
        false,
        &mut HashSet::new(),
        &mut manifests,
    );
    assert_eq!(manifests.len(), 1);
    assert_eq!(
        manifests[0].canonicalize().unwrap(),
        shared.join("plugin.json").canonicalize().unwrap()
    );
}

#[test]
fn codex_profile_discovers_all_local_scopes_plugins_system_and_duplicates() {
    let temp = tempfile::tempdir().unwrap();
    let env = environment(temp.path());
    write_skill(
        &env.repository_root.join(".agents/skills"),
        "shared",
        "shared",
        "repository copy",
    );
    write_skill(
        &env.working_directory.join(".agents/skills"),
        "nested",
        "nested",
        "nearest repository skill",
    );
    write_skill(
        &env.home.as_ref().unwrap().join(".agents/skills"),
        "shared",
        "shared",
        "user copy",
    );
    write_skill(
        &env.codex_home.as_ref().unwrap().join("skills"),
        "ordinary",
        "ordinary",
        "codex skill",
    );
    write_skill(
        &env.codex_home.as_ref().unwrap().join("skills/.system"),
        "creator",
        "creator",
        "bundled skill",
    );
    write_skill(
        env.admin_skills.as_ref().unwrap(),
        "admin",
        "admin",
        "admin skill",
    );
    let plugin = env.codex_home.as_ref().unwrap().join("plugins/example");
    fs::create_dir_all(plugin.join(".codex-plugin")).unwrap();
    fs::write(
        plugin.join(".codex-plugin/plugin.json"),
        r#"{"name":"example","skills":"./skills"}"#,
    )
    .unwrap();
    write_skill(
        &plugin.join("skills"),
        "plugin-skill",
        "plugin-skill",
        "plugin skill",
    );
    let provider = SkillProvider::with_environment(env, &SkillsConfig::default(), "$").unwrap();
    assert_eq!(provider.len(), 7);
    let results = provider
        .query(request(""), &CancellationFlag::default())
        .unwrap();
    assert_eq!(
        results
            .iter()
            .filter(|item| item.friendly_text == "$shared")
            .count(),
        2
    );
    let secondary = results
        .iter()
        .map(|item| item.display.secondary.as_deref().unwrap())
        .collect::<Vec<_>>();
    assert!(secondary.iter().any(|text| text.contains("repository")));
    assert!(secondary.iter().any(|text| text.contains("plugin")));
    assert!(secondary.iter().any(|text| text.contains("admin")));
}

#[test]
fn disable_rules_and_interface_metadata_are_applied() {
    let temp = tempfile::tempdir().unwrap();
    let env = environment(temp.path());
    let disabled = write_skill(
        &env.codex_home.as_ref().unwrap().join("skills"),
        "disabled",
        "disabled",
        "do not show",
    );
    let visible = write_skill(
        &env.codex_home.as_ref().unwrap().join("skills"),
        "visible",
        "visible",
        "frontmatter description",
    );
    fs::create_dir_all(visible.parent().unwrap().join("agents")).unwrap();
    fs::write(
        visible.parent().unwrap().join("agents/openai.yaml"),
        "interface:\n  display_name: Visible Skill\n  short_description: Interface summary\n",
    )
    .unwrap();
    fs::write(
        env.codex_config.as_ref().unwrap(),
        format!(
            "[[skills.config]]\npath = {:?}\nenabled = false\n",
            disabled.parent().unwrap()
        ),
    )
    .unwrap();
    let provider = SkillProvider::with_environment(env, &SkillsConfig::default(), "$").unwrap();
    assert_eq!(provider.len(), 1);
    let result = provider
        .query(request("visible"), &CancellationFlag::default())
        .unwrap()
        .remove(0);
    assert_eq!(result.display.primary, "Visible Skill");
    assert!(
        result
            .display
            .secondary
            .unwrap()
            .contains("Interface summary")
    );
}

#[test]
fn generic_roots_support_ancestor_recursive_custom_keys_and_templates() {
    let temp = tempfile::tempdir().unwrap();
    let env = environment(temp.path());
    let ancestor_root = env.repository_root.join("generic");
    let nested = ancestor_root.join("category/tool");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        nested.join("AGENT.md"),
        "---\ntitle: custom\nsummary: custom description\n---\n",
    )
    .unwrap();
    let mut config = SkillsConfig {
        profile: "generic".into(),
        mention: "${leader}${name}".into(),
        read_codex_disable_rules: false,
        roots: vec![SkillRootConfig {
            path: PathBuf::from("generic"),
            scope: "team".into(),
            discovery: SkillDiscovery::Recursive,
            walk_ancestors: true,
            contained: true,
            metadata: "AGENT.md".into(),
            name_key: "title".into(),
            description_key: "summary".into(),
            mention: Some("use:${name}:${leader}".into()),
        }],
    };
    let provider = SkillProvider::with_environment(env.clone(), &config, "§").unwrap();
    assert_eq!(provider.len(), 1);
    let candidate = provider
        .query(request("custom"), &CancellationFlag::default())
        .unwrap()
        .remove(0);
    assert_eq!(candidate.friendly_text, "use:custom:§");
    config.roots[0].discovery = SkillDiscovery::DirectChildren;
    let direct = SkillProvider::with_environment(env, &config, "§").unwrap();
    assert!(direct.is_empty());
}

#[cfg(unix)]
#[test]
fn contained_roots_reject_escape_but_recursive_symlinks_avoid_cycles() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let env = environment(temp.path());
    let root = temp.path().join("configured");
    let outside = temp.path().join("outside");
    write_skill(&outside, "escaped", "escaped", "outside root");
    fs::create_dir_all(&root).unwrap();
    symlink(outside.join("escaped"), root.join("escaped")).unwrap();
    let local = write_skill(&root, "local", "local", "local skill");
    symlink(&root, local.parent().unwrap().join("cycle")).unwrap();
    let config = SkillsConfig {
        profile: "generic".into(),
        mention: "${leader}${name}".into(),
        read_codex_disable_rules: false,
        roots: vec![SkillRootConfig {
            path: root,
            scope: "safe".into(),
            discovery: SkillDiscovery::Recursive,
            walk_ancestors: false,
            contained: true,
            metadata: "SKILL.md".into(),
            name_key: "name".into(),
            description_key: "description".into(),
            mention: None,
        }],
    };
    let provider = SkillProvider::with_environment(env, &config, "$").unwrap();
    assert_eq!(provider.len(), 1);
    assert!(!provider.diagnostics().is_empty());
}
