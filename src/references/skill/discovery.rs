use super::metadata::{interface_metadata, parse_frontmatter, render_mention};
use super::roots::{
    ancestors_through, codex_root, collect_named_files, configured_paths, plugin_skill_roots,
    read_disabled_paths,
};
use super::{SkillProvider, SkillRecord};
use crate::config::{SkillDiscovery, SkillRootConfig, SkillsConfig};
use anyhow::{Context, Result, ensure};
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

impl SkillProvider {
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
            leader: &leader,
            default_mention: &config.mention,
            disabled,
            seen: HashSet::new(),
            skills: Vec::new(),
            diagnostics: Vec::new(),
        };
        if config.profile == "codex-local" {
            for ancestor in ancestors_through(&working_directory, &repository_root) {
                loader.scan_root(&codex_root(
                    ancestor.join(".agents/skills"),
                    "repository",
                    SkillDiscovery::DirectChildren,
                ));
            }
            if let Some(home) = environment.home.as_deref() {
                loader.scan_root(&codex_root(
                    home.join(".agents/skills"),
                    "user",
                    SkillDiscovery::DirectChildren,
                ));
            }
            if let Some(codex_home) = environment.codex_home.as_deref() {
                loader.scan_root(&codex_root(
                    codex_home.join("skills"),
                    "codex",
                    SkillDiscovery::Recursive,
                ));
            }
            if let Some(admin) = environment.admin_skills {
                loader.scan_root(&codex_root(admin, "admin", SkillDiscovery::DirectChildren));
            }
            if let Some(codex_home) = environment.codex_home.as_deref() {
                for root in plugin_skill_roots(codex_home, &mut loader.diagnostics) {
                    loader.scan_root(&codex_root(root, "plugin", SkillDiscovery::DirectChildren));
                }
            }
        }
        for configured in &config.roots {
            for path in configured_paths(
                configured,
                &repository_root,
                &working_directory,
                environment.home.as_deref(),
            ) {
                loader.scan_root(&SkillRootConfig {
                    path,
                    ..configured.clone()
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
}

struct Loader<'a> {
    leader: &'a str,
    default_mention: &'a str,
    disabled: Vec<PathBuf>,
    seen: HashSet<PathBuf>,
    skills: Vec<SkillRecord>,
    diagnostics: Vec<String>,
}

impl Loader<'_> {
    fn scan_root(&mut self, spec: &SkillRootConfig) {
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
            SkillDiscovery::Recursive => collect_named_files(
                &spec.path,
                &spec.metadata,
                true,
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
                spec.mention
                    .as_deref()
                    .filter(|mention| !mention.is_empty())
                    .unwrap_or(self.default_mention),
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
