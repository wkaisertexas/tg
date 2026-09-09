use super::*;

pub(crate) fn apply_toml_patch(config: &mut Config, input: &str) -> Result<(), ConfigError> {
    let value: toml::Value = toml::from_str(input).map_err(ConfigError::Toml)?;
    validate_known_keys(&value)?;
    let deserializer = toml::de::Deserializer::parse(input).map_err(ConfigError::Toml)?;
    let patch: ConfigPatch =
        serde_path_to_error::deserialize(deserializer).map_err(|error| ConfigError::Type {
            path: error.path().to_string(),
            message: error.inner().to_string(),
        })?;
    patch.apply(config)?;
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigPatch {
    version: Option<u32>,
    editor: Option<EditorPatch>,
    ui: Option<UiPatch>,
    leaders: Option<LeadersPatch>,
    search: Option<SearchPatch>,
    tokens: Option<TokenPatch>,
    skills: Option<SkillsPatch>,
    providers: Option<ProvidersPatch>,
}

impl ConfigPatch {
    fn apply(self, config: &mut Config) -> Result<(), ValidationError> {
        if let Some(version) = self.version {
            validate_version(version)?;
            config.version = version;
        }
        if let Some(patch) = self.editor {
            patch.apply(&mut config.editor);
        }
        if let Some(patch) = self.ui {
            patch.apply(&mut config.ui);
        }
        if let Some(patch) = self.leaders {
            patch.apply(&mut config.leaders);
        }
        if let Some(patch) = self.search {
            patch.apply(&mut config.search);
        }
        if let Some(patch) = self.tokens {
            patch.apply(&mut config.tokens);
        }
        if let Some(patch) = self.skills {
            patch.apply(&mut config.skills)?;
        }
        if let Some(patch) = self.providers {
            patch.apply(&mut config.providers);
        }
        Ok(())
    }
}

fn validate_known_keys(value: &toml::Value) -> Result<(), ConfigError> {
    let Some(root) = value.as_table() else {
        return Ok(());
    };
    check_keys(
        root,
        "",
        &[
            "version",
            "editor",
            "ui",
            "leaders",
            "search",
            "tokens",
            "skills",
            "providers",
        ],
    )?;
    for (child, keys) in [
        ("editor", EditorPatch::KEYS),
        ("ui", UiPatch::KEYS),
        ("leaders", LeadersPatch::KEYS),
        ("search", SearchPatch::KEYS),
        ("tokens", TokenPatch::KEYS),
        (
            "skills",
            &["profile", "mention", "read_codex_disable_rules", "roots"],
        ),
    ] {
        check_child(root, child, keys)?;
    }
    if let Some(roots) = root
        .get("skills")
        .and_then(|value| value.get("roots"))
        .and_then(toml::Value::as_array)
    {
        for (index, root) in roots.iter().enumerate() {
            if let Some(table) = root.as_table() {
                check_keys(
                    table,
                    &format!("skills.roots[{index}]"),
                    &[
                        "path",
                        "scope",
                        "discovery",
                        "walk_ancestors",
                        "contained",
                        "metadata",
                        "name_key",
                        "description_key",
                        "mention",
                    ],
                )?;
            }
        }
    }
    check_child(root, "providers", &["github", "jira"])?;
    if let Some(providers) = root.get("providers").and_then(toml::Value::as_table) {
        for (name, keys) in [
            ("github", GithubProviderPatch::KEYS),
            ("jira", JiraProviderPatch::KEYS),
        ] {
            if let Some(table) = providers.get(name).and_then(toml::Value::as_table) {
                check_keys(table, &format!("providers.{name}"), keys)?;
            }
        }
    }
    Ok(())
}

fn check_child(
    parent: &toml::map::Map<String, toml::Value>,
    child: &str,
    allowed: &[&str],
) -> Result<(), ConfigError> {
    if let Some(table) = parent.get(child).and_then(toml::Value::as_table) {
        check_keys(table, child, allowed)?;
    }
    Ok(())
}

fn check_keys(
    table: &toml::map::Map<String, toml::Value>,
    prefix: &str,
    allowed: &[&str],
) -> Result<(), ConfigError> {
    if let Some(key) = table.keys().find(|key| !allowed.contains(&key.as_str())) {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        return Err(ConfigError::UnknownKey(path));
    }
    Ok(())
}

macro_rules! patch {
    ($name:ident => $target:ty { $($field:ident: $kind:ty),+ $(,)? }) => {
        #[derive(Debug, Default, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        struct $name {
            $( $field: Option<$kind>, )+
        }

        impl $name {
            const KEYS: &'static [&'static str] = &[$(stringify!($field)),+];

            fn apply(self, target: &mut $target) {
                $( if let Some(value) = self.$field { target.$field = value; } )+
            }
        }
    };
}

patch!(EditorPatch => EditorConfig {
    line_numbers: LineNumbers,
    current_line_absolute: bool,
    tab_width: u8,
    wrap: bool,
    copy_command: String,
});
patch!(UiPatch => UiConfig {
    preview: PreviewMode,
    preview_toggle: String,
    completion_height: u16,
    completion_width_percent: u8,
    status_timeout_ms: u64,
    color: ColorMode,
});
patch!(LeadersPatch => LeadersConfig {
    files: String,
    broad_files: String,
    symbols: String,
    skills: String,
    github_issues: String,
    github_pull_requests: String,
    jira_issues: String,
});
patch!(SearchPatch => SearchConfig { limit: usize, debounce_ms: u64, broad_excludes: Vec<String> });
patch!(TokenPatch => TokenConfig { tokenizer: String, decimals: u8, show_file: bool, show_symbol: bool, show_total: bool });

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SkillsPatch {
    profile: Option<String>,
    mention: Option<String>,
    read_codex_disable_rules: Option<bool>,
    roots: Option<Vec<SkillRootPatch>>,
}
impl SkillsPatch {
    fn apply(self, target: &mut SkillsConfig) -> Result<(), ValidationError> {
        if let Some(value) = self.profile {
            target.profile = value;
        }
        if let Some(value) = self.mention {
            target.mention = value;
        }
        if let Some(value) = self.read_codex_disable_rules {
            target.read_codex_disable_rules = value;
        }
        if let Some(roots) = self.roots {
            target.roots = roots
                .into_iter()
                .enumerate()
                .map(|(index, root)| root.into_config(index))
                .collect::<Result<_, _>>()?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillRootPatch {
    path: Option<PathBuf>,
    scope: Option<String>,
    discovery: Option<SkillDiscovery>,
    walk_ancestors: Option<bool>,
    contained: Option<bool>,
    metadata: Option<String>,
    name_key: Option<String>,
    description_key: Option<String>,
    mention: Option<String>,
}
impl SkillRootPatch {
    fn into_config(self, index: usize) -> Result<SkillRootConfig, ValidationError> {
        Ok(SkillRootConfig {
            path: self
                .path
                .ok_or_else(|| invalid(format!("skills.roots[{index}].path"), "is required"))?,
            scope: self.scope.unwrap_or_else(|| "user".into()),
            discovery: self.discovery.unwrap_or(SkillDiscovery::DirectChildren),
            walk_ancestors: self.walk_ancestors.unwrap_or(false),
            contained: self.contained.unwrap_or(true),
            metadata: self.metadata.unwrap_or_else(|| "SKILL.md".into()),
            name_key: self.name_key.unwrap_or_else(|| "name".into()),
            description_key: self.description_key.unwrap_or_else(|| "description".into()),
            mention: self.mention,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProvidersPatch {
    github: Option<GithubProviderPatch>,
    jira: Option<JiraProviderPatch>,
}
impl ProvidersPatch {
    fn apply(self, target: &mut ProvidersConfig) {
        if let Some(patch) = self.github {
            patch.apply(&mut target.github);
        }
        if let Some(patch) = self.jira {
            patch.apply(&mut target.jira);
        }
    }
}
patch!(GithubProviderPatch => GithubProviderConfig { enabled: bool, command: PathBuf, limit: usize, timeout_ms: u64 });
patch!(JiraProviderPatch => JiraProviderConfig { enabled: bool, command: PathBuf, key_prefix: Option<String>, limit: usize, timeout_ms: u64 });
