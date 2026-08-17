use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tg"));
    command.env("HOME", home);
    for name in [
        "XDG_CONFIG_HOME",
        "TG_CONFIG",
        "TG_ROOT",
        "TG_TOKENIZER",
        "TG_GH_COMMAND",
        "TG_JIRA_COMMAND",
        "TG_NO_PROJECT_CONFIG",
        "NO_COLOR",
    ] {
        command.env_remove(name);
    }
    command
}

fn resolve(command: &mut Command, root_arguments: &[&str]) -> Output {
    command
        .args(root_arguments)
        .args(["--resolve", "Read @selected.txt"])
        .output()
        .unwrap()
}

fn create_root(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::write(path.join("selected.txt"), "selected\n").unwrap();
}

#[test]
fn help_documents_the_phase_one_surface() {
    let temp = tempfile::tempdir().unwrap();
    let output = command(temp.path()).arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "--root",
        "--config",
        "--no-project-config",
        "--tokenizer",
        "--no-preview",
        "--no-color",
        "update",
        "ROOT_FOLDER",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected} in:\n{stdout}"
        );
    }
}

#[test]
fn legacy_and_explicit_root_resolve_identically() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let home = temp.path().join("home");
    create_root(&root);
    fs::create_dir(&home).unwrap();
    let root_text = root.to_str().unwrap();

    let legacy = resolve(&mut command(&home), &[root_text]);
    let explicit = resolve(&mut command(&home), &["--root", root_text]);
    assert!(
        legacy.status.success(),
        "{}",
        String::from_utf8_lossy(&legacy.stderr)
    );
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    assert_eq!(legacy.stdout, explicit.stdout);
    assert_eq!(legacy.stdout, b"Read selected.txt\n");
}

#[test]
fn root_precedence_is_cli_then_tg_root_then_legacy() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cli_root = temp.path().join("cli");
    let env_root = temp.path().join("environment");
    let legacy_root = temp.path().join("legacy-missing");
    fs::create_dir(&home).unwrap();
    create_root(&cli_root);
    create_root(&env_root);

    let output = resolve(
        command(&home).env("TG_ROOT", &env_root),
        &[legacy_root.to_str().unwrap()],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = resolve(
        command(&home).env("TG_ROOT", temp.path().join("env-missing")),
        &["--root", cli_root.to_str().unwrap()],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn positional_and_explicit_root_are_rejected_together() {
    let temp = tempfile::tempdir().unwrap();
    let output = command(temp.path())
        .args(["legacy", "--root", "explicit", "--resolve", "plain"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}

#[test]
fn explicit_and_default_config_errors_block_headless_work() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = temp.path().join("root");
    fs::create_dir_all(home.join(".config/tg")).unwrap();
    create_root(&root);
    fs::write(
        home.join(".config/tg/config.toml"),
        "[search]\nunknown=true\n",
    )
    .unwrap();

    let output = resolve(&mut command(&home), &["--root", root.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("config.toml"), "{stderr}");
    assert!(stderr.contains("search.unknown"), "{stderr}");

    fs::write(home.join(".config/tg/config.toml"), "").unwrap();
    let bad = temp.path().join("bad.toml");
    fs::write(&bad, "[providers.github]\ncommand=''\n").unwrap();
    let output = command(&home)
        .args([
            "--root",
            root.to_str().unwrap(),
            "--config",
            bad.to_str().unwrap(),
            "--resolve",
            "plain",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("providers.github.command"));
}

#[test]
fn project_config_can_be_disabled_by_cli_or_environment() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = temp.path().join("root");
    fs::create_dir(&home).unwrap();
    create_root(&root);
    fs::create_dir(root.join(".git")).unwrap();
    fs::write(
        root.join(".tg.toml"),
        "[providers.github]\ncommand='evil'\n",
    )
    .unwrap();
    let root_text = root.to_str().unwrap();

    let blocked = resolve(&mut command(&home), &["--root", root_text]);
    assert!(!blocked.status.success());

    let cli = resolve(
        &mut command(&home),
        &["--root", root_text, "--no-project-config"],
    );
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );

    let environment = resolve(
        command(&home).env("TG_NO_PROJECT_CONFIG", "1"),
        &["--root", root_text],
    );
    assert!(
        environment.status.success(),
        "{}",
        String::from_utf8_lossy(&environment.stderr)
    );
}

#[test]
fn tokenizer_cli_override_is_validated_after_files() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = temp.path().join("root");
    fs::create_dir(&home).unwrap();
    create_root(&root);
    let output = command(&home)
        .args([
            "--root",
            root.to_str().unwrap(),
            "--tokenizer",
            "future",
            "--resolve",
            "plain",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("tokens.tokenizer"));
}
