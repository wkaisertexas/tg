use std::fs;
use std::path::{Path, PathBuf};

fn language_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/languages")
        .join(name)
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

#[test]
fn c_family_lookahead_handles_unicode_near_its_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("unicode.cpp");
    let source = format!(
        "struct Forward; // {}り\nstruct Defined {{}};\n",
        "x".repeat(487)
    );
    fs::write(&path, source).unwrap();
    let parsed = tscodeselection::language::parse(&path).unwrap().unwrap();
    assert!(
        parsed
            .symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "Defined")
    );
}

#[test]
fn detects_javascript_and_typescript_dialects() {
    for name in [
        "sample.js",
        "sample.mjs",
        "sample.cjs",
        "sample.jsx",
        "sample.ts",
        "sample.mts",
        "sample.cts",
        "sample.tsx",
        "Jakefile",
    ] {
        assert!(
            tscodeselection::language::supports(Path::new(name)),
            "{name}"
        );
    }
    assert!(!tscodeselection::language::supports(Path::new(
        "package.json"
    )));
}

#[test]
fn extracts_javascript_exports_classes_methods_and_arrows() {
    let parsed = tscodeselection::language::parse(&language_fixture("javascript.js"))
        .unwrap()
        .unwrap();
    let names: Vec<_> = parsed
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect();

    assert!(names.contains(&"render"));
    assert!(names.contains(&"stream"));
    assert!(names.contains(&"View"));
    assert!(names.contains(&"View::render"));
    assert!(names.contains(&"View::#reset"));
    assert!(names.contains(&"View::#cache"));
    assert!(names.contains(&"View::title"));
    assert!(names.contains(&"build"));
    assert!(names.contains(&"namedExpression"));
    assert!(!names.contains(&"View::constructor"));
    assert!(!parsed.symbols.iter().any(|symbol| matches!(
        symbol.leaf_name.as_str(),
        "nested" | "local" | "hidden" | "value"
    )));

    let members = tscodeselection::language::find_symbols(&parsed.symbols, "View.");
    assert!(members.iter().any(|symbol| symbol.leaf_name == "render"));
    assert_eq!(
        parsed
            .symbols
            .iter()
            .filter(|symbol| symbol.leaf_name == "render")
            .count(),
        2
    );
}

#[test]
fn jsx_extracts_components_but_not_markup_names() {
    let parsed = tscodeselection::language::parse(&language_fixture("component.jsx"))
        .unwrap()
        .unwrap();
    let names: Vec<_> = parsed
        .symbols
        .iter()
        .map(|symbol| symbol.leaf_name.as_str())
        .collect();
    assert!(names.contains(&"Card"));
    assert!(names.contains(&"Badge"));
    assert!(
        !names
            .iter()
            .any(|name| matches!(*name, "article" | "h1" | "span"))
    );
}

#[test]
fn extracts_typescript_structures_signatures_and_definition_preference() {
    let parsed = tscodeselection::language::parse(&language_fixture("typescript.ts"))
        .unwrap()
        .unwrap();
    let symbol = |qualified: &str| {
        parsed
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == qualified)
            .unwrap_or_else(|| panic!("missing {qualified}"))
    };

    assert_eq!(symbol("Api").kind, "module");
    assert_eq!(symbol("Api::request").kind, "function");
    assert_eq!(symbol("Api::Client").kind, "interface");
    assert_eq!(symbol("Api::Client::fetch").kind, "method");
    assert!(!symbol("Api::Client::fetch").is_definition);
    assert!(!symbol("Api::Client::endpoint").is_definition);
    assert_eq!(symbol("Result").kind, "type alias");
    assert_eq!(symbol("State::Ready").kind, "enum member");
    assert_eq!(symbol("State::Named").kind, "enum member");
    assert_eq!(symbol("Repository::load").kind, "method");
    assert!(!symbol("Repository::load").is_definition);
    assert!(symbol("Repository::save").is_definition);

    let overloads: Vec<_> = parsed
        .symbols
        .iter()
        .filter(|symbol| symbol.leaf_name == "parse")
        .collect();
    assert_eq!(overloads.len(), 3);
    assert_eq!(
        overloads
            .iter()
            .filter(|symbol| symbol.is_definition)
            .count(),
        1
    );
    assert!(
        tscodeselection::language::resolve_unique(&parsed.symbols, "parse")
            .unwrap()
            .is_definition
    );
}

#[test]
fn switches_between_typescript_and_tsx_and_preserves_unicode_byte_ranges() {
    let ts_source = fs::read_to_string(language_fixture("typescript.ts")).unwrap();
    let tsx_source = fs::read_to_string(language_fixture("component.tsx")).unwrap();
    let mut parser = tscodeselection::language::SymbolParser::new();

    let ts = parser
        .parse_source(Path::new("first.ts"), ts_source.clone())
        .unwrap()
        .unwrap();
    assert!(ts.symbols.iter().any(|symbol| symbol.leaf_name == "Client"));
    let tsx = parser
        .parse_source(Path::new("second.tsx"), tsx_source.clone())
        .unwrap()
        .unwrap();
    let cafe = tsx
        .symbols
        .iter()
        .find(|symbol| symbol.leaf_name == "Café")
        .unwrap();
    assert_eq!(
        &tsx.source[cafe.name_start_byte..cafe.name_end_byte],
        "Café"
    );
    assert!(tsx.symbols.iter().any(|symbol| symbol.leaf_name == "Panel"));
    assert!(!tsx.symbols.iter().any(|symbol| symbol.leaf_name == "main"));

    let mut reverse = tscodeselection::language::SymbolParser::new();
    reverse
        .parse_source(Path::new("first.tsx"), tsx_source)
        .unwrap()
        .unwrap();
    let reparsed_ts = reverse
        .parse_source(Path::new("second.ts"), ts_source)
        .unwrap()
        .unwrap();
    assert!(
        reparsed_ts
            .symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "Result")
    );
}

#[test]
fn malformed_tsx_retains_usable_symbols() {
    let parsed = tscodeselection::language::parse(&language_fixture("malformed.tsx"))
        .unwrap()
        .unwrap();
    assert!(
        parsed
            .symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "before")
    );
    assert!(
        parsed
            .symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "Broken")
    );
    assert!(
        parsed
            .symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "okay")
    );
    for symbol in &parsed.symbols {
        assert!(parsed.source.is_char_boundary(symbol.name_start_byte));
        assert!(parsed.source.is_char_boundary(symbol.name_end_byte));
        assert!(symbol.name_start_byte < symbol.name_end_byte);
    }
}
