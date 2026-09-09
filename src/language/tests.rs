use super::extract::supplement_c_family_declarations;
use super::markdown::parse_markdown_headings;
use super::*;

#[test]
fn points_are_one_based() {
    assert_eq!(
        SourcePoint::from(Point { row: 4, column: 7 }),
        SourcePoint { line: 5, column: 8 }
    );
}

#[test]
fn declaration_range_is_distinct_from_unicode_name_location() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("unicode.rs");
    let source = "// λ\npub fn naïve(value: usize) -> usize {\n    value + 1\n}\n";
    std::fs::write(&path, source).unwrap();
    let parsed = parse(&path).unwrap().unwrap();
    let symbol = parsed
        .symbols
        .iter()
        .find(|symbol| symbol.leaf_name == "naïve")
        .unwrap();
    assert_eq!(
        source.get(symbol.name_start_byte..symbol.name_end_byte),
        Some("naïve")
    );
    let declaration = source
        .get(symbol.range_start_byte..symbol.range_end_byte)
        .unwrap();
    assert!(declaration.starts_with("pub fn naïve"), "{declaration:?}");
    assert!(declaration.ends_with('}'), "{declaration:?}");
    assert!(symbol.range_start_byte < symbol.name_start_byte);
    assert!(symbol.name_end_byte < symbol.range_end_byte);
    assert_eq!(symbol.start, SourcePoint { line: 2, column: 8 });
    assert_eq!(
        lowered_reference("unicode.rs", symbol),
        "unicode.rs::2:8 naïve"
    );
}

#[test]
fn markdown_ranges_cover_heading_syntax_not_only_the_name() {
    let source = "  ## Héading ##\nbody\nSetext title\n---\n";
    let symbols = parse_markdown_headings(source);
    let atx = &symbols[0];
    assert_eq!(
        source.get(atx.name_start_byte..atx.name_end_byte),
        Some("Héading")
    );
    assert_eq!(
        source.get(atx.range_start_byte..atx.range_end_byte),
        Some("## Héading ##")
    );
    assert_eq!(atx.start, SourcePoint { line: 1, column: 6 });
    let setext = &symbols[1];
    assert_eq!(
        source.get(setext.name_start_byte..setext.name_end_byte),
        Some("Setext title")
    );
    assert_eq!(
        source.get(setext.range_start_byte..setext.range_end_byte),
        Some("Setext title\n---")
    );
    assert_eq!(setext.start, SourcePoint { line: 3, column: 1 });
}

#[test]
fn supplemented_c_declarations_use_the_source_line_as_the_range() {
    let source = "  struct Widget; // declaration\n";
    let mut symbols = Vec::new();
    assert!(supplement_c_family_declarations(source, &mut symbols));
    let symbol = &symbols[0];
    assert_eq!(
        source.get(symbol.name_start_byte..symbol.name_end_byte),
        Some("Widget")
    );
    assert_eq!(
        source.get(symbol.range_start_byte..symbol.range_end_byte),
        Some("struct Widget; // declaration")
    );
    assert_eq!(
        symbol.start,
        SourcePoint {
            line: 1,
            column: 10
        }
    );
    assert!(!symbol.is_definition);
}
