mod cli;

use anyhow::{Context, Result};
use cli::{Cli, Command};
use std::process::ExitCode;

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn real_main() -> Result<ExitCode> {
    let cli = Cli::parse_process();
    if cli.requests_update() {
        tscodeselection::updater::update()?;
        return Ok(ExitCode::SUCCESS);
    }

    let cwd = std::env::current_dir().context("cannot determine current working directory")?;
    // File validation deliberately precedes terminal setup. Invalid UTF-8,
    // directories, and missing parents therefore cannot leave raw mode active.
    let (document, save_target) = match cli.file() {
        Some(path) => {
            let opened = tscodeselection::editor::save::SaveTarget::open(path)
                .with_context(|| format!("could not open document {}", path.display()))?;
            let existed = opened.target.existed_at_baseline();
            let document = tscodeselection::editor::Document::from_opened_bytes(
                opened.target.logical_path(),
                opened.bytes,
                existed,
            )
            .with_context(|| format!("could not decode document {}", path.display()))?;
            (document, Some(opened.target))
        }
        None => (tscodeselection::editor::Document::unnamed(), None),
    };
    let environment_root = std::env::var_os("TG_ROOT");
    let repository = tscodeselection::repository::Repository::for_editor(
        cli.explicit_root(),
        environment_root.as_deref().map(std::path::Path::new),
        &cwd,
        cli.file(),
    )?;
    let config_inputs = cli.config_inputs(&cwd, &repository.search_root);
    let loaded_config = tscodeselection::config::load(&config_inputs).context(
        "could not load configuration; fix the file/key below. Use --no-project-config to bypass project settings, or --config FILE to select a different user configuration",
    )?;

    match cli.command() {
        Some(Command::Doctor { check, json }) => {
            let failed = tscodeselection::onboarding::doctor(
                &repository,
                &loaded_config,
                check.as_deref(),
                *json,
            )?;
            return Ok(if failed {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            });
        }
        Some(Command::Setup) => {
            if tscodeselection::app::is_terminal() {
                tscodeselection::onboarding::run(repository, loaded_config)?;
            } else {
                tscodeselection::onboarding::doctor(&repository, &loaded_config, None, false)?;
            }
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }
    if let Some(prompt) = cli.resolve {
        let lowered = tscodeselection::composer::resolve_prompt_with_config(
            &repository,
            &loaded_config.config,
            &prompt,
        )
        .context("could not resolve prompt")?;
        println!("{lowered}");
        return Ok(ExitCode::SUCCESS);
    }
    anyhow::ensure!(
        tscodeselection::app::is_terminal(),
        "interactive mode requires a terminal; use --resolve for headless operation or `tg doctor` for setup diagnostics"
    );
    tscodeselection::app::run(tscodeselection::app::Startup {
        repository,
        config: loaded_config,
        document,
        save_target,
    })?;
    Ok(ExitCode::SUCCESS)
}
