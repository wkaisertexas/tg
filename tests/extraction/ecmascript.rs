use super::*;

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
        assert!(language::supports(Path::new(name)), "{name}");
    }
    assert!(!language::supports(Path::new("package.json")));
}

#[test]
fn extracts_javascript_exports_classes_methods_and_arrows() {
    let parsed = fixture("javascript.js");
    let names: Vec<_> = parsed
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect();
    for name in [
        "render",
        "stream",
        "View",
        "View::render",
        "View::#reset",
        "View::#cache",
        "View::title",
        "build",
        "namedExpression",
    ] {
        assert!(names.contains(&name), "missing {name}");
    }
    assert!(!names.contains(&"View::constructor"));
    assert!(!parsed.symbols.iter().any(|symbol| matches!(
        symbol.leaf_name.as_str(),
        "nested" | "local" | "hidden" | "value"
    )));
    let members = language::find_symbols(&parsed.symbols, "View.");
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
    let parsed = fixture("component.jsx");
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
    let parsed = fixture("typescript.ts");
    let symbol = |name| named_symbol(&parsed, name);
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
        language::resolve_unique(&parsed.symbols, "parse")
            .unwrap()
            .is_definition
    );
}

#[test]
fn switches_between_typescript_and_tsx_and_preserves_unicode_byte_ranges() {
    let ts_source = fs::read_to_string(language_fixture("typescript.ts")).unwrap();
    let tsx_source = fs::read_to_string(language_fixture("component.tsx")).unwrap();
    let mut parser = language::SymbolParser::new();
    let ts = parser
        .parse_source(Path::new("first.ts"), ts_source.clone())
        .unwrap()
        .unwrap();
    assert!(has_symbol(&ts, "Client"));
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
    assert!(has_symbol(&tsx, "Panel"));
    assert!(!has_symbol(&tsx, "main"));
    let mut reverse = language::SymbolParser::new();
    reverse
        .parse_source(Path::new("first.tsx"), tsx_source)
        .unwrap()
        .unwrap();
    let reparsed_ts = reverse
        .parse_source(Path::new("second.ts"), ts_source)
        .unwrap()
        .unwrap();
    assert!(has_symbol(&reparsed_ts, "Result"));
}

#[test]
fn malformed_tsx_retains_usable_symbols() {
    let parsed = fixture("malformed.tsx");
    for name in ["before", "Broken", "okay"] {
        assert!(has_symbol(&parsed, name));
    }
    for symbol in &parsed.symbols {
        assert!(parsed.source.is_char_boundary(symbol.name_start_byte));
        assert!(parsed.source.is_char_boundary(symbol.name_end_byte));
        assert!(symbol.name_start_byte < symbol.name_end_byte);
    }
}
