use super::*;

#[test]
fn detects_go_and_java_sources_and_conventional_java_filenames() {
    for name in [
        "sample.go",
        "Sample.java",
        "module-info.java",
        "package-info.java",
    ] {
        assert!(language::supports(Path::new(name)), "{name}");
    }
    assert!(!language::supports(Path::new("go.mod")));
    assert!(!language::supports(Path::new("Sample.class")));
}

#[test]
fn extracts_go_types_methods_interfaces_and_unicode_ranges() {
    let parsed = fixture("sample.go");
    let symbol = |name| named_symbol(&parsed, name);
    assert_eq!(symbol("Service").kind, "struct");
    assert_eq!(symbol("Service::Name").kind, "field");
    assert_eq!(symbol("Service::Count").kind, "field");
    assert_eq!(symbol("Service::Total").kind, "field");
    assert_eq!(symbol("Service::Embedded").kind, "field");
    assert_eq!(symbol("Default").kind, "constant");
    assert_eq!(symbol("Alternate").kind, "constant");
    assert_eq!(symbol("Current").kind, "variable");
    assert_eq!(symbol("Reader").kind, "interface");
    assert_eq!(symbol("Reader::Read").kind, "method");
    assert!(!symbol("Reader::Read").is_definition);
    assert_eq!(symbol("Alias").kind, "type alias");
    assert_eq!(symbol("Identifier").kind, "type");
    assert_eq!(symbol("Service::Save").kind, "method");
    assert_eq!(symbol("Service::Render").kind, "method");
    assert_eq!(symbol("Render").kind, "function");
    assert!(
        !parsed
            .symbols
            .iter()
            .any(|symbol| matches!(symbol.leaf_name.as_str(), "local" | "Hidden"))
    );
    let cafe = symbol("Café");
    assert_eq!(
        &parsed.source[cafe.name_start_byte..cafe.name_end_byte],
        "Café"
    );
    assert_eq!(symbol("Café::Crème").kind, "field");
    let members = language::find_symbols(&parsed.symbols, "Service.");
    assert!(members.iter().any(|symbol| symbol.leaf_name == "Render"));
    assert_eq!(
        parsed
            .symbols
            .iter()
            .filter(|symbol| symbol.leaf_name == "Render")
            .count(),
        2
    );
}

#[test]
fn extracts_java_nested_types_fields_signatures_and_records() {
    let parsed = fixture("Sample.java");
    let symbol = |name| named_symbol(&parsed, name);
    assert_eq!(symbol("Service").kind, "class");
    assert_eq!(symbol("Service::name").kind, "field");
    assert_eq!(symbol("Service::alias").kind, "field");
    assert_eq!(symbol("Service::render").kind, "method");
    assert_eq!(symbol("Service::Inner").kind, "class");
    assert_eq!(symbol("Service::Inner::render").kind, "method");
    assert!(!has_symbol(&parsed, "Hidden"));
    assert!(!has_symbol(&parsed, "local"));
    assert_eq!(symbol("Reader").kind, "interface");
    assert!(!symbol("Reader::read").is_definition);
    assert!(symbol("Reader::close").is_definition);
    assert_eq!(symbol("State::READY").kind, "enum member");
    assert_eq!(symbol("State::NAMED").kind, "enum member");
    assert_eq!(symbol("Result").kind, "record");
    assert_eq!(symbol("Result::value").kind, "field");
    assert_eq!(symbol("Result::unwrap").kind, "method");
    assert_eq!(symbol("Marker").kind, "annotation");
    assert!(!symbol("Marker::value").is_definition);
    assert!(
        !parsed
            .symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "Service" && symbol.kind == "method")
    );
    let cafe = symbol("Café");
    assert_eq!(
        &parsed.source[cafe.name_start_byte..cafe.name_end_byte],
        "Café"
    );
    assert_eq!(symbol("Café::crème").kind, "field");
    assert_eq!(
        parsed
            .symbols
            .iter()
            .filter(|symbol| symbol.leaf_name == "render")
            .count(),
        2
    );
    let members = language::find_symbols(&parsed.symbols, "Service.");
    assert!(members.iter().any(|symbol| symbol.leaf_name == "save"));
}

#[test]
fn switches_between_go_and_java_parsers_in_both_directions() {
    let go_source = fs::read_to_string(language_fixture("sample.go")).unwrap();
    let java_source = fs::read_to_string(language_fixture("Sample.java")).unwrap();
    let mut parser = language::SymbolParser::new();
    let go = parser
        .parse_source(Path::new("first.go"), go_source.clone())
        .unwrap()
        .unwrap();
    assert!(has_symbol(&go, "Service"));
    let java = parser
        .parse_source(Path::new("Second.java"), java_source.clone())
        .unwrap()
        .unwrap();
    assert!(has_symbol(&java, "Reader"));
    let mut reverse = language::SymbolParser::new();
    reverse
        .parse_source(Path::new("First.java"), java_source)
        .unwrap()
        .unwrap();
    let reparsed_go = reverse
        .parse_source(Path::new("second.go"), go_source)
        .unwrap()
        .unwrap();
    assert!(has_symbol(&reparsed_go, "Alias"));
}

#[test]
fn malformed_go_and_java_retain_usable_symbols_and_valid_ranges() {
    for name in ["malformed.go", "Malformed.java"] {
        let parsed = fixture(name);
        for expected in ["Before", "Broken"] {
            assert!(has_symbol(&parsed, expected), "{name}: missing {expected}");
        }
        assert_valid_symbol_ranges(&parsed);
    }
}
