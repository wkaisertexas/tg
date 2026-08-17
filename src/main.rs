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

    let root_folder = cli
        .root(std::env::var_os("TG_ROOT").as_deref())
        .context("a root folder is required; pass --root DIRECTORY")?;
    let repository = tscodeselection::repository::Repository::discover(&root_folder)?;
    let cwd = std::env::current_dir().context("cannot determine current working directory")?;
    let config_inputs = cli.config_inputs(&cwd, &repository.search_root);
    let _loaded_config =
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
    tscodeselection::app::run(repository)
}
