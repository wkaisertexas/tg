use std::fs;
use std::path::{Path, PathBuf};
use tscodeselection::language;

mod extraction {
    use super::*;
    mod core;
    mod csharp_ruby;
    mod ecmascript;
    mod go_java;
    mod shell;
}

fn language_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/languages")
        .join(name)
}

fn fixture(name: &str) -> language::ParsedFile {
    language::parse(&language_fixture(name)).unwrap().unwrap()
}

fn named_symbol<'a>(parsed: &'a language::ParsedFile, qualified: &str) -> &'a language::Symbol {
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
}

fn has_symbol(parsed: &language::ParsedFile, name: &str) -> bool {
    parsed.symbols.iter().any(|symbol| symbol.leaf_name == name)
}

fn assert_valid_symbol_ranges(parsed: &language::ParsedFile) {
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
