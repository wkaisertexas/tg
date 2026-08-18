use super::model::{
    CandidateDisplay, CandidateId, ContextCost, Preview, PreviewLine, QueryRequest, QueryScope,
    ReferenceCandidate, ReferenceKind, ReferenceTarget, SkillTarget, ValidatedTarget,
};
use super::{CancellationFlag, ReferenceProvider};
use crate::config::{SkillDiscovery, SkillRootConfig, SkillsConfig};
use anyhow::{Context, Result, bail, ensure};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use serde_yaml::Value as YamlValue;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SkillDiscoveryEnvironment {
    pub repository_root: PathBuf,
    pub working_directory: PathBuf,
    pub home: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
    pub admin_skills: Option<PathBuf>,
    pub codex_config: Option<PathBuf>,
}

impl SkillDiscoveryEnvironment {
    pub fn local(repository_root: &Path, working_directory: &Path) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let codex_home = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".codex")));
        Self {
            repository_root: repository_root.to_path_buf(),
            working_directory: working_directory.to_path_buf(),
            home,
            codex_config: codex_home.as_ref().map(|root| root.join("config.toml")),
            codex_home,
            admin_skills: Some(PathBuf::from("/etc/codex/skills")),
        }
    }
}

#[derive(Debug, Clone)]
struct SkillRecord {
    name: String,
    description: String,
    display_name: String,
    short_description: String,
    scope: String,
    metadata_path: PathBuf,
    canonical_metadata: PathBuf,
    mention: String,
    name_key: String,
    description_key: String,
}

#[derive(Debug)]
pub struct SkillProvider {
    skills: Vec<SkillRecord>,
    diagnostics: Vec<String>,
}

impl SkillProvider {
    pub fn new(
        repository_root: &Path,
        working_directory: &Path,
        config: &SkillsConfig,
        leader: impl Into<String>,
    ) -> Result<Self> {
        Self::with_environment(
            SkillDiscoveryEnvironment::local(repository_root, working_directory),
            config,
            leader,
        )
    }

    pub fn with_environment(
        environment: SkillDiscoveryEnvironment,
        config: &SkillsConfig,
        leader: impl Into<String>,
    ) -> Result<Self> {
        let repository_root = environment
            .repository_root
            .canonicalize()
            .with_context(|| {
                format!(
                    "cannot read repository root {}",
                    environment.repository_root.display()
                )
            })?;
        let working_directory =
            environment
                .working_directory
                .canonicalize()
                .with_context(|| {
                    format!(
                        "cannot read working directory {}",
                        environment.working_directory.display()
                    )
                })?;
        ensure!(
            working_directory.starts_with(&repository_root),
            "working directory is outside repository root"
        );
        let leader = leader.into();
        let disabled = if config.read_codex_disable_rules {
            environment
                .codex_config
                .as_deref()
                .map(|path| read_disabled_paths(path, environment.home.as_deref()))
                .transpose()?
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut loader = Loader {
            home: environment.home.as_deref(),
            leader: &leader,
            default_mention: &config.mention,
            disabled,
            seen: HashSet::new(),
            skills: Vec::new(),
            diagnostics: Vec::new(),
        };

        if config.profile == "codex-local" {
            for ancestor in ancestors_through(&working_directory, &repository_root) {
                loader.scan_root(&RootSpec::codex(
                    ancestor.join(".agents/skills"),
                    "repository",
                    SkillDiscovery::DirectChildren,
                ));
            }
            if let Some(home) = environment.home.as_deref() {
                loader.scan_root(&RootSpec::codex(
                    home.join(".agents/skills"),
                    "user",
                    SkillDiscovery::DirectChildren,
                ));
            }
            if let Some(codex_home) = environment.codex_home.as_deref() {
                loader.scan_root(&RootSpec::codex(
                    codex_home.join("skills"),
                    "codex",
                    SkillDiscovery::Recursive,
                ));
            }
            if let Some(admin) = environment.admin_skills {
                loader.scan_root(&RootSpec::codex(
                    admin,
                    "admin",
                    SkillDiscovery::DirectChildren,
                ));
            }
            if let Some(codex_home) = environment.codex_home.as_deref() {
                for root in plugin_skill_roots(codex_home, &mut loader.diagnostics) {
                    loader.scan_root(&RootSpec::codex(
                        root,
                        "plugin",
                        SkillDiscovery::DirectChildren,
                    ));
                }
            }
        }

        for configured in &config.roots {
            for path in configured_paths(
                configured,
                &repository_root,
                &working_directory,
                loader.home,
            ) {
                loader.scan_root(&RootSpec {
                    path,
                    scope: configured.scope.clone(),
                    discovery: configured.discovery,
                    contained: configured.contained,
                    metadata: configured.metadata.clone(),
                    name_key: configured.name_key.clone(),
                    description_key: configured.description_key.clone(),
                    mention: configured
                        .mention
                        .clone()
                        .unwrap_or_else(|| config.mention.clone()),
                });
            }
        }

        loader.skills.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
                .then_with(|| left.scope.cmp(&right.scope))
                .then_with(|| left.metadata_path.cmp(&right.metadata_path))
        });
        Ok(Self {
            skills: loader.skills,
            diagnostics: loader.diagnostics,
        })
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    fn record_by_id(&self, id: &CandidateId) -> Result<&SkillRecord> {
        ensure!(
            id.provider == ReferenceKind::Skill,
            "candidate belongs to another provider"
        );
        self.skills
            .iter()
            .find(|skill| opaque_id(skill) == id.opaque)
            .context("skill candidate is no longer available")
    }
}

impl ReferenceProvider for SkillProvider {
    fn kind(&self) -> ReferenceKind {
        ReferenceKind::Skill
    }

    fn query(
        &self,
        request: QueryRequest,
        cancellation: &CancellationFlag,
    ) -> Result<Vec<ReferenceCandidate>> {
        ensure!(
            request.scope == QueryScope::Repository,
            "skill provider only supports repository queries"
        );
        let matcher = SkimMatcherV2::default().ignore_case();
        let query = request.query.to_ascii_lowercase();
        let mut found = self
            .skills
            .iter()
            .filter_map(|skill| {
                if cancellation.is_cancelled() {
                    return None;
                }
                let name = skill.name.to_ascii_lowercase();
                let score = if query.is_empty() {
                    0
                } else if name == query {
                    1_000_000
                } else if name.starts_with(&query) {
                    500_000
                } else {
                    matcher
                        .fuzzy_match(&skill.name, &request.query)
                        .or_else(|| matcher.fuzzy_match(&skill.display_name, &request.query))
                        .or_else(|| matcher.fuzzy_match(&skill.description, &request.query))?
                };
                Some((score, skill))
            })
            .collect::<Vec<_>>();
        found.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.scope.cmp(&right.scope))
                .then_with(|| left.metadata_path.cmp(&right.metadata_path))
        });
        Ok(found
            .into_iter()
            .take(request.limit)
            .map(|(_, skill)| ReferenceCandidate {
                id: CandidateId {
                    provider: ReferenceKind::Skill,
                    opaque: opaque_id(skill),
                },
                generation: request.generation,
                kind: ReferenceKind::Skill,
                friendly_text: skill.mention.clone(),
                display: CandidateDisplay {
                    primary: skill.display_name.clone(),
                    secondary: Some(format!(
                        "{} · {} · {}",
                        skill.short_description,
                        skill.scope,
                        skill.metadata_path.display()
                    )),
                    ..CandidateDisplay::default()
                },
                context_cost: ContextCost::None,
                file_context_cost: None,
                source_version: None,
                token_source: None,
            })
            .collect())
    }

    fn resolve(&self, id: &CandidateId) -> Result<ReferenceTarget> {
        let skill = self.record_by_id(id)?;
        Ok(ReferenceTarget::Skill(SkillTarget {
            name: skill.name.clone(),
            mention: skill.mention.clone(),
            source_path: skill.metadata_path.clone(),
        }))
    }

    fn validate(&self, target: &ReferenceTarget) -> Result<ValidatedTarget> {
        let ReferenceTarget::Skill(target) = target else {
            bail!("skill provider cannot validate this target")
        };
        let canonical = target
            .source_path
            .canonicalize()
            .context("skill metadata no longer exists")?;
        let skill = self
            .skills
            .iter()
            .find(|skill| skill.canonical_metadata == canonical)
            .context("skill is no longer available")?;
        let current = parse_frontmatter(
            &fs::read_to_string(&canonical)?,
            &skill.name_key,
            &skill.description_key,
        )?;
        ensure!(current.0 == skill.name, "skill name changed");
        Ok(ValidatedTarget {
            target: ReferenceTarget::Skill(SkillTarget {
                name: skill.name.clone(),
                mention: skill.mention.clone(),
                source_path: skill.metadata_path.clone(),
            }),
            context_cost: ContextCost::None,
        })
    }

    fn lower(&self, target: &ValidatedTarget) -> Result<String> {
        let ReferenceTarget::Skill(skill) = &target.target else {
            bail!("skill provider cannot lower this target")
        };
        Ok(skill.mention.clone())
    }

    fn preview(&self, target: &ReferenceTarget) -> Result<Option<Preview>> {
        let ReferenceTarget::Skill(target) = target else {
            bail!("skill provider cannot preview this target")
        };
        let canonical = target.source_path.canonicalize()?;
        let skill = self
            .skills
            .iter()
            .find(|skill| skill.canonical_metadata == canonical)
            .context("skill is no longer available")?;
        Ok(Some(Preview {
            title: Some(skill.display_name.clone()),
            lines: [
                format!("Name: {}", skill.name),
                format!("Description: {}", skill.short_description),
                format!("Scope: {}", skill.scope),
                format!("Source: {}", skill.metadata_path.display()),
            ]
            .into_iter()
            .map(|text| PreviewLine { number: None, text })
            .collect(),
            highlighted_lines: None,
        }))
    }

    fn context_cost(&self, _target: &ReferenceTarget) -> Result<ContextCost> {
        Ok(ContextCost::None)
    }
}

fn opaque_id(skill: &SkillRecord) -> String {
    skill.canonical_metadata.to_string_lossy().into_owned()
}

struct Loader<'a> {
    home: Option<&'a Path>,
    leader: &'a str,
    default_mention: &'a str,
    disabled: Vec<PathBuf>,
    seen: HashSet<PathBuf>,
    skills: Vec<SkillRecord>,
    diagnostics: Vec<String>,
}

#[derive(Debug)]
struct RootSpec {
    path: PathBuf,
    scope: String,
    discovery: SkillDiscovery,
    contained: bool,
    metadata: String,
    name_key: String,
    description_key: String,
    mention: String,
}

impl RootSpec {
    fn codex(path: PathBuf, scope: &str, discovery: SkillDiscovery) -> Self {
        Self {
            path,
            scope: scope.into(),
            discovery,
            contained: false,
            metadata: "SKILL.md".into(),
            name_key: "name".into(),
            description_key: "description".into(),
            mention: String::new(),
        }
    }
}

impl Loader<'_> {
    fn scan_root(&mut self, spec: &RootSpec) {
        if !spec.path.exists() {
            return;
        }
        let canonical_root = match spec.path.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                self.diagnostics.push(format!(
                    "cannot read skill root {}: {error}",
                    spec.path.display()
                ));
                return;
            }
        };
        let mut metadata = Vec::new();
        match spec.discovery {
            SkillDiscovery::DirectChildren => {
                let Ok(entries) = fs::read_dir(&spec.path) else {
                    return;
                };
                for entry in entries.flatten() {
                    let path = entry.path().join(&spec.metadata);
                    if path.is_file() {
                        metadata.push(path);
                    }
                }
            }
            SkillDiscovery::Recursive => collect_metadata(
                &spec.path,
                &spec.metadata,
                &mut HashSet::new(),
                &mut metadata,
            ),
        }
        metadata.sort();
        for path in metadata {
            let canonical = match path.canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    self.diagnostics
                        .push(format!("cannot resolve skill {}: {error}", path.display()));
                    continue;
                }
            };
            if spec.contained && !canonical.starts_with(&canonical_root) {
                self.diagnostics
                    .push(format!("skill escapes contained root: {}", path.display()));
                continue;
            }
            if !self.seen.insert(canonical.clone()) {
                continue;
            }
            if self.disabled.iter().any(|disabled| {
                canonical == *disabled
                    || canonical.parent().is_some_and(|directory| {
                        directory == disabled || directory.starts_with(disabled)
                    })
            }) {
                continue;
            }
            let source = match fs::read_to_string(&canonical) {
                Ok(source) => source,
                Err(error) => {
                    self.diagnostics
                        .push(format!("cannot read skill {}: {error}", path.display()));
                    continue;
                }
            };
            let (name, description) =
                match parse_frontmatter(&source, &spec.name_key, &spec.description_key) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        self.diagnostics
                            .push(format!("invalid skill {}: {error}", path.display()));
                        continue;
                    }
                };
            let (display_name, short_description) = interface_metadata(&canonical)
                .unwrap_or_else(|| (name.clone(), description.clone()));
            let mention = render_mention(
                if spec.mention.is_empty() {
                    self.default_mention
                } else {
                    &spec.mention
                },
                self.leader,
                &name,
            );
            self.skills.push(SkillRecord {
                name,
                description,
                display_name,
                short_description,
                scope: spec.scope.clone(),
                metadata_path: path,
                canonical_metadata: canonical,
                mention,
                name_key: spec.name_key.clone(),
                description_key: spec.description_key.clone(),
            });
        }
    }
}

fn parse_frontmatter(
    source: &str,
    name_key: &str,
    description_key: &str,
) -> Result<(String, String)> {
    let mut lines = source.lines();
    ensure!(lines.next() == Some("---"), "missing YAML frontmatter");
    let mut yaml = String::new();
    let mut closed = false;
    for line in lines {
        if line == "---" {
            closed = true;
            break;
        }
        yaml.push_str(line);
        yaml.push('\n');
    }
    ensure!(closed, "unterminated YAML frontmatter");
    let value: YamlValue = serde_yaml::from_str(&yaml)?;
    let name = yaml_string(&value, name_key).context("missing skill name")?;
    let description = yaml_string(&value, description_key).context("missing skill description")?;
    ensure!(!name.trim().is_empty(), "skill name is empty");
    ensure!(!description.trim().is_empty(), "skill description is empty");
    Ok((name, description))
}

fn yaml_string(value: &YamlValue, key: &str) -> Option<String> {
    let value = key.split('.').try_fold(value, |value, segment| {
        value.as_mapping()?.get(YamlValue::String(segment.into()))
    })?;
    value.as_str().map(str::to_owned)
}

fn interface_metadata(metadata: &Path) -> Option<(String, String)> {
    let path = metadata.parent()?.join("agents/openai.yaml");
    let value: YamlValue = serde_yaml::from_str(&fs::read_to_string(path).ok()?).ok()?;
    Some((
        yaml_string(&value, "interface.display_name")?,
        yaml_string(&value, "interface.short_description")?,
    ))
}

fn render_mention(template: &str, leader: &str, name: &str) -> String {
    template
        .replace("$${", "\0{")
        .replace("${leader}", leader)
        .replace("${name}", name)
        .replace("\0{", "${")
}

fn collect_metadata(
    directory: &Path,
    metadata_name: &str,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    let Ok(canonical) = directory.canonicalize() else {
        return;
    };
    if !visited.insert(canonical) {
        return;
    }
    let metadata = directory.join(metadata_name);
    if metadata.is_file() {
        out.push(metadata);
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_type()
            .is_ok_and(|kind| kind.is_dir() || kind.is_symlink())
        {
            collect_metadata(&entry.path(), metadata_name, visited, out);
        }
    }
}

fn ancestors_through(path: &Path, root: &Path) -> Vec<PathBuf> {
    let mut current = Some(path);
    let mut result = Vec::new();
    while let Some(path) = current {
        result.push(path.to_path_buf());
        if path == root {
            break;
        }
        current = path.parent();
    }
    result.reverse();
    result
}

fn configured_paths(
    config: &SkillRootConfig,
    repository_root: &Path,
    working_directory: &Path,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let raw = expand_home(&config.path, home);
    if raw.is_absolute() {
        return vec![raw];
    }
    if config.walk_ancestors {
        ancestors_through(working_directory, repository_root)
            .into_iter()
            .map(|ancestor| ancestor.join(&raw))
            .collect()
    } else {
        vec![repository_root.join(raw)]
    }
}

fn expand_home(path: &Path, home: Option<&Path>) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        return home.unwrap_or(path).to_path_buf();
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return home.map_or_else(|| path.to_path_buf(), |home| home.join(rest));
    }
    path.to_path_buf()
}

fn plugin_skill_roots(codex_home: &Path, diagnostics: &mut Vec<String>) -> Vec<PathBuf> {
    let plugins = codex_home.join("plugins");
    let mut manifests = Vec::new();
    collect_named_files(&plugins, "plugin.json", &mut HashSet::new(), &mut manifests);
    let mut roots = Vec::new();
    for manifest in manifests {
        let Ok(source) = fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&source) else {
            diagnostics.push(format!("invalid plugin manifest {}", manifest.display()));
            continue;
        };
        let Some(skills) = value.get("skills") else {
            continue;
        };
        let values: Vec<_> = match skills {
            serde_json::Value::String(path) => vec![path.as_str()],
            serde_json::Value::Array(paths) => {
                paths.iter().filter_map(|path| path.as_str()).collect()
            }
            _ => Vec::new(),
        };
        let base = manifest
            .parent()
            .and_then(Path::parent)
            .unwrap_or(&manifest);
        roots.extend(values.into_iter().map(|path| base.join(path)));
    }
    roots
}

fn collect_named_files(
    directory: &Path,
    name: &str,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    let Ok(canonical) = directory.canonicalize() else {
        return;
    };
    if !visited.insert(canonical) {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|file| file == name) {
            out.push(path);
        } else if entry
            .file_type()
            .is_ok_and(|kind| kind.is_dir() || kind.is_symlink())
        {
            collect_named_files(&path, name, visited, out);
        }
    }
}

fn read_disabled_paths(path: &Path, home: Option<&Path>) -> Result<Vec<PathBuf>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let source = fs::read_to_string(path)?;
    let value: toml::Value = toml::from_str(&source)?;
    let rules = value
        .get("skills")
        .and_then(|skills| skills.get("config"))
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let base = path.parent().unwrap_or(Path::new("."));
    Ok(rules
        .iter()
        .filter(|rule| rule.get("enabled").and_then(toml::Value::as_bool) == Some(false))
        .filter_map(|rule| rule.get("path").and_then(toml::Value::as_str))
        .map(|path| {
            let path = expand_home(Path::new(path), home);
            let path = if path.is_absolute() {
                path
            } else {
                base.join(path)
            };
            path.canonicalize().unwrap_or(path)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::model::{GenerationId, ReferenceTarget};
    use std::fs;

    fn write_skill(root: &Path, directory: &str, name: &str, description: &str) -> PathBuf {
        let directory = root.join(directory);
        fs::create_dir_all(&directory).unwrap();
        let metadata = directory.join("SKILL.md");
        fs::write(
            &metadata,
            format!("---\nname: {name}\ndescription: {description}\n---\n# ignored body\n"),
        )
        .unwrap();
        metadata
    }

    fn environment(temp: &Path) -> SkillDiscoveryEnvironment {
        let repository = temp.join("repo");
        let working = repository.join("nested/work");
        fs::create_dir_all(&working).unwrap();
        fs::create_dir_all(temp.join("home")).unwrap();
        fs::create_dir_all(temp.join("codex")).unwrap();
        fs::create_dir_all(temp.join("admin")).unwrap();
        SkillDiscoveryEnvironment {
            repository_root: repository,
            working_directory: working,
            home: Some(temp.join("home")),
            codex_home: Some(temp.join("codex")),
            admin_skills: Some(temp.join("admin")),
            codex_config: Some(temp.join("codex/config.toml")),
        }
    }

    fn request(query: &str) -> QueryRequest {
        QueryRequest {
            generation: GenerationId(9),
            query: query.into(),
            scope: QueryScope::Repository,
            limit: 100,
            typed_leader: "$".into(),
        }
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

        let provider =
            SkillProvider::with_environment(env, &SkillsConfig::default(), "$ ".trim()).unwrap();
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

        let provider =
            SkillProvider::with_environment(env, &SkillsConfig::default(), "$ ".trim()).unwrap();
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
        let provider = SkillProvider::with_environment(env, &config, "$ ".trim()).unwrap();
        assert_eq!(provider.len(), 1);
        assert!(!provider.diagnostics().is_empty());
    }

    #[test]
    fn malformed_skills_are_diagnostic_and_provider_contract_is_stable() {
        let temp = tempfile::tempdir().unwrap();
        let env = environment(temp.path());
        let root = env.home.as_ref().unwrap().join(".agents/skills");
        let valid = write_skill(&root, "valid", "valid", "useful description");
        fs::create_dir_all(root.join("bad")).unwrap();
        fs::write(root.join("bad/SKILL.md"), "name: no-frontmatter").unwrap();
        let provider =
            SkillProvider::with_environment(env, &SkillsConfig::default(), "$ ".trim()).unwrap();
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
}
