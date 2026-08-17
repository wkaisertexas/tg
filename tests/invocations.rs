use serde::Deserialize;
use std::process::Command;

#[derive(Deserialize)]
struct Document {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    submitted_prompt: String,
    selected: Selected,
    expected: Expected,
}

#[derive(Deserialize)]
struct Selected {
    file: String,
    symbol: String,
    line: usize,
    column: usize,
}

#[derive(Deserialize)]
struct Expected {
    stdout: String,
    preview: ExpectedPreview,
}

#[derive(Deserialize)]
struct ExpectedPreview {
    first_line: usize,
    last_line: usize,
    focus_line: usize,
}

#[test]
fn every_documented_invocation_resolves_end_to_end() {
    let document: Document =
        serde_yaml::from_str(include_str!("../examples/invocations.yaml")).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let home = tempfile::tempdir().unwrap();
    for case in document.cases {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tg"));
        isolate_config(&mut command, home.path());
        let output = command
            .current_dir(root)
            .args(["--root", ".", "--resolve", &case.submitted_prompt])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}: {}",
            case.id,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            case.expected.stdout,
            "{}",
            case.id
        );

        let parsed = tscodeselection::language::parse(&root.join(&case.selected.file))
            .unwrap()
            .unwrap();
        let symbol = parsed
            .symbols
            .iter()
            .find(|symbol| {
                symbol.leaf_name == case.selected.symbol
                    && symbol.start.line == case.selected.line
                    && symbol.start.column == case.selected.column
            })
            .unwrap_or_else(|| panic!("{}: expected symbol was not extracted", case.id));
        let preview = tscodeselection::preview::window(&parsed, symbol, 5);
        assert_eq!(
            preview.first().unwrap().0,
            case.expected.preview.first_line,
            "{}",
            case.id
        );
        assert_eq!(
            preview.last().unwrap().0,
            case.expected.preview.last_line,
            "{}",
            case.id
        );
        assert!(
            preview
                .iter()
                .any(|(line, _)| *line == case.expected.preview.focus_line)
        );
    }
}

fn isolate_config(command: &mut Command, home: &std::path::Path) {
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
}
