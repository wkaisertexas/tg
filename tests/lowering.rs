use std::fs;

#[test]
fn lowers_multiple_references_without_eating_punctuation() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("model.rs"), "pub struct User;\n").unwrap();
    fs::write(temp.path().join("notes.txt"), "notes\n").unwrap();
    let prompt = "Compare @model.rs::User with %notes.txt.";
    assert_eq!(
        tscodeselection::composer::resolve_prompt(temp.path(), prompt).unwrap(),
        "Compare model.rs:1:12 with notes.txt."
    );
}
