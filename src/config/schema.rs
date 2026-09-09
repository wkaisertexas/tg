mod patch;
#[cfg(test)]
mod tests;
mod validation;

pub(crate) use patch::apply_toml_patch;
use serde::Deserialize;
use std::path::PathBuf;
use validation::*;

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
        let mut config = Self::default();
        apply_toml_patch(&mut config, input)?;
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
                        *left_path,
                        format!("duplicates `{right_path}` with leader `{left}`"),
                    ));
                }
                if left.starts_with(right) || right.starts_with(left) {
                    return Err(invalid(
                        *left_path,
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
            return Err(invalid(format!("{base}.path"), "must not be empty"));
        }
        validate_printable(format!("{base}.scope"), &self.scope)?;
        validate_metadata_filename(format!("{base}.metadata"), &self.metadata)?;
        validate_printable(format!("{base}.name_key"), &self.name_key)?;
        validate_printable(format!("{base}.description_key"), &self.description_key)?;
        if let Some(mention) = &self.mention {
            validate_mention(format!("{base}.mention"), mention)?;
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

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid TOML: {0}")]
    Toml(#[source] toml::de::Error),
    #[error("unknown configuration key `{0}`")]
    UnknownKey(String),
    #[error("invalid value for `{path}`: {message}")]
    Type { path: String, message: String },
    #[error("{0}")]
    Validation(#[from] ValidationError),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{path}: {message}")]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}
