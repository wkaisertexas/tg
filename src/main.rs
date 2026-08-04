use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Directory inside the project to search.
    root_folder: Option<PathBuf>,
    /// Resolve one prompt without opening the TUI (useful for scripts/tests).
    #[arg(long)]
    resolve: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Replace this executable with the latest verified GitHub release.
    Update,
}

fn main() {
    if let Err(error) = real_main() {
        eprintln!("error: {error:#}");
        std::process::exit(2);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Some(Command::Update)) {
        return tscodeselection::updater::update();
    }
    let root_folder = cli.root_folder.context("a root folder is required")?;
    let repository = tscodeselection::repository::Repository::discover(&root_folder)?;
    if let Some(prompt) = cli.resolve {
        let lowered = tscodeselection::composer::resolve_prompt(&repository.search_root, &prompt)
            .context("could not resolve prompt")?;
        println!("{lowered}");
        return Ok(());
    }
    anyhow::ensure!(
        tscodeselection::app::is_terminal(),
        "interactive mode requires a terminal; use --resolve for headless operation"
    );
    tscodeselection::app::run(repository)
}
