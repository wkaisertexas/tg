use super::*;

#[test]
fn core_language_fixtures_cover_nested_duplicate_and_excluded_local_symbols() {
    let cases = [
        ("core.c", "Outer::Nested", "duplicate", &["local"][..]),
        ("core.cpp", "Outer::Nested", "duplicate", &["local"][..]),
        (
            "core.py",
            "Outer::Nested",
            "duplicate",
            &["local", "Hidden", "inner"][..],
        ),
        (
            "core.rs",
            "outer::Nested",
            "duplicate",
            &["Hidden", "local"][..],
        ),
        (
            "Sample.java",
            "Service::Inner",
            "render",
            &["Hidden", "local"][..],
        ),
        (
            "Sample.cs",
            "Acme::Tools::Service::Inner",
            "Render",
            &["Hidden", "Local"][..],
        ),
        (
            "sample.rb",
            "Acme::Service",
            "save",
            &["hidden", "local", "INNER"][..],
        ),
        (
            "javascript.js",
            "View::render",
            "render",
            &["nested", "local", "hidden", "value"][..],
        ),
        (
            "core.md",
            "Outer::Nested",
            "Duplicate",
            &["Hidden", "Still hidden"][..],
        ),
    ];
    for (name, nested, duplicate, excluded) in cases {
        let parsed = fixture(name);
        assert!(
            parsed
                .symbols
                .iter()
                .any(|symbol| symbol.qualified_name == nested),
            "{name}: missing nested symbol {nested}"
        );
        assert_eq!(
            parsed
                .symbols
                .iter()
                .filter(|symbol| symbol.leaf_name == duplicate)
                .count(),
            2,
            "{name}: expected duplicate symbol {duplicate}"
        );
        for excluded in excluded {
            assert!(
                !has_symbol(&parsed, excluded),
                "{name}: unexpected local symbol {excluded}"
            );
        }
        assert_valid_symbol_ranges(&parsed);
    }
}

#[test]
fn malformed_core_language_fixtures_retain_symbols_and_valid_ranges() {
    let cases = [
        ("malformed.c", &["before", "Broken"][..]),
        ("malformed.cpp", &["before", "Broken"][..]),
        ("malformed.py", &["before", "Broken"][..]),
        ("malformed.rs", &["before", "Broken"][..]),
        ("Malformed.java", &["Before", "Broken"][..]),
        ("malformed.js", &["before", "Broken"][..]),
        ("malformed.md", &["Before", "Broken"][..]),
    ];
    for (name, expected) in cases {
        let parsed = fixture(name);
        for expected in expected {
            assert!(has_symbol(&parsed, expected), "{name}: missing {expected}");
        }
        assert_valid_symbol_ranges(&parsed);
    }
}

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
        let parsed = language::parse(&path).unwrap().unwrap();
        assert!(has_symbol(&parsed, "Outer"), "{name}");
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
    let parsed = language::parse(&path).unwrap().unwrap();
    let macos = parsed
        .symbols
        .iter()
        .find(|symbol| symbol.leaf_name == "macOS")
        .unwrap();
    assert_eq!(macos.qualified_name, "Guide::Setup::macOS");
    assert_eq!(
        language::markdown_slug("Symbol-Aware File Selector"),
        "symbol-aware-file-selector"
    );
    assert_eq!((macos.start.line, macos.start.column), (6, 5));
    let members = language::find_symbols(&parsed.symbols, "Setup.");
    assert_eq!(members[0].leaf_name, "macOS");
}

#[test]
fn rust_impl_methods_are_available_through_dot_completion() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("model.rs");
    fs::write(&path, "struct User;\nimpl User { fn save(&self) {} }\n").unwrap();
    let parsed = language::parse(&path).unwrap().unwrap();
    let members = language::find_symbols(&parsed.symbols, "User.");
    assert_eq!(members[0].qualified_name, "User::save");
}

#[test]
fn c_family_lookahead_handles_unicode_near_its_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("unicode.cpp");
    let source = format!(
        "struct Forward; // {}り\nstruct Defined {{}};\n",
        "x".repeat(487)
    );
    fs::write(&path, source).unwrap();
    let parsed = language::parse(&path).unwrap().unwrap();
    assert!(has_symbol(&parsed, "Defined"));
}
