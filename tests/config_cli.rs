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
fn help_documents_the_prompt_editor_surface() {
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
        "FILE",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected} in:\n{stdout}"
        );
    }
}

#[test]
fn cwd_discovery_and_explicit_root_resolve_identically_with_a_positional_file() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let home = temp.path().join("home");
    create_root(&root);
    fs::write(root.join("prompt.md"), "prompt\n").unwrap();
    fs::create_dir(&home).unwrap();
    let root_text = root.to_str().unwrap();

    let implicit = resolve(command(&home).current_dir(&root), &["prompt.md"]);
    let explicit = resolve(&mut command(&home), &["--root", root_text]);
    assert!(
        implicit.status.success(),
        "{}",
        String::from_utf8_lossy(&implicit.stderr)
    );
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    assert_eq!(implicit.stdout, explicit.stdout);
    assert_eq!(implicit.stdout, b"Read selected.txt\n");
}

#[test]
fn root_precedence_is_cli_then_tg_root() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cli_root = temp.path().join("cli");
    let env_root = temp.path().join("environment");
    fs::create_dir(&home).unwrap();
    create_root(&cli_root);
    create_root(&env_root);

    let output = resolve(command(&home).env("TG_ROOT", &env_root), &[]);
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
fn positional_file_and_explicit_root_are_allowed_but_directories_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("prompt.md");
    fs::write(&file, "prompt").unwrap();
    let output = command(temp.path())
        .args([
            file.to_str().unwrap(),
            "--root",
            temp.path().to_str().unwrap(),
            "--resolve",
            "plain",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = command(temp.path())
        .args([temp.path().to_str().unwrap(), "--resolve", "plain"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not a regular file"));
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

#[test]
fn headless_resolution_uses_loaded_custom_leaders() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = temp.path().join("root");
    fs::create_dir(&home).unwrap();
    create_root(&root);
    let config = temp.path().join("custom.toml");
    fs::write(&config, "[leaders]\nfiles='@@'\n").unwrap();

    let output = command(&home)
        .args([
            "--root",
            root.to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            "--resolve",
            "Read @@selected.txt",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"Read selected.txt\n");
}

#[test]
fn late_headless_failure_emits_no_partial_stdout() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let root = temp.path().join("root");
    fs::create_dir(&home).unwrap();
    create_root(&root);

    let output = command(&home)
        .args([
            "--root",
            root.to_str().unwrap(),
            "--resolve",
            "Read @selected.txt then @missing.txt",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no exact file reference"));
}
