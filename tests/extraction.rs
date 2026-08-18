use std::fs;
use std::path::{Path, PathBuf};

fn language_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/languages")
        .join(name)
}

fn assert_valid_symbol_ranges(parsed: &tscodeselection::language::ParsedFile) {
    for symbol in &parsed.symbols {
        assert!(parsed.source.is_char_boundary(symbol.name_start_byte));
        assert!(parsed.source.is_char_boundary(symbol.name_end_byte));
        assert!(parsed.source.is_char_boundary(symbol.range_start_byte));
        assert!(parsed.source.is_char_boundary(symbol.range_end_byte));
        assert!(symbol.name_start_byte < symbol.name_end_byte);
        assert!(symbol.range_start_byte <= symbol.name_start_byte);
        assert!(symbol.name_end_byte <= symbol.range_end_byte);
    }
}

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

    for (fixture, nested, duplicate, excluded) in cases {
        let parsed = tscodeselection::language::parse(&language_fixture(fixture))
            .unwrap()
            .unwrap();
        assert!(
            parsed
                .symbols
                .iter()
                .any(|symbol| symbol.qualified_name == nested),
            "{fixture}: missing nested symbol {nested}"
        );
        assert_eq!(
            parsed
                .symbols
                .iter()
                .filter(|symbol| symbol.leaf_name == duplicate)
                .count(),
            2,
            "{fixture}: expected duplicate symbol {duplicate}"
        );
        for excluded in excluded {
            assert!(
                !parsed
                    .symbols
                    .iter()
                    .any(|symbol| symbol.leaf_name == *excluded),
                "{fixture}: unexpected local symbol {excluded}"
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

    for (fixture, expected) in cases {
        let parsed = tscodeselection::language::parse(&language_fixture(fixture))
            .unwrap()
            .unwrap();
        for expected in expected {
            assert!(
                parsed
                    .symbols
                    .iter()
                    .any(|symbol| symbol.leaf_name == *expected),
                "{fixture}: missing {expected}"
            );
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

#[test]
fn detects_go_and_java_sources_and_conventional_java_filenames() {
    for name in [
        "sample.go",
        "Sample.java",
        "module-info.java",
        "package-info.java",
    ] {
        assert!(
            tscodeselection::language::supports(Path::new(name)),
            "{name}"
        );
    }
    assert!(!tscodeselection::language::supports(Path::new("go.mod")));
    assert!(!tscodeselection::language::supports(Path::new(
        "Sample.class"
    )));
}

#[test]
fn extracts_go_types_methods_interfaces_and_unicode_ranges() {
    let parsed = tscodeselection::language::parse(&language_fixture("sample.go"))
        .unwrap()
        .unwrap();
    let symbol = |qualified: &str| {
        parsed
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == qualified)
            .unwrap_or_else(|| panic!("missing {qualified}"))
    };

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
    let members = tscodeselection::language::find_symbols(&parsed.symbols, "Service.");
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
    let parsed = tscodeselection::language::parse(&language_fixture("Sample.java"))
        .unwrap()
        .unwrap();
    let symbol = |qualified: &str| {
        parsed
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == qualified)
            .unwrap_or_else(|| panic!("missing {qualified}"))
    };

    assert_eq!(symbol("Service").kind, "class");
    assert_eq!(symbol("Service::name").kind, "field");
    assert_eq!(symbol("Service::alias").kind, "field");
    assert_eq!(symbol("Service::render").kind, "method");
    assert_eq!(symbol("Service::Inner").kind, "class");
    assert_eq!(symbol("Service::Inner::render").kind, "method");
    assert!(
        !parsed
            .symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "Hidden")
    );
    assert!(
        !parsed
            .symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "local")
    );
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
    let members = tscodeselection::language::find_symbols(&parsed.symbols, "Service.");
    assert!(members.iter().any(|symbol| symbol.leaf_name == "save"));
}

#[test]
fn switches_between_go_and_java_parsers_in_both_directions() {
    let go_source = fs::read_to_string(language_fixture("sample.go")).unwrap();
    let java_source = fs::read_to_string(language_fixture("Sample.java")).unwrap();
    let mut parser = tscodeselection::language::SymbolParser::new();
    let go = parser
        .parse_source(Path::new("first.go"), go_source.clone())
        .unwrap()
        .unwrap();
    assert!(
        go.symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "Service")
    );
    let java = parser
        .parse_source(Path::new("Second.java"), java_source.clone())
        .unwrap()
        .unwrap();
    assert!(
        java.symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "Reader")
    );

    let mut reverse = tscodeselection::language::SymbolParser::new();
    reverse
        .parse_source(Path::new("First.java"), java_source)
        .unwrap()
        .unwrap();
    let reparsed_go = reverse
        .parse_source(Path::new("second.go"), go_source)
        .unwrap()
        .unwrap();
    assert!(
        reparsed_go
            .symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "Alias")
    );
}

#[test]
fn malformed_go_and_java_retain_usable_symbols_and_valid_ranges() {
    for fixture in ["malformed.go", "Malformed.java"] {
        let parsed = tscodeselection::language::parse(&language_fixture(fixture))
            .unwrap()
            .unwrap();
        for expected in ["Before", "Broken"] {
            assert!(
                parsed
                    .symbols
                    .iter()
                    .any(|symbol| symbol.leaf_name == expected),
                "{fixture}: missing {expected}"
            );
        }
        for symbol in &parsed.symbols {
            assert!(parsed.source.is_char_boundary(symbol.name_start_byte));
            assert!(parsed.source.is_char_boundary(symbol.name_end_byte));
            assert!(symbol.name_start_byte < symbol.name_end_byte);
            assert!(symbol.range_start_byte <= symbol.name_start_byte);
            assert!(symbol.name_end_byte <= symbol.range_end_byte);
        }
    }
}

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
        assert!(
            tscodeselection::language::supports(Path::new(name)),
            "{name}"
        );
    }
    assert!(!tscodeselection::language::supports(Path::new(
        "Sample.dll"
    )));
}

#[test]
fn extracts_csharp_namespaces_types_members_fields_and_definition_semantics() {
    let parsed = tscodeselection::language::parse(&language_fixture("Sample.cs"))
        .unwrap()
        .unwrap();
    let symbol = |qualified: &str| {
        parsed
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == qualified)
            .unwrap_or_else(|| {
                panic!(
                    "missing {qualified}; got {:?}",
                    parsed
                        .symbols
                        .iter()
                        .map(|symbol| symbol.qualified_name.as_str())
                        .collect::<Vec<_>>()
                )
            })
    };

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
    let parsed = tscodeselection::language::parse(&language_fixture("sample.rb"))
        .unwrap()
        .unwrap();
    let symbol = |qualified: &str| {
        parsed
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == qualified)
            .unwrap_or_else(|| panic!("missing {qualified}"))
    };

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
    let mut parser = tscodeselection::language::SymbolParser::new();
    let csharp = parser
        .parse_source(Path::new("First.cs"), csharp_source.clone())
        .unwrap()
        .unwrap();
    assert!(
        csharp
            .symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "Service")
    );
    let ruby = parser
        .parse_source(Path::new("second.rb"), ruby_source.clone())
        .unwrap()
        .unwrap();
    assert!(
        ruby.symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "DEFAULT")
    );

    let mut reverse = tscodeselection::language::SymbolParser::new();
    reverse
        .parse_source(Path::new("first.rb"), ruby_source)
        .unwrap()
        .unwrap();
    let reparsed_csharp = reverse
        .parse_source(Path::new("Second.cs"), csharp_source)
        .unwrap()
        .unwrap();
    assert!(
        reparsed_csharp
            .symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "Handler")
    );
}

#[test]
fn malformed_csharp_and_ruby_retain_usable_symbols_and_valid_ranges() {
    for fixture in ["malformed.cs", "malformed.rb"] {
        let parsed = tscodeselection::language::parse(&language_fixture(fixture))
            .unwrap()
            .unwrap();
        for expected in ["Before", "Broken"] {
            assert!(
                parsed
                    .symbols
                    .iter()
                    .any(|symbol| symbol.leaf_name == expected),
                "{fixture}: missing {expected}"
            );
        }
        for symbol in &parsed.symbols {
            assert!(parsed.source.is_char_boundary(symbol.name_start_byte));
            assert!(parsed.source.is_char_boundary(symbol.name_end_byte));
            assert!(symbol.name_start_byte < symbol.name_end_byte);
            assert!(symbol.range_start_byte <= symbol.name_start_byte);
            assert!(symbol.name_end_byte <= symbol.range_end_byte);
        }
    }
}

#[test]
fn detects_shell_sources_and_conventional_filenames() {
    for name in [
        "script.sh",
        "script.bash",
        ".bashrc",
        ".bash_profile",
        ".bash_login",
        ".bash_logout",
        ".profile",
        "bash.bashrc",
        "profile",
        "PKGBUILD",
        "APKBUILD",
    ] {
        assert!(
            tscodeselection::language::supports(Path::new(name)),
            "{name}"
        );
    }
    for name in ["deploy", ".zshrc", "script.zsh", "script.fish"] {
        assert!(
            !tscodeselection::language::supports(Path::new(name)),
            "{name}"
        );
    }
}

#[test]
fn detects_extensionless_shell_sources_from_shebang() {
    for source in [
        "#!/bin/sh\nrun() { :; }\n",
        "#!/usr/bin/bash\nrun() { :; }\n",
        "#!/usr/bin/env bash\nrun() { :; }\n",
        "#!/usr/bin/env -S bash -eu\nrun() { :; }\n",
        "#!/bin/dash\nrun() { :; }\n",
    ] {
        let parsed =
            tscodeselection::language::parse_source(Path::new("deploy"), source.to_owned())
                .unwrap()
                .unwrap();
        assert_eq!(parsed.symbols[0].leaf_name, "run", "{source}");
    }

    for source in [
        "run() { :; }\n",
        "#!/usr/bin/env python\nrun() { :; }\n",
        "#!/usr/bin/zsh\nrun() { :; }\n",
    ] {
        assert!(
            tscodeselection::language::parse_source(Path::new("deploy"), source.to_owned(),)
                .unwrap()
                .is_none(),
            "{source}"
        );
    }
}

#[test]
fn extracts_shell_functions_variables_nested_units_and_ranges() {
    let parsed = tscodeselection::language::parse(&language_fixture("sample.sh"))
        .unwrap()
        .unwrap();
    let symbol = |qualified: &str| {
        parsed
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == qualified)
            .unwrap_or_else(|| panic!("missing {qualified}"))
    };
    let slice = |symbol: &tscodeselection::language::Symbol| {
        &parsed.source[symbol.range_start_byte..symbol.range_end_byte]
    };

    assert_eq!(symbol("GLOBAL").kind, "variable");
    assert_eq!(slice(symbol("GLOBAL")), "GLOBAL=\"日本語\"");
    assert_eq!(slice(symbol("EXPORTED")), "export EXPORTED=2");
    assert_eq!(slice(symbol("READONLY")), "readonly READONLY=3");
    assert_eq!(symbol("posix_fn").kind, "function");
    assert_eq!(symbol("bash_fn").kind, "function");
    assert_eq!(
        slice(symbol("redirected")),
        "redirected() { :; } >\"$TMPDIR/out\""
    );
    assert_eq!(symbol("outer::inner").leaf_name, "inner");
    assert_eq!(symbol("one::child").leaf_name, "child");
    assert_eq!(symbol("two::child").leaf_name, "child");
    assert_eq!(
        parsed
            .symbols
            .iter()
            .filter(|symbol| symbol.leaf_name == "render")
            .count(),
        2
    );

    for excluded in ["LOCAL", "FOO", "A", "B", "ARRAY", "item", "LOOP"] {
        assert!(
            !parsed
                .symbols
                .iter()
                .any(|symbol| symbol.leaf_name == excluded),
            "unexpected shell symbol {excluded}"
        );
    }
    for symbol in &parsed.symbols {
        assert!(parsed.source.is_char_boundary(symbol.name_start_byte));
        assert!(parsed.source.is_char_boundary(symbol.name_end_byte));
        assert!(parsed.source.is_char_boundary(symbol.range_start_byte));
        assert!(parsed.source.is_char_boundary(symbol.range_end_byte));
        assert!(symbol.range_start_byte <= symbol.name_start_byte);
        assert!(symbol.name_end_byte <= symbol.range_end_byte);
    }
}

#[test]
fn malformed_shell_retains_usable_symbols_and_skips_missing_names() {
    let parsed = tscodeselection::language::parse(&language_fixture("malformed.sh"))
        .unwrap()
        .unwrap();
    for expected in ["before", "broken"] {
        assert!(
            parsed
                .symbols
                .iter()
                .any(|symbol| symbol.leaf_name == expected),
            "missing {expected}"
        );
    }

    let malformed = tscodeselection::language::parse_source(
        Path::new("malformed.sh"),
        "function () {\n".to_owned(),
    )
    .unwrap()
    .unwrap();
    assert!(
        malformed
            .symbols
            .iter()
            .all(|symbol| !symbol.leaf_name.is_empty())
    );
}

#[test]
fn switches_between_shell_and_ruby_parsers_in_both_directions() {
    let shell_source = fs::read_to_string(language_fixture("sample.sh")).unwrap();
    let ruby_source = fs::read_to_string(language_fixture("sample.rb")).unwrap();
    let mut parser = tscodeselection::language::SymbolParser::new();
    let shell = parser
        .parse_source(Path::new("first.sh"), shell_source.clone())
        .unwrap()
        .unwrap();
    assert!(
        shell
            .symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "outer")
    );
    let ruby = parser
        .parse_source(Path::new("second.rb"), ruby_source.clone())
        .unwrap()
        .unwrap();
    assert!(
        ruby.symbols
            .iter()
            .any(|symbol| symbol.leaf_name == "DEFAULT")
    );

    let mut reverse = tscodeselection::language::SymbolParser::new();
    reverse
        .parse_source(Path::new("first.rb"), ruby_source)
        .unwrap()
        .unwrap();
    let reparsed_shell = reverse
        .parse_source(Path::new("second.sh"), shell_source)
        .unwrap()
        .unwrap();
    assert!(
        reparsed_shell
            .symbols
            .iter()
            .any(|symbol| symbol.qualified_name == "outer::inner")
    );
}
