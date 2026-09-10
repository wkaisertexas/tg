use super::*;

#[test]
fn malformed_skills_are_diagnostic_and_provider_contract_is_stable() {
    let temp = tempfile::tempdir().unwrap();
    let env = environment(temp.path());
    let root = env.home.as_ref().unwrap().join(".agents/skills");
    let valid = write_skill(&root, "valid", "valid", "useful description");
    fs::create_dir_all(root.join("bad")).unwrap();
    fs::write(root.join("bad/SKILL.md"), "name: no-frontmatter").unwrap();
    let provider = SkillProvider::with_environment(env, &SkillsConfig::default(), "$").unwrap();
    assert_eq!(provider.len(), 1);
    assert_eq!(provider.diagnostics().len(), 1);
    let candidate = provider
        .query(request("val"), &CancellationFlag::default())
        .unwrap()
        .remove(0);
    assert_eq!(candidate.generation, GenerationId(9));
    assert_eq!(candidate.context_cost, ContextCost::None);
    let target = provider.resolve(&candidate.id).unwrap();
    let ReferenceTarget::Skill(skill) = &target else {
        unreachable!()
    };
    assert_eq!(skill.source_path, valid);
    assert_eq!(skill.mention, "$valid");
    let validated = provider.validate(&target).unwrap();
    assert_eq!(provider.lower(&validated).unwrap(), "$valid");
    assert_eq!(provider.context_cost(&target).unwrap(), ContextCost::None);
    let preview = provider.preview(&target).unwrap().unwrap();
    let preview_text = preview
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>();
    assert!(
        preview_text
            .iter()
            .any(|line| line.contains("useful description"))
    );
    assert!(preview_text.iter().any(|line| line.contains("user")));
    fs::write(&valid, "---\nname: changed\ndescription: changed\n---\n").unwrap();
    assert!(provider.validate(&target).is_err());
}

#[test]
fn cancellation_scope_and_escaped_mentions_are_respected() {
    let temp = tempfile::tempdir().unwrap();
    let env = environment(temp.path());
    write_skill(
        &env.home.as_ref().unwrap().join(".agents/skills"),
        "one",
        "one",
        "first",
    );
    let config = SkillsConfig {
        mention: "$${name}:${leader}".into(),
        ..SkillsConfig::default()
    };
    let provider = SkillProvider::with_environment(env, &config, "§").unwrap();
    let cancellation = CancellationFlag::default();
    cancellation.cancel();
    assert!(
        provider
            .query(request(""), &cancellation)
            .unwrap()
            .is_empty()
    );
    let mut wrong = request("");
    wrong.scope = QueryScope::File {
        path: PathBuf::from("x"),
        origin: crate::references::model::FileOrigin::GitAware,
    };
    assert!(provider.query(wrong, &CancellationFlag::default()).is_err());
    let candidate = provider
        .query(request(""), &CancellationFlag::default())
        .unwrap()
        .remove(0);
    assert_eq!(candidate.friendly_text, "${name}:§");
}
