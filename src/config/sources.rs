use super::schema::apply_toml_patch;
use super::{ColorMode, Config, PreviewMode};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

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
    if let Some(path) = user_path {
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
        let origin: Option<&LoadedSource> = provenance.get(&error.path);
        ConfigLoadError::new(
            origin.map(|source| source.kind),
            origin.and_then(|source| source.path.clone()),
            error.message.clone(),
        )
        .key(error.path)
    })?;
    Ok(LoadedConfig { config, sources })
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

fn record_provenance(
    value: &toml::Value,
    prefix: &str,
    kind: SourceKind,
    path: Option<&Path>,
    provenance: &mut BTreeMap<String, LoadedSource>,
) {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                let child = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                record_provenance(value, &child, kind, path, provenance);
            }
        }
        toml::Value::Array(values) => {
            provenance.insert(
                prefix.into(),
                LoadedSource {
                    kind,
                    path: path.map(Path::to_path_buf),
                },
            );
            for (index, value) in values.iter().enumerate() {
                record_provenance(value, &format!("{prefix}[{index}]"), kind, path, provenance);
            }
        }
        _ => {
            provenance.insert(
                prefix.into(),
                LoadedSource {
                    kind,
                    path: path.map(Path::to_path_buf),
                },
            );
        }
    }
}

fn record_environment_provenance(
    inputs: &ConfigInputs,
    provenance: &mut BTreeMap<String, LoadedSource>,
) {
    for (variable, key) in [
        ("TG_TOKENIZER", "tokens.tokenizer"),
        ("TG_GH_COMMAND", "providers.github.command"),
        ("TG_JIRA_COMMAND", "providers.jira.command"),
        ("NO_COLOR", "ui.color"),
    ] {
        if inputs.env(variable).is_some() {
            provenance.insert(
                key.into(),
                LoadedSource {
                    kind: SourceKind::Environment,
                    path: None,
                },
            );
        }
    }
}

fn record_cli_provenance(
    cli: &CliConfigOverrides,
    provenance: &mut BTreeMap<String, LoadedSource>,
) {
    for (changed, key) in [
        (cli.tokenizer.is_some(), "tokens.tokenizer"),
        (cli.no_preview, "ui.preview"),
        (cli.no_color, "ui.color"),
    ] {
        if changed {
            provenance.insert(
                key.into(),
                LoadedSource {
                    kind: SourceKind::Cli,
                    path: None,
                },
            );
        }
    }
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

fn validate_project_fields(
    value: &toml::Value,
    config_path: &Path,
    repository_root: &Path,
) -> Result<(), ConfigLoadError> {
    let Some(root) = value.as_table() else {
        return Ok(());
    };
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
                    &["enabled", "key_prefix", "limit", "timeout_ms"]
                } else {
                    &["enabled", "limit", "timeout_ms"]
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
    if candidate.exists() {
        let canonical_root = repository_root
            .canonicalize()
            .map_err(|error| format!("cannot resolve repository root: {error}"))?;
        let canonical_candidate = candidate
            .canonicalize()
            .map_err(|error| format!("cannot resolve skill root: {error}"))?;
        if !canonical_candidate.starts_with(canonical_root) {
            return Err("resolves outside the repository".into());
        }
    }
    Ok(())
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

fn resolve_project_skill_roots(
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

#[cfg(test)]
mod tests {
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
}
