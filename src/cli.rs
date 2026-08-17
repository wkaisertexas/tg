use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use tscodeselection::config::{CliConfigOverrides, ConfigInputs};

const CONFIG_ENVIRONMENT: &[&str] = &[
    "HOME",
    "XDG_CONFIG_HOME",
    "TG_CONFIG",
    "TG_ROOT",
    "TG_TOKENIZER",
    "TG_GH_COMMAND",
    "TG_JIRA_COMMAND",
    "TG_NO_PROJECT_CONFIG",
    "NO_COLOR",
];

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Compose coding-agent prompts with structured references",
    long_about = "Compose coding-agent prompts with structured references.\n\nThe positional ROOT_FOLDER form is retained for v0.1 compatibility; prefer --root."
)]
pub(crate) struct Cli {
    /// Legacy search root (prefer --root; becomes FILE in the prompt editor).
    #[arg(value_name = "ROOT_FOLDER", conflicts_with = "root")]
    legacy_root: Option<PathBuf>,

    /// Directory inside the project to search.
    #[arg(long, value_name = "DIRECTORY")]
    root: Option<PathBuf>,

    /// Replace the normal user configuration file.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Do not load .tg.toml from the repository root.
    #[arg(long)]
    no_project_config: bool,

    /// Select a built-in tokenizer by name.
    #[arg(long, value_name = "NAME")]
    tokenizer: Option<String>,

    /// Disable the completion preview.
    #[arg(long)]
    no_preview: bool,

    /// Disable colored output.
    #[arg(long)]
    no_color: bool,

    /// Resolve one prompt without opening the TUI (useful for scripts/tests).
    #[arg(long, value_name = "PROMPT")]
    pub(crate) resolve: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Replace this executable with the latest verified GitHub release.
    Update,
}

impl Cli {
    pub(crate) fn parse_process() -> Self {
        Self::parse()
    }

    pub(crate) fn requests_update(&self) -> bool {
        matches!(self.command, Some(Command::Update))
    }

    pub(crate) fn root(&self, tg_root: Option<&OsStr>) -> Option<PathBuf> {
        self.root
            .clone()
            .or_else(|| tg_root.map(PathBuf::from))
            .or_else(|| self.legacy_root.clone())
    }

    pub(crate) fn config_inputs(&self, cwd: &Path, repository_root: &Path) -> ConfigInputs {
        let environment: BTreeMap<String, OsString> = CONFIG_ENVIRONMENT
            .iter()
            .filter_map(|name| std::env::var_os(name).map(|value| ((*name).into(), value)))
            .collect();
        let mut inputs = ConfigInputs::new(cwd.to_path_buf());
        inputs.home_dir = environment.get("HOME").map(PathBuf::from);
        inputs.repository_root = Some(repository_root.to_path_buf());
        inputs.environment = environment;
        inputs.cli = CliConfigOverrides {
            config_path: self.config.clone(),
            no_project_config: self.no_project_config,
            tokenizer: self.tokenizer.clone(),
            no_preview: self.no_preview,
            no_color: self.no_color,
        };
        inputs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn root_precedence_is_cli_then_environment_then_legacy() {
        let cli = Cli::try_parse_from(["tg", "legacy", "--root", "explicit"]);
        assert!(cli.is_err(), "positional and --root must conflict");

        let cli = Cli::try_parse_from(["tg", "--root", "explicit"]).unwrap();
        assert_eq!(
            cli.root(Some(OsStr::new("environment"))),
            Some(PathBuf::from("explicit"))
        );

        let cli = Cli::try_parse_from(["tg", "legacy"]).unwrap();
        assert_eq!(
            cli.root(Some(OsStr::new("environment"))),
            Some(PathBuf::from("environment"))
        );
        assert_eq!(cli.root(None), Some(PathBuf::from("legacy")));
    }

    #[test]
    fn flags_map_to_config_overrides() {
        let cli = Cli::try_parse_from([
            "tg",
            "--root",
            ".",
            "--config",
            "custom.toml",
            "--no-project-config",
            "--tokenizer",
            "gpt-4o",
            "--no-preview",
            "--no-color",
        ])
        .unwrap();
        let inputs = cli.config_inputs(Path::new("/cwd"), Path::new("/repo"));
        assert_eq!(inputs.repository_root, Some(PathBuf::from("/repo")));
        assert_eq!(inputs.cli.config_path, Some(PathBuf::from("custom.toml")));
        assert!(inputs.cli.no_project_config);
        assert_eq!(inputs.cli.tokenizer.as_deref(), Some("gpt-4o"));
        assert!(inputs.cli.no_preview);
        assert!(inputs.cli.no_color);
    }
}
