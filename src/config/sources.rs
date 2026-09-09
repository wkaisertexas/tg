mod project;
mod provenance;
#[cfg(test)]
mod tests;

use super::schema::apply_toml_patch;
use super::{ColorMode, Config, PreviewMode};
use project::{resolve_project_skill_roots, validate_project_fields};
use provenance::*;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliConfigOverrides {
    pub config_path: Option<PathBuf>,
    pub no_project_config: bool,
    pub tokenizer: Option<String>,
    pub no_preview: bool,
    pub no_color: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigInputs {
    pub cwd: PathBuf,
    pub home_dir: Option<PathBuf>,
    pub repository_root: Option<PathBuf>,
    pub environment: BTreeMap<String, OsString>,
    pub cli: CliConfigOverrides,
}
impl ConfigInputs {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            home_dir: None,
            repository_root: None,
            environment: BTreeMap::new(),
            cli: CliConfigOverrides::default(),
        }
    }
    fn env(&self, name: &str) -> Option<&OsStr> {
        self.environment.get(name).map(OsString::as_os_str)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    User,
    Project,
    Environment,
    Cli,
}
impl fmt::Display for SourceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::User => "user configuration",
            Self::Project => "project configuration",
            Self::Environment => "environment",
            Self::Cli => "command line",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSource {
    pub kind: SourceKind,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    pub config: Config,
    pub sources: Vec<LoadedSource>,
    pub provenance: BTreeMap<String, LoadedSource>,
    pub user_path: Option<PathBuf>,
}

#[derive(Debug)]
pub struct ConfigLoadError {
    pub source: Option<SourceKind>,
    pub path: Option<PathBuf>,
    pub key: Option<String>,
    message: String,
}
impl ConfigLoadError {
    fn new(source: Option<SourceKind>, path: Option<PathBuf>, message: impl Into<String>) -> Self {
        Self {
            source,
            path,
            key: None,
            message: message.into(),
        }
    }
    fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}
impl fmt::Display for ConfigLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(path) = &self.path {
            write!(formatter, "{}: ", path.display())?;
        } else if let Some(source) = self.source {
            write!(formatter, "{source}: ")?;
        }
        if let Some(key) = &self.key {
            write!(formatter, "{key}: ")?;
        }
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for ConfigLoadError {}

pub fn load(inputs: &ConfigInputs) -> Result<LoadedConfig, ConfigLoadError> {
    let mut config = Config::default();
    let mut sources = Vec::new();
    let mut provenance = BTreeMap::new();
    let (user_path, explicit_user_path) = user_config_path(inputs)?;
    if let Some(path) = user_path.clone() {
        if path.is_file() {
            let value = apply_file(&mut config, &path, SourceKind::User)?;
            record_provenance(&value, "", SourceKind::User, Some(&path), &mut provenance);
            // Relative skill roots are repository-relative, regardless of
            // which user configuration file declared them.
            let skill_base = inputs.repository_root.as_deref().unwrap_or(&inputs.cwd);
            resolve_trusted_skill_roots(&mut config, skill_base, inputs)?;
            sources.push(LoadedSource {
                kind: SourceKind::User,
                path: Some(path),
            });
        } else if explicit_user_path {
            return Err(ConfigLoadError::new(
                Some(SourceKind::User),
                Some(path),
                "explicit configuration file does not exist",
            ));
        }
    }
    if !project_config_disabled(inputs)?
        && let Some(repository_root) = &inputs.repository_root
    {
        let path = repository_root.join(".tg.toml");
        if path.exists() {
            let text = read_file(&path, SourceKind::Project)?;
            let value: toml::Value = toml::from_str(&text).map_err(|error| {
                ConfigLoadError::new(
                    Some(SourceKind::Project),
                    Some(path.clone()),
                    format!("invalid TOML: {error}"),
                )
            })?;
            validate_project_fields(&value, &path, repository_root)?;
            let replaces_roots = value
                .get("skills")
                .and_then(|skills| skills.get("roots"))
                .is_some();
            apply_text(&mut config, &text, &path, SourceKind::Project)?;
            record_provenance(
                &value,
                "",
                SourceKind::Project,
                Some(&path),
                &mut provenance,
            );
            if replaces_roots {
                resolve_project_skill_roots(&mut config, repository_root, &path)?;
            }
            sources.push(LoadedSource {
                kind: SourceKind::Project,
                path: Some(path),
            });
        }
    }
    if apply_environment(&mut config, inputs)? {
        record_environment_provenance(inputs, &mut provenance);
        sources.push(LoadedSource {
            kind: SourceKind::Environment,
            path: None,
        });
    }
    if apply_cli(&mut config, &inputs.cli) {
        record_cli_provenance(&inputs.cli, &mut provenance);
        sources.push(LoadedSource {
            kind: SourceKind::Cli,
            path: None,
        });
    }
    config.normalize_and_validate().map_err(|error| {
        let mut key = error.path.clone();
        let mut origin: Option<&LoadedSource> = provenance.get(&key);
        if origin.is_none()
            && key.starts_with("leaders.")
            && let Some((configured_key, configured_origin)) = provenance
                .iter()
                .filter(|(candidate, _)| candidate.starts_with("leaders."))
                .max_by_key(|(_, source)| source_precedence(source.kind))
        {
            key.clone_from(configured_key);
            origin = Some(configured_origin);
        }
        ConfigLoadError::new(
            origin.map(|source| source.kind),
            origin.and_then(|source| source.path.clone()),
            error.message.clone(),
        )
        .key(key)
    })?;
    Ok(LoadedConfig {
        config,
        sources,
        provenance,
        user_path,
    })
}

fn user_config_path(inputs: &ConfigInputs) -> Result<(Option<PathBuf>, bool), ConfigLoadError> {
    if let Some(path) = &inputs.cli.config_path {
        return Ok((Some(resolve_path(path, &inputs.cwd, inputs)?), true));
    }
    if let Some(path) = inputs.env("TG_CONFIG") {
        return Ok((
            Some(resolve_path(Path::new(path), &inputs.cwd, inputs)?),
            true,
        ));
    }
    if let Some(path) = inputs
        .env("XDG_CONFIG_HOME")
        .filter(|path| !path.is_empty())
    {
        let path = PathBuf::from(path);
        let base = if path.is_absolute() {
            path
        } else {
            inputs.cwd.join(path)
        };
        return Ok((Some(base.join("tg/config.toml")), false));
    }
    Ok((
        inputs
            .home_dir
            .as_ref()
            .map(|home| home.join(".config/tg/config.toml")),
        false,
    ))
}

fn project_config_disabled(inputs: &ConfigInputs) -> Result<bool, ConfigLoadError> {
    if inputs.cli.no_project_config {
        return Ok(true);
    }
    inputs
        .env("TG_NO_PROJECT_CONFIG")
        .map(parse_bool)
        .transpose()
        .map_err(|message| {
            ConfigLoadError::new(Some(SourceKind::Environment), None, message)
                .key("TG_NO_PROJECT_CONFIG")
        })
        .map(|value| value.unwrap_or(false))
}

fn parse_bool(value: &OsStr) -> Result<bool, String> {
    match value.to_string_lossy().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" | "" => Ok(false),
        _ => Err("must be a boolean (true/false or 1/0)".into()),
    }
}

fn apply_file(
    config: &mut Config,
    path: &Path,
    kind: SourceKind,
) -> Result<toml::Value, ConfigLoadError> {
    let text = read_file(path, kind)?;
    let value = toml::from_str(&text).map_err(|error| {
        ConfigLoadError::new(
            Some(kind),
            Some(path.to_path_buf()),
            format!("invalid TOML: {error}"),
        )
    })?;
    apply_text(config, &text, path, kind)?;
    Ok(value)
}

fn read_file(path: &Path, kind: SourceKind) -> Result<String, ConfigLoadError> {
    fs::read_to_string(path).map_err(|error| {
        ConfigLoadError::new(
            Some(kind),
            Some(path.to_path_buf()),
            format!("could not read configuration: {error}"),
        )
    })
}

fn apply_text(
    config: &mut Config,
    text: &str,
    path: &Path,
    kind: SourceKind,
) -> Result<(), ConfigLoadError> {
    apply_toml_patch(config, text).map_err(|error| {
        ConfigLoadError::new(Some(kind), Some(path.to_path_buf()), error.to_string())
    })
}

fn apply_environment(config: &mut Config, inputs: &ConfigInputs) -> Result<bool, ConfigLoadError> {
    let mut changed = false;
    if let Some(value) = inputs.env("TG_TOKENIZER") {
        config.tokens.tokenizer = env_string(value, "TG_TOKENIZER")?;
        changed = true;
    }
    if let Some(value) = inputs.env("TG_GH_COMMAND") {
        config.providers.github.command = PathBuf::from(env_string(value, "TG_GH_COMMAND")?);
        changed = true;
    }
    if let Some(value) = inputs.env("TG_JIRA_COMMAND") {
        config.providers.jira.command = PathBuf::from(env_string(value, "TG_JIRA_COMMAND")?);
        changed = true;
    }
    if inputs.env("NO_COLOR").is_some() {
        config.ui.color = ColorMode::Never;
        changed = true;
    }
    Ok(changed)
}

fn env_string(value: &OsStr, name: &str) -> Result<String, ConfigLoadError> {
    value.to_str().map(str::to_owned).ok_or_else(|| {
        ConfigLoadError::new(
            Some(SourceKind::Environment),
            None,
            "must contain valid UTF-8",
        )
        .key(name)
    })
}

fn apply_cli(config: &mut Config, cli: &CliConfigOverrides) -> bool {
    let mut changed = false;
    if let Some(value) = &cli.tokenizer {
        config.tokens.tokenizer.clone_from(value);
        changed = true;
    }
    if cli.no_preview {
        config.ui.preview = PreviewMode::Disabled;
        changed = true;
    }
    if cli.no_color {
        config.ui.color = ColorMode::Never;
        changed = true;
    }
    changed
}

fn resolve_trusted_skill_roots(
    config: &mut Config,
    base: &Path,
    inputs: &ConfigInputs,
) -> Result<(), ConfigLoadError> {
    for root in &mut config.skills.roots {
        root.path = resolve_path(&root.path, base, inputs)?;
    }
    Ok(())
}

fn resolve_path(
    path: &Path,
    base: &Path,
    inputs: &ConfigInputs,
) -> Result<PathBuf, ConfigLoadError> {
    let expanded = if path.starts_with("~") {
        let home = inputs.home_dir.as_ref().ok_or_else(|| {
            ConfigLoadError::new(
                None,
                None,
                "cannot expand `~` because the home directory is unknown",
            )
        })?;
        let suffix = path.strip_prefix("~").expect("prefix checked");
        home.join(suffix)
    } else {
        path.to_path_buf()
    };
    Ok(if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    })
}
