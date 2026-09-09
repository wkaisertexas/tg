use super::*;

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
        assert!(language::supports(Path::new(name)), "{name}");
    }
    for name in ["deploy", ".zshrc", "script.zsh", "script.fish"] {
        assert!(!language::supports(Path::new(name)), "{name}");
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
        let parsed = language::parse_source(Path::new("deploy"), source.to_owned())
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
            language::parse_source(Path::new("deploy"), source.to_owned())
                .unwrap()
                .is_none(),
            "{source}"
        );
    }
}

#[test]
fn extracts_shell_functions_variables_nested_units_and_ranges() {
    let parsed = fixture("sample.sh");
    let symbol = |name| named_symbol(&parsed, name);
    let slice =
        |symbol: &language::Symbol| &parsed.source[symbol.range_start_byte..symbol.range_end_byte];
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
            !has_symbol(&parsed, excluded),
            "unexpected shell symbol {excluded}"
        );
    }
    assert_valid_symbol_ranges(&parsed);
}

#[test]
fn malformed_shell_retains_usable_symbols_and_skips_missing_names() {
    let parsed = fixture("malformed.sh");
    for expected in ["before", "broken"] {
        assert!(has_symbol(&parsed, expected), "missing {expected}");
    }
    let malformed = language::parse_source(Path::new("malformed.sh"), "function () {\n".to_owned())
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
    let mut parser = language::SymbolParser::new();
    let shell = parser
        .parse_source(Path::new("first.sh"), shell_source.clone())
        .unwrap()
        .unwrap();
    assert!(has_symbol(&shell, "outer"));
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
