use crate::config::{SkillDiscovery, SkillRootConfig};
use anyhow::Result;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn codex_root(path: PathBuf, scope: &str, discovery: SkillDiscovery) -> SkillRootConfig {
    SkillRootConfig {
        path,
        scope: scope.into(),
        discovery,
        walk_ancestors: false,
        contained: false,
        metadata: "SKILL.md".into(),
        name_key: "name".into(),
        description_key: "description".into(),
        mention: None,
    }
}

pub(super) fn collect_named_files(
    directory: &Path,
    name: &str,
    prune_match: bool,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    let Ok(canonical) = directory.canonicalize() else {
        return;
    };
    if !visited.insert(canonical) {
        return;
    }
    if prune_match {
        let metadata = directory.join(name);
        if metadata.is_file() {
            out.push(metadata);
            return;
        }
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !prune_match && path.file_name().is_some_and(|file| file == name) {
            out.push(path);
        } else if entry
            .file_type()
            .is_ok_and(|kind| kind.is_dir() || kind.is_symlink())
        {
            collect_named_files(&path, name, prune_match, visited, out);
        }
    }
}

pub(super) fn ancestors_through(path: &Path, root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for ancestor in path.ancestors() {
        result.push(ancestor.to_path_buf());
        if ancestor == root {
            break;
        }
    }
    result.reverse();
    result
}

pub(super) fn configured_paths(
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

pub(super) fn plugin_skill_roots(codex_home: &Path, diagnostics: &mut Vec<String>) -> Vec<PathBuf> {
    let mut manifests = Vec::new();
    collect_named_files(
        &codex_home.join("plugins"),
        "plugin.json",
        false,
        &mut HashSet::new(),
        &mut manifests,
    );
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

pub(super) fn read_disabled_paths(path: &Path, home: Option<&Path>) -> Result<Vec<PathBuf>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let source = fs::read_to_string(path)?;
    let value: toml::Value = toml::from_str(&source)?;
    let rules = value
        .get("skills")
        .and_then(|skills| skills.get("config"))
        .and_then(toml::Value::as_array);
    let base = path.parent().unwrap_or(Path::new("."));
    Ok(rules
        .into_iter()
        .flatten()
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
