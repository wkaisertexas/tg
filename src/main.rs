mod cli;

use anyhow::{Context, Result};
use cli::Cli;

fn main() {
    if let Err(error) = real_main() {
        eprintln!("error: {error:#}");
        std::process::exit(2);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse_process();
    if cli.requests_update() {
        return tscodeselection::updater::update();
    }

    let cwd = std::env::current_dir().context("cannot determine current working directory")?;
    // File validation deliberately precedes terminal setup. Invalid UTF-8,
    // directories, and missing parents therefore cannot leave raw mode active.
    let document = match cli.file() {
        Some(path) => tscodeselection::editor::Document::open(path)
            .with_context(|| format!("could not open document {}", path.display()))?,
        None => tscodeselection::editor::Document::unnamed(),
    };
    let environment_root = std::env::var_os("TG_ROOT");
    let repository = tscodeselection::repository::Repository::for_editor(
        cli.explicit_root(),
        environment_root.as_deref().map(std::path::Path::new),
        &cwd,
        cli.file(),
    )?;
    let config_inputs = cli.config_inputs(&cwd, &repository.search_root);
    let loaded_config =
        tscodeselection::config::load(&config_inputs).context("could not load configuration")?;

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
    tscodeselection::app::run(tscodeselection::app::Startup {
        repository,
        config: loaded_config.config,
        document,
    })
}
