use std::fs;

#[test]
fn duplicate_symbol_names_prefer_a_definition() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("duplicate.cpp");
    fs::write(&path, "void render();\nvoid render() { }\n").unwrap();

    let parsed = tscodeselection::language::parse(&path).unwrap().unwrap();
    let selected = tscodeselection::language::resolve_unique(&parsed.symbols, "render").unwrap();
    assert!(selected.is_definition);
    assert_eq!(selected.start.line, 2);
}

#[test]
fn unresolved_and_unsupported_references_do_not_modify_the_source_prompt() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("plain.txt"), "plain\n").unwrap();
    let missing = "Read @missing.txt exactly";
    let unsupported = "Read @plain.txt::section exactly";

    assert!(tscodeselection::composer::resolve_prompt(temp.path(), missing).is_err());
    assert!(tscodeselection::composer::resolve_prompt(temp.path(), unsupported).is_err());
    assert_eq!(missing, "Read @missing.txt exactly");
    assert_eq!(unsupported, "Read @plain.txt::section exactly");
}

#[test]
fn token_boundaries_and_escaped_looking_text_match_v01_behavior() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("notes.txt"), "notes\n").unwrap();

    assert_eq!(
        tscodeselection::composer::resolve_prompt(
            temp.path(),
            "email@example.com (@notes.txt), x@notes.txt and \\@notes.txt",
        )
        .unwrap(),
        "email@example.com (notes.txt), x@notes.txt and \\@notes.txt"
    );
}
