use super::*;
use std::path::Component;

pub(super) fn validate_project_fields(
    value: &toml::Value,
    config_path: &Path,
    repository_root: &Path,
) -> Result<(), ConfigLoadError> {
    let Some(root) = value.as_table() else {
        return Ok(());
    };
    for key in root.keys() {
        if !matches!(
            key.as_str(),
            "version" | "ui" | "leaders" | "search" | "tokens" | "skills" | "providers"
        ) {
            return Err(forbidden(config_path, key.clone()));
        }
    }
    if let Some(ui) = root.get("ui").and_then(toml::Value::as_table)
        && ui.contains_key("preview_toggle")
    {
        return Err(forbidden(config_path, "ui.preview_toggle".into()));
    }
    if let Some(tokens) = root.get("tokens").and_then(toml::Value::as_table)
        && let Some(key) = tokens.keys().find(|key| key.as_str() != "tokenizer")
    {
        return Err(forbidden(config_path, format!("tokens.{key}")));
    }
    if let Some(skills) = root.get("skills").and_then(toml::Value::as_table) {
        for key in skills.keys() {
            if key != "roots" {
                return Err(forbidden(config_path, format!("skills.{key}")));
            }
        }
        if let Some(roots) = skills.get("roots").and_then(toml::Value::as_array) {
            for (index, root) in roots.iter().enumerate() {
                if let Some(table) = root.as_table() {
                    for forbidden_key in ["mention"] {
                        if table.contains_key(forbidden_key) {
                            return Err(forbidden(
                                config_path,
                                format!("skills.roots[{index}].{forbidden_key}"),
                            ));
                        }
                    }
                    if table.get("walk_ancestors").and_then(toml::Value::as_bool) == Some(true) {
                        return Err(forbidden(
                            config_path,
                            format!("skills.roots[{index}].walk_ancestors"),
                        ));
                    }
                    if table.get("contained").and_then(toml::Value::as_bool) == Some(false) {
                        return Err(forbidden(
                            config_path,
                            format!("skills.roots[{index}].contained"),
                        ));
                    }
                }
                if let Some(path) = root.get("path").and_then(toml::Value::as_str) {
                    validate_project_root(path, repository_root).map_err(|message| {
                        ConfigLoadError::new(
                            Some(SourceKind::Project),
                            Some(config_path.to_path_buf()),
                            message,
                        )
                        .key(format!("skills.roots[{index}].path"))
                    })?;
                }
            }
        }
    }
    if let Some(providers) = root.get("providers").and_then(toml::Value::as_table) {
        for (provider_name, provider) in providers {
            if let Some(table) = provider.as_table() {
                let allowed: &[&str] = if provider_name == "jira" {
                    &["key_prefix"]
                } else {
                    &[]
                };
                if let Some(key) = table.keys().find(|key| !allowed.contains(&key.as_str())) {
                    return Err(forbidden(
                        config_path,
                        format!("providers.{provider_name}.{key}"),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn forbidden(path: &Path, key: String) -> ConfigLoadError {
    ConfigLoadError::new(
        Some(SourceKind::Project),
        Some(path.to_path_buf()),
        "is not allowed in project configuration",
    )
    .key(key)
}

fn validate_project_root(path: &str, repository_root: &Path) -> Result<(), String> {
    let path = Path::new(path);
    if path.is_absolute()
        || path.starts_with("~")
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("must be a relative path contained in the repository".into());
    }
    let candidate = repository_root.join(path);
    let canonical_root = repository_root
        .canonicalize()
        .map_err(|error| format!("cannot resolve repository root: {error}"))?;
    let mut existing = candidate.as_path();
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| "cannot resolve a containing directory".to_owned())?;
    }
    let canonical_existing = existing
        .canonicalize()
        .map_err(|error| format!("cannot resolve skill-root ancestor: {error}"))?;
    if !canonical_existing.starts_with(canonical_root) {
        return Err("resolves outside the repository".into());
    }
    Ok(())
}

pub(super) fn resolve_project_skill_roots(
    config: &mut Config,
    repository_root: &Path,
    config_path: &Path,
) -> Result<(), ConfigLoadError> {
    for (index, root) in config.skills.roots.iter_mut().enumerate() {
        validate_project_root(&root.path.to_string_lossy(), repository_root).map_err(
            |message| {
                ConfigLoadError::new(
                    Some(SourceKind::Project),
                    Some(config_path.to_path_buf()),
                    message,
                )
                .key(format!("skills.roots[{index}].path"))
            },
        )?;
        root.path = repository_root.join(&root.path);
        root.scope = "repository".into();
        root.contained = true;
        root.walk_ancestors = false;
    }
    Ok(())
}
