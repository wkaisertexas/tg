use std::fs;

#[test]
fn lowers_a_markdown_section_reference() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("guide.md"), "# Guide\n\n## Quick Start\n").unwrap();
    assert_eq!(
        tscodeselection::composer::resolve_prompt(temp.path(), "Follow @guide.md::QuickStart")
            .unwrap(),
        "Follow guide.md#quick-start"
    );
}
