use serde::Deserialize;
use std::fmt;
use std::path::{Component, Path, PathBuf};

const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub version: u32,
    pub editor: EditorConfig,
    pub ui: UiConfig,
    pub leaders: LeadersConfig,
    pub search: SearchConfig,
    pub tokens: TokenConfig,
    pub skills: SkillsConfig,
    pub providers: ProvidersConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            editor: EditorConfig::default(),
            ui: UiConfig::default(),
            leaders: LeadersConfig::default(),
            search: SearchConfig::default(),
            tokens: TokenConfig::default(),
            skills: SkillsConfig::default(),
            providers: ProvidersConfig::default(),
        }
    }
}

impl Config {
    /// Parse one configuration document over the compiled defaults.
    ///
    /// Source discovery and multi-source precedence are intentionally handled
    /// outside the schema layer.
    pub fn from_toml(input: &str) -> Result<Self, ConfigError> {
        let patch = parse_patch(input)?;
        let mut config = Self::default();
        patch.apply(&mut config)?;
        config.normalize_and_validate()?;
        Ok(config)
    }

    pub fn normalize_and_validate(&mut self) -> Result<(), ValidationError> {
        validate_version(self.version)?;
        self.editor.validate()?;
        self.ui.validate()?;
        self.leaders.validate()?;
        self.search.validate()?;
        self.tokens.validate()?;
        self.skills.validate()?;
        self.providers.validate()?;
        Ok(())
    }
}

pub(crate) fn apply_toml_patch(config: &mut Config, input: &str) -> Result<(), ConfigError> {
    let patch = parse_patch(input)?;
    patch.apply(config)?;
    Ok(())
}

fn parse_patch(input: &str) -> Result<ConfigPatch, ConfigError> {
    let value: toml::Value = toml::from_str(input).map_err(ConfigError::Toml)?;
    validate_known_keys(&value)?;
    let deserializer = toml::de::Deserializer::parse(input).map_err(ConfigError::Toml)?;
    serde_path_to_error::deserialize(deserializer).map_err(|error| ConfigError::Type {
        path: error.path().to_string(),
        message: error.inner().to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorConfig {
    pub line_numbers: LineNumbers,
    pub current_line_absolute: bool,
    pub tab_width: u8,
    pub wrap: bool,
    pub copy_command: String,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            line_numbers: LineNumbers::Relative,
            current_line_absolute: true,
            tab_width: 4,
            wrap: true,
            copy_command: ":copy".into(),
        }
    }
}

impl EditorConfig {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_minimum("editor.tab_width", self.tab_width, 1)?;
        if !self.copy_command.starts_with(':')
            || self.copy_command.len() == 1
            || self.copy_command.chars().any(char::is_control)
        {
            return Err(invalid(
                "editor.copy_command",
                "must be a colon-prefixed printable command",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineNumbers {
    Relative,
    Absolute,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiConfig {
    pub preview: PreviewMode,
    pub preview_toggle: String,
    pub completion_height: u16,
    pub completion_width_percent: u8,
    pub status_timeout_ms: u64,
    pub color: ColorMode,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            preview: PreviewMode::Manual,
            preview_toggle: "ctrl-p".into(),
            completion_height: 12,
            completion_width_percent: 80,
            status_timeout_ms: 2_500,
            color: ColorMode::Auto,
        }
    }
}

impl UiConfig {
    fn validate(&self) -> Result<(), ValidationError> {
        if !is_key_notation(&self.preview_toggle) {
            return Err(invalid(
                "ui.preview_toggle",
                "must use application key notation such as `ctrl-p`",
            ));
        }
        validate_minimum("ui.completion_height", self.completion_height, 1)?;
        validate_range(
            "ui.completion_width_percent",
            self.completion_width_percent,
            1,
            100,
        )?;
        validate_minimum("ui.status_timeout_ms", self.status_timeout_ms, 1)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewMode {
    Manual,
    Automatic,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadersConfig {
    pub files: String,
    pub broad_files: String,
    pub symbols: String,
    pub skills: String,
    pub github_issues: String,
    pub github_pull_requests: String,
    pub jira_issues: String,
}

impl Default for LeadersConfig {
    fn default() -> Self {
        Self {
            files: "@".into(),
            broad_files: "%".into(),
            symbols: "::".into(),
            skills: "$".into(),
            github_issues: "#".into(),
            github_pull_requests: "!".into(),
            jira_issues: "&".into(),
        }
    }
}

impl LeadersConfig {
    fn validate(&self) -> Result<(), ValidationError> {
        let leaders = [
            ("leaders.files", self.files.as_str()),
            ("leaders.broad_files", self.broad_files.as_str()),
            ("leaders.symbols", self.symbols.as_str()),
            ("leaders.skills", self.skills.as_str()),
            ("leaders.github_issues", self.github_issues.as_str()),
            (
                "leaders.github_pull_requests",
                self.github_pull_requests.as_str(),
            ),
            ("leaders.jira_issues", self.jira_issues.as_str()),
        ];

        for (path, leader) in leaders {
            if leader.is_empty() {
                return Err(invalid(path, "must not be empty"));
            }
            if leader.contains('\\') {
                return Err(invalid(path, "must not contain the reserved escape `\\`"));
            }
            if leader
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
            {
                return Err(invalid(
                    path,
                    "must contain only printable non-whitespace characters",
                ));
            }
            if is_key_notation(leader) {
                return Err(invalid(path, "must not use application key notation"));
            }
        }

        for (index, (left_path, left)) in leaders.iter().enumerate() {
            for (right_path, right) in &leaders[index + 1..] {
                if left == right {
                    return Err(invalid(
                        left_path,
                        format!("duplicates `{right_path}` with leader `{left}`"),
                    ));
                }
                if left.starts_with(right) || right.starts_with(left) {
                    return Err(invalid(
                        left_path,
                        format!("is an ambiguous prefix of `{right_path}`"),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchConfig {
    pub limit: usize,
    pub debounce_ms: u64,
    pub broad_excludes: Vec<String>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            limit: 100,
            debounce_ms: 35,
            broad_excludes: vec![".git".into()],
        }
    }
}

impl SearchConfig {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_minimum("search.limit", self.limit, 1)?;
        validate_minimum("search.debounce_ms", self.debounce_ms, 1)?;
        if self
            .broad_excludes
            .iter()
            .any(|value| value.is_empty() || value.chars().any(char::is_control))
        {
            return Err(invalid(
                "search.broad_excludes",
                "entries must be nonempty and printable",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenConfig {
    pub tokenizer: String,
    pub decimals: u8,
    pub show_file: bool,
    pub show_symbol: bool,
    pub show_total: bool,
}

impl Default for TokenConfig {
    fn default() -> Self {
        Self {
            tokenizer: "gpt-4o".into(),
            decimals: 1,
            show_file: true,
            show_symbol: true,
            show_total: true,
        }
    }
}

impl TokenConfig {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.tokenizer != "gpt-4o" {
            return Err(invalid(
                "tokens.tokenizer",
                format!("unknown built-in tokenizer `{}`", self.tokenizer),
            ));
        }
        validate_range("tokens.decimals", self.decimals, 0, 3)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillsConfig {
    pub profile: String,
    pub mention: String,
    pub read_codex_disable_rules: bool,
    pub roots: Vec<SkillRootConfig>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            profile: "codex-local".into(),
            mention: "${leader}${name}".into(),
            read_codex_disable_rules: true,
            roots: Vec::new(),
        }
    }
}

impl SkillsConfig {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_printable("skills.profile", &self.profile)?;
        validate_mention("skills.mention", &self.mention)?;
        for (index, root) in self.roots.iter().enumerate() {
            root.validate(index)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRootConfig {
    pub path: PathBuf,
    pub scope: String,
    pub discovery: SkillDiscovery,
    pub walk_ancestors: bool,
    pub contained: bool,
    pub metadata: String,
    pub name_key: String,
    pub description_key: String,
    pub mention: Option<String>,
}

impl SkillRootConfig {
    fn validate(&self, index: usize) -> Result<(), ValidationError> {
        let base = format!("skills.roots[{index}]");
        if self.path.as_os_str().is_empty() {
            return Err(invalid_owned(format!("{base}.path"), "must not be empty"));
        }
        validate_printable_owned(format!("{base}.scope"), &self.scope)?;
        validate_metadata_filename(format!("{base}.metadata"), &self.metadata)?;
        validate_printable_owned(format!("{base}.name_key"), &self.name_key)?;
        validate_printable_owned(format!("{base}.description_key"), &self.description_key)?;
        if let Some(mention) = &self.mention {
            validate_mention_owned(format!("{base}.mention"), mention)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkillDiscovery {
    Recursive,
    DirectChildren,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProvidersConfig {
    pub github: GithubProviderConfig,
    pub jira: JiraProviderConfig,
}

impl ProvidersConfig {
    fn validate(&mut self) -> Result<(), ValidationError> {
        self.github.validate()?;
        self.jira.normalize_and_validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubProviderConfig {
    pub enabled: bool,
    pub command: PathBuf,
    pub limit: usize,
    pub timeout_ms: u64,
}

impl Default for GithubProviderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            command: "gh".into(),
            limit: 50,
            timeout_ms: 5_000,
        }
    }
}

impl GithubProviderConfig {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_command("providers.github.command", &self.command)?;
        validate_minimum("providers.github.limit", self.limit, 1)?;
        validate_minimum("providers.github.timeout_ms", self.timeout_ms, 1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraProviderConfig {
    pub enabled: bool,
    pub command: PathBuf,
    pub key_prefix: Option<String>,
    pub limit: usize,
    pub timeout_ms: u64,
}

impl Default for JiraProviderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            command: "jira".into(),
            key_prefix: None,
            limit: 50,
            timeout_ms: 5_000,
        }
    }
}

impl JiraProviderConfig {
    fn normalize_and_validate(&mut self) -> Result<(), ValidationError> {
        validate_command("providers.jira.command", &self.command)?;
        validate_minimum("providers.jira.limit", self.limit, 1)?;
        validate_minimum("providers.jira.timeout_ms", self.timeout_ms, 1)?;
        if let Some(prefix) = &mut self.key_prefix {
            *prefix = prefix
                .strip_suffix('-')
                .unwrap_or(prefix)
                .to_ascii_uppercase();
            let mut characters = prefix.chars();
            let valid = characters
                .next()
                .is_some_and(|value| value.is_ascii_uppercase())
                && characters.clone().next().is_some()
                && characters.all(|value| value.is_ascii_uppercase() || value.is_ascii_digit());
            if !valid {
                return Err(invalid(
                    "providers.jira.key_prefix",
                    "must be a Jira project key without a trailing hyphen",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Toml(toml::de::Error),
    UnknownKey(String),
    Type { path: String, message: String },
    Validation(ValidationError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml(error) => write!(formatter, "invalid TOML: {error}"),
            Self::UnknownKey(path) => write!(formatter, "unknown configuration key `{path}`"),
            Self::Type { path, message } => {
                write!(formatter, "invalid value for `{path}`: {message}")
            }
            Self::Validation(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Toml(error) => Some(error),
            Self::UnknownKey(_) => None,
            Self::Type { .. } => None,
            Self::Validation(error) => Some(error),
        }
    }
}

impl From<ValidationError> for ConfigError {
    fn from(value: ValidationError) -> Self {
        Self::Validation(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for ValidationError {}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ConfigPatch {
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
    check_child(
        root,
        "editor",
        &[
            "line_numbers",
            "current_line_absolute",
            "tab_width",
            "wrap",
            "copy_command",
        ],
    )?;
    check_child(
        root,
        "ui",
        &[
            "preview",
            "preview_toggle",
            "completion_height",
            "completion_width_percent",
            "status_timeout_ms",
            "color",
        ],
    )?;
    check_child(
        root,
        "leaders",
        &[
            "files",
            "broad_files",
            "symbols",
            "skills",
            "github_issues",
            "github_pull_requests",
            "jira_issues",
        ],
    )?;
    check_child(root, "search", &["limit", "debounce_ms", "broad_excludes"])?;
    check_child(
        root,
        "tokens",
        &[
            "tokenizer",
            "decimals",
            "show_file",
            "show_symbol",
            "show_total",
        ],
    )?;
    check_child(
        root,
        "skills",
        &["profile", "mention", "read_codex_disable_rules", "roots"],
    )?;
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
        if let Some(github) = providers.get("github").and_then(toml::Value::as_table) {
            check_keys(
                github,
                "providers.github",
                &["enabled", "command", "limit", "timeout_ms"],
            )?;
        }
        if let Some(jira) = providers.get("jira").and_then(toml::Value::as_table) {
            check_keys(
                jira,
                "providers.jira",
                &["enabled", "command", "key_prefix", "limit", "timeout_ms"],
            )?;
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

patch!(SearchPatch => SearchConfig {
    limit: usize,
    debounce_ms: u64,
    broad_excludes: Vec<String>,
});

patch!(TokenPatch => TokenConfig {
    tokenizer: String,
    decimals: u8,
    show_file: bool,
    show_symbol: bool,
    show_total: bool,
});

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
            path: self.path.ok_or_else(|| {
                invalid_owned(format!("skills.roots[{index}].path"), "is required")
            })?,
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

patch!(GithubProviderPatch => GithubProviderConfig {
    enabled: bool,
    command: PathBuf,
    limit: usize,
    timeout_ms: u64,
});

patch!(JiraProviderPatch => JiraProviderConfig {
    enabled: bool,
    command: PathBuf,
    key_prefix: Option<String>,
    limit: usize,
    timeout_ms: u64,
});

fn validate_version(version: u32) -> Result<(), ValidationError> {
    if version != CONFIG_VERSION {
        return Err(invalid(
            "version",
            format!("unsupported configuration version `{version}`; expected `{CONFIG_VERSION}`"),
        ));
    }
    Ok(())
}

fn validate_command(path: &'static str, command: &Path) -> Result<(), ValidationError> {
    if command.as_os_str().is_empty() {
        return Err(invalid(path, "must not be empty"));
    }
    if command
        .to_string_lossy()
        .chars()
        .any(|character| character.is_control())
    {
        return Err(invalid(
            path,
            "must not contain terminal control characters",
        ));
    }
    Ok(())
}

fn validate_metadata_filename(path: String, value: &str) -> Result<(), ValidationError> {
    if value.is_empty()
        || value.chars().any(char::is_control)
        || Path::new(value).components().count() != 1
        || !matches!(
            Path::new(value).components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(invalid_owned(
            path,
            "must be a printable filename, not a path",
        ));
    }
    Ok(())
}

fn validate_mention(path: &'static str, value: &str) -> Result<(), ValidationError> {
    validate_mention_owned(path.into(), value)
}

fn validate_mention_owned(path: String, value: &str) -> Result<(), ValidationError> {
    if value.contains('\n') || value.contains('\r') || value.chars().any(char::is_control) {
        return Err(invalid_owned(path, "must be a single printable line"));
    }
    if !value.contains("${name}") {
        return Err(invalid_owned(
            path,
            "must contain the `${name}` placeholder",
        ));
    }
    let remainder = value.replace("${name}", "").replace("${leader}", "");
    if remainder.contains("${") {
        return Err(invalid_owned(
            path,
            "supports only `${leader}` and `${name}` placeholders",
        ));
    }
    Ok(())
}

fn validate_printable(path: &'static str, value: &str) -> Result<(), ValidationError> {
    validate_printable_owned(path.into(), value)
}

fn validate_printable_owned(path: String, value: &str) -> Result<(), ValidationError> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(invalid_owned(path, "must be nonempty and printable"));
    }
    Ok(())
}

fn is_key_notation(value: &str) -> bool {
    let Some((modifier, key)) = value.split_once('-') else {
        return false;
    };
    matches!(
        modifier.to_ascii_lowercase().as_str(),
        "ctrl" | "alt" | "shift" | "meta"
    ) && !key.is_empty()
        && !key.contains('-')
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
}

fn validate_range<T>(
    path: &'static str,
    value: T,
    minimum: T,
    maximum: T,
) -> Result<(), ValidationError>
where
    T: Copy + Ord + fmt::Display,
{
    if value < minimum || value > maximum {
        return Err(invalid(
            path,
            format!("must be between {minimum} and {maximum}, got {value}"),
        ));
    }
    Ok(())
}

fn validate_minimum<T>(path: &'static str, value: T, minimum: T) -> Result<(), ValidationError>
where
    T: Copy + Ord + fmt::Display,
{
    if value < minimum {
        return Err(invalid(
            path,
            format!("must be at least {minimum}, got {value}"),
        ));
    }
    Ok(())
}

fn invalid(path: &'static str, message: impl Into<String>) -> ValidationError {
    invalid_owned(path.into(), message)
}

fn invalid_owned(path: String, message: impl Into<String>) -> ValidationError {
    ValidationError {
        path,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_defaults_match_the_documented_schema() {
        let config = Config::default();
        assert_eq!(config.version, 1);
        assert_eq!(config.editor.line_numbers, LineNumbers::Relative);
        assert!(config.editor.current_line_absolute);
        assert_eq!(config.editor.tab_width, 4);
        assert!(config.editor.wrap);
        assert_eq!(config.editor.copy_command, ":copy");
        assert_eq!(config.ui.preview, PreviewMode::Manual);
        assert_eq!(config.ui.preview_toggle, "ctrl-p");
        assert_eq!(config.ui.completion_height, 12);
        assert_eq!(config.ui.completion_width_percent, 80);
        assert_eq!(config.ui.status_timeout_ms, 2_500);
        assert_eq!(config.ui.color, ColorMode::Auto);
        assert_eq!(config.leaders, LeadersConfig::default());
        assert_eq!(config.search.limit, 100);
        assert_eq!(config.search.debounce_ms, 35);
        assert_eq!(config.search.broad_excludes, [".git"]);
        assert_eq!(config.tokens.tokenizer, "gpt-4o");
        assert_eq!(config.tokens.decimals, 1);
        assert!(config.tokens.show_file && config.tokens.show_symbol && config.tokens.show_total);
        assert_eq!(config.skills.profile, "codex-local");
        assert_eq!(config.skills.mention, "${leader}${name}");
        assert!(config.skills.read_codex_disable_rules);
        assert!(config.skills.roots.is_empty());
        assert_eq!(config.providers.github, GithubProviderConfig::default());
        assert_eq!(config.providers.jira, JiraProviderConfig::default());
        Config::default().normalize_and_validate().unwrap();
    }

    #[test]
    fn complete_documented_fixture_decodes_strictly() {
        let config =
            Config::from_toml(include_str!("../../tests/fixtures/config/complete.toml")).unwrap();
        assert_eq!(config.editor.line_numbers, LineNumbers::Absolute);
        assert_eq!(config.ui.preview, PreviewMode::Automatic);
        assert_eq!(config.ui.color, ColorMode::Never);
        assert_eq!(config.leaders.files, "@@");
        assert_eq!(config.search.limit, 75);
        assert_eq!(config.skills.roots.len(), 1);
        assert_eq!(config.skills.roots[0].discovery, SkillDiscovery::Recursive);
        assert_eq!(
            config.providers.github.command,
            PathBuf::from("/opt/bin/gh")
        );
        assert_eq!(config.providers.jira.key_prefix.as_deref(), Some("G5"));
    }

    #[test]
    fn unknown_root_and_nested_keys_are_errors() {
        assert!(
            Config::from_toml("mystery = true")
                .unwrap_err()
                .to_string()
                .contains("mystery")
        );
        assert!(
            Config::from_toml("[ui]\nmystery = true")
                .unwrap_err()
                .to_string()
                .contains("mystery")
        );
        assert!(
            Config::from_toml("[[skills.roots]]\npath='skills'\nmystery=true")
                .unwrap_err()
                .to_string()
                .contains("mystery")
        );
    }

    #[test]
    fn partial_documents_keep_compiled_defaults() {
        let config = Config::from_toml("[search]\nlimit = 7").unwrap();
        assert_eq!(config.search.limit, 7);
        assert_eq!(config.search.debounce_ms, 35);
        assert_eq!(config.ui, UiConfig::default());
    }

    #[test]
    fn leaders_reject_invalid_characters_duplicates_and_prefixes() {
        for value in ["", "two words", "\\", "ctrl-p", "x\n"] {
            let input = format!("[leaders]\nfiles = {value:?}");
            assert!(Config::from_toml(&input).is_err(), "accepted {value:?}");
        }
        let duplicate = Config::from_toml("[leaders]\nfiles='!' ");
        assert!(duplicate.unwrap_err().to_string().contains("duplicates"));
        let prefix = Config::from_toml("[leaders]\nfiles=':'");
        assert!(prefix.unwrap_err().to_string().contains("ambiguous prefix"));
    }

    #[test]
    fn numeric_bounds_are_enforced() {
        for input in [
            "[editor]\ntab_width=0",
            "[ui]\ncompletion_height=0",
            "[ui]\ncompletion_width_percent=101",
            "[ui]\nstatus_timeout_ms=0",
            "[search]\nlimit=0",
            "[search]\ndebounce_ms=0",
            "[tokens]\ndecimals=4",
            "[providers.github]\nlimit=0",
            "[providers.github]\ntimeout_ms=0",
        ] {
            assert!(Config::from_toml(input).is_err(), "accepted {input}");
        }
    }

    #[test]
    fn jira_prefix_is_normalized_and_validated() {
        let config = Config::from_toml("[providers.jira]\nkey_prefix='g5-'").unwrap();
        assert_eq!(config.providers.jira.key_prefix.as_deref(), Some("G5"));
        for value in ["", "5G", "A-2", "G5--", "G_5", "G 5", "A"] {
            let input = format!("[providers.jira]\nkey_prefix={value:?}");
            assert!(Config::from_toml(&input).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn mention_templates_are_safe_and_named() {
        for value in ["literal", "${unknown}${name}", "${name}\nignore"] {
            let input = format!("[skills]\nmention={value:?}");
            assert!(Config::from_toml(&input).is_err(), "accepted {value:?}");
        }
        Config::from_toml("[skills]\nmention='$${name}'").unwrap();
        Config::from_toml("[skills]\nmention='${leader}${name}'").unwrap();
    }

    #[test]
    fn commands_and_required_skill_root_fields_are_validated() {
        assert!(Config::from_toml("[providers.github]\ncommand=''").is_err());
        assert!(Config::from_toml("[providers.jira]\ncommand=\"jira\\nunsafe\"").is_err());
        assert!(Config::from_toml("[[skills.roots]]\nscope='user'").is_err());
        assert!(
            Config::from_toml("[[skills.roots]]\npath='skills'\nmetadata='../SKILL.md'").is_err()
        );
    }

    #[test]
    fn unsupported_versions_and_tokenizers_are_rejected() {
        assert!(Config::from_toml("version=2").is_err());
        assert!(Config::from_toml("[tokens]\ntokenizer='future'").is_err());
    }
}
