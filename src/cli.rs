use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::ffi::OsString;
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
    long_about = "Edit coding-agent prompts with structured references.\n\nFILE may be an existing UTF-8 file or a new file whose parent directory exists."
)]
pub(crate) struct Cli {
    /// File to edit. Omit for an unnamed buffer.
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,

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

    pub(crate) fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    pub(crate) fn explicit_root(&self) -> Option<&Path> {
        self.root.as_deref()
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
    fn positional_file_and_explicit_root_are_independent() {
        let cli = Cli::try_parse_from(["tg", "prompt.md", "--root", "project"]).unwrap();
        assert_eq!(cli.file(), Some(Path::new("prompt.md")));
        assert_eq!(cli.explicit_root(), Some(Path::new("project")));

        let unnamed = Cli::try_parse_from(["tg", "--root", "project"]).unwrap();
        assert!(unnamed.file().is_none());

        let update = Cli::try_parse_from(["tg", "update"]).unwrap();
        assert!(update.requests_update(), "update must remain a subcommand");
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
