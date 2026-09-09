use super::*;

#[test]
fn detects_csharp_and_ruby_sources_and_conventional_ruby_filenames() {
    for name in [
        "Sample.cs",
        "sample.rb",
        "tasks.rake",
        "example.gemspec",
        "config.ru",
        "Gemfile",
        "Rakefile",
        "Guardfile",
        "Vagrantfile",
        "Podfile",
        "Fastfile",
        "Appfile",
        "Dangerfile",
        "Berksfile",
        "Capfile",
    ] {
        assert!(language::supports(Path::new(name)), "{name}");
    }
    assert!(!language::supports(Path::new("Sample.dll")));
}

#[test]
fn extracts_csharp_namespaces_types_members_fields_and_definition_semantics() {
    let parsed = fixture("Sample.cs");
    let symbol = |name| named_symbol(&parsed, name);
    assert_eq!(symbol("Acme::Tools").kind, "module");
    assert_eq!(symbol("Acme::Tools::IReader").kind, "interface");
    assert!(!symbol("Acme::Tools::IReader::Name").is_definition);
    assert!(!symbol("Acme::Tools::IReader::Read").is_definition);
    assert!(symbol("Acme::Tools::IReader::Close").is_definition);
    assert_eq!(symbol("Acme::Tools::Service").kind, "class");
    assert_eq!(symbol("Acme::Tools::Service::name").kind, "field");
    assert_eq!(symbol("Acme::Tools::Service::alias").kind, "field");
    assert_eq!(symbol("Acme::Tools::Service::Title").kind, "property");
    assert_eq!(symbol("Acme::Tools::Service::Service").kind, "method");
    assert!(symbol("Acme::Tools::Service::Render").is_definition);
    assert!(!symbol("Acme::Tools::Service::Missing").is_definition);
    assert_eq!(symbol("Acme::Tools::Service::Inner").kind, "class");
    assert!(symbol("Acme::Tools::Service::Inner::Save").is_definition);
    assert_eq!(symbol("Acme::Tools::State::Ready").kind, "enum member");
    assert_eq!(symbol("Acme::Tools::State::Named").kind, "enum member");
    assert_eq!(symbol("Acme::Tools::Handler").kind, "delegate");
    assert!(
        !parsed
            .symbols
            .iter()
            .any(|symbol| matches!(symbol.leaf_name.as_str(), "Hidden" | "Local" | "value"))
    );
    let cafe = symbol("Acme::Tools::Café");
    assert_eq!(
        &parsed.source[cafe.name_start_byte..cafe.name_end_byte],
        "Café"
    );
    assert_eq!(symbol("Acme::Tools::Café::Crème").kind, "field");
    let field = symbol("Acme::Tools::Service::alias");
    let declaration = &parsed.source[field.range_start_byte..field.range_end_byte];
    assert!(declaration.contains("name"));
    assert!(declaration.contains("alias"));
}

#[test]
fn extracts_ruby_scopes_methods_operators_aliases_and_constants() {
    let parsed = fixture("sample.rb");
    let symbol = |name| named_symbol(&parsed, name);
    assert_eq!(symbol("Acme").kind, "module");
    assert_eq!(symbol("Acme::Service").kind, "class");
    assert_eq!(symbol("Acme::Service::DEFAULT").kind, "constant");
    assert_eq!(symbol("Acme::Service::render").kind, "method");
    assert_eq!(symbol("Acme::Service::title=").kind, "method");
    assert_eq!(symbol("Acme::Service::[]").kind, "method");
    assert_eq!(symbol("Acme::Service::build").kind, "method");
    assert_eq!(symbol("Acme::Service::start").kind, "method");
    assert_eq!(symbol("Admin::User").leaf_name, "User");
    assert_eq!(symbol("Admin::User::VALUE").kind, "constant");
    assert_eq!(symbol("Admin::User::save").kind, "method");
    assert_eq!(symbol("Widget::create").kind, "method");
    assert_eq!(symbol("TOP_LEVEL").kind, "constant");
    assert_eq!(symbol("Admin::EXPLICIT").kind, "constant");
    assert!(!parsed.symbols.iter().any(|symbol| matches!(
        symbol.leaf_name.as_str(),
        "local" | "hidden" | "INNER" | "value" | "key"
    )));
    let cafe = symbol("Café");
    assert_eq!(
        &parsed.source[cafe.name_start_byte..cafe.name_end_byte],
        "Café"
    );
    assert_eq!(symbol("Café::CRÈME").kind, "constant");
    assert!(parsed.symbols.iter().all(|symbol| symbol.is_definition));
}

#[test]
fn switches_between_csharp_and_ruby_parsers_in_both_directions() {
    let csharp_source = fs::read_to_string(language_fixture("Sample.cs")).unwrap();
    let ruby_source = fs::read_to_string(language_fixture("sample.rb")).unwrap();
    let mut parser = language::SymbolParser::new();
    let csharp = parser
        .parse_source(Path::new("First.cs"), csharp_source.clone())
        .unwrap()
        .unwrap();
    assert!(has_symbol(&csharp, "Service"));
    let ruby = parser
        .parse_source(Path::new("second.rb"), ruby_source.clone())
        .unwrap()
        .unwrap();
    assert!(has_symbol(&ruby, "DEFAULT"));
    let mut reverse = language::SymbolParser::new();
    reverse
        .parse_source(Path::new("first.rb"), ruby_source)
        .unwrap()
        .unwrap();
    let reparsed_csharp = reverse
        .parse_source(Path::new("Second.cs"), csharp_source)
        .unwrap()
        .unwrap();
    assert!(has_symbol(&reparsed_csharp, "Handler"));
}

#[test]
fn malformed_csharp_and_ruby_retain_usable_symbols_and_valid_ranges() {
    for name in ["malformed.cs", "malformed.rb"] {
        let parsed = fixture(name);
        for expected in ["Before", "Broken"] {
            assert!(has_symbol(&parsed, expected), "{name}: missing {expected}");
        }
        assert_valid_symbol_ranges(&parsed);
    }
}
