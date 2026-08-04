use std::fs;

#[test]
fn extracts_declarations_from_all_supported_languages() {
    let temp = tempfile::tempdir().unwrap();
    let cases = [
        (
            "sample.py",
            "class Outer:\n    def method(self):\n        local = 1\n",
        ),
        (
            "sample.rs",
            "struct Outer { field: i32 }\nimpl Outer { fn method(&self) {} }\n",
        ),
        (
            "sample.c",
            "struct Outer { int field; };\nint function(int parameter) { int local = 1; return local; }\n",
        ),
        (
            "sample.cpp",
            "namespace n { class Outer { int field; void method(); }; }\n",
        ),
        (
            "sample.md",
            "# Outer\n\n## method\n\n### Details\n\n```md\n# Not a heading\n```\n",
        ),
    ];
    for (name, source) in cases {
        let path = temp.path().join(name);
        fs::write(&path, source).unwrap();
        let parsed = tscodeselection::language::parse(&path).unwrap().unwrap();
        assert!(
            parsed
                .symbols
                .iter()
                .any(|symbol| symbol.leaf_name == "Outer"),
            "{name}"
        );
        assert!(
            !parsed
                .symbols
                .iter()
                .any(|symbol| symbol.leaf_name == "local" || symbol.leaf_name == "parameter"),
            "{name}"
        );
    }
}

#[test]
fn markdown_headings_are_hierarchical_and_support_dot_completion() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("guide.md");
    fs::write(&path, "Guide\n=====\n\n## Setup\n\n### macOS\n").unwrap();
    let parsed = tscodeselection::language::parse(&path).unwrap().unwrap();
    let macos = parsed
        .symbols
        .iter()
        .find(|symbol| symbol.leaf_name == "macOS")
        .unwrap();
    assert_eq!(macos.qualified_name, "Guide::Setup::macOS");
    assert_eq!(
        tscodeselection::language::markdown_slug("Symbol-Aware File Selector"),
        "symbol-aware-file-selector"
    );
    assert_eq!((macos.start.line, macos.start.column), (6, 5));
    let members = tscodeselection::language::find_symbols(&parsed.symbols, "Setup.");
    assert_eq!(members[0].leaf_name, "macOS");
}

#[test]
fn rust_impl_methods_are_available_through_dot_completion() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("model.rs");
    fs::write(&path, "struct User;\nimpl User { fn save(&self) {} }\n").unwrap();
    let parsed = tscodeselection::language::parse(&path).unwrap().unwrap();
    let members = tscodeselection::language::find_symbols(&parsed.symbols, "User.");
    assert_eq!(members[0].qualified_name, "User::save");
}
