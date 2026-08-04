use anyhow::{Context, Result};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use std::path::Path;
use tree_sitter::{Language, Node, Parser, Point};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePoint {
    pub line: usize,
    pub column: usize,
}

impl From<Point> for SourcePoint {
    fn from(value: Point) -> Self {
        Self {
            line: value.row + 1,
            column: value.column + 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub leaf_name: String,
    pub qualified_name: String,
    pub kind: String,
    pub start: SourcePoint,
    pub start_byte: usize,
    pub end_byte: usize,
    pub is_definition: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedFile {
    pub source: String,
    pub symbols: Vec<Symbol>,
}

#[derive(Clone, Copy)]
enum Flavor {
    C,
    Cpp,
    Rust,
    Python,
}

fn grammar(path: &Path) -> Option<(Language, Flavor)> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "c" => Some((tree_sitter_c::LANGUAGE.into(), Flavor::C)),
        "h" | "hh" | "hpp" | "hxx" | "cc" | "cpp" | "cxx" => {
            Some((tree_sitter_cpp::LANGUAGE.into(), Flavor::Cpp))
        }
        "rs" => Some((tree_sitter_rust::LANGUAGE.into(), Flavor::Rust)),
        "py" | "pyi" => Some((tree_sitter_python::LANGUAGE.into(), Flavor::Python)),
        _ => None,
    }
}

pub fn parse(path: &Path) -> Result<Option<ParsedFile>> {
    let Some((language, flavor)) = grammar(path) else {
        return Ok(None);
    };
    let bytes = std::fs::read(path)?;
    let source = String::from_utf8(bytes).context("source is not valid UTF-8")?;
    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let tree = parser
        .parse(&source, None)
        .context("Tree-sitter could not parse the file")?;
    let mut symbols = Vec::new();
    visit(
        tree.root_node(),
        source.as_bytes(),
        flavor,
        &[],
        &mut symbols,
    );
    if matches!(flavor, Flavor::Cpp | Flavor::C) {
        supplement_c_family_declarations(&source, &mut symbols);
    }
    symbols.sort_by_key(|symbol| (symbol.start_byte, symbol.end_byte));
    Ok(Some(ParsedFile { source, symbols }))
}

fn classification(
    kind: &str,
    flavor: Flavor,
    inside_type: bool,
) -> Option<(&'static str, &'static str)> {
    let result = match kind {
        "namespace_definition" => ("namespace", "name"),
        "class_definition" => ("class", "name"),
        "class_specifier" => ("class", "name"),
        "struct_specifier" | "struct_item" => ("struct", "name"),
        "union_specifier" | "union_item" => ("union", "name"),
        "enum_specifier" | "enum_item" => ("enum", "name"),
        "trait_item" => ("trait", "name"),
        "type_item" | "type_definition" | "alias_declaration" => ("type alias", "name"),
        "function_item" | "function_definition" => {
            (if inside_type { "method" } else { "function" }, "name")
        }
        "enumerator" | "enum_variant" => ("enum member", "name"),
        "field_declaration" => ("field", "declarator"),
        _ => return None,
    };
    if matches!(flavor, Flavor::Python) && kind == "function_definition" {
        return Some((if inside_type { "method" } else { "function" }, "name"));
    }
    Some(result)
}

fn supplement_c_family_declarations(source: &str, symbols: &mut Vec<Symbol>) {
    let declaration =
        regex::Regex::new(r"\b(class|struct|union|enum)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let mut byte_offset = 0;
    for (row, line) in source.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("//") && !trimmed.starts_with("/*") && !trimmed.starts_with('*') {
            for captures in declaration.captures_iter(line) {
                let whole = captures.get(0).unwrap();
                let name = captures.get(2).unwrap();
                // Ignore template parameter declarations such as `template<class T>`.
                if line[..whole.start()].trim_end().ends_with("template<") {
                    continue;
                }
                let start_byte = byte_offset + name.start();
                let name_end = byte_offset + name.end();
                if symbols.iter().any(|symbol| symbol.start_byte == start_byte) {
                    continue;
                }
                let kind = captures.get(1).unwrap().as_str();
                let tail = &source[name_end..(name_end + 500).min(source.len())];
                let brace = tail.find('{');
                let semicolon = tail.find(';');
                symbols.push(Symbol {
                    leaf_name: name.as_str().to_owned(),
                    qualified_name: name.as_str().to_owned(),
                    kind: kind.to_owned(),
                    start: SourcePoint {
                        line: row + 1,
                        column: name.start() + 1,
                    },
                    start_byte,
                    end_byte: name_end,
                    is_definition: brace
                        .is_some_and(|brace| semicolon.is_none_or(|semicolon| brace < semicolon)),
                });
            }
        }
        byte_offset += line.len();
    }
}

fn is_scope(kind: &str) -> bool {
    matches!(
        kind,
        "namespace_definition"
            | "class_definition"
            | "class_specifier"
            | "struct_specifier"
            | "struct_item"
            | "union_specifier"
            | "union_item"
            | "enum_specifier"
            | "enum_item"
            | "trait_item"
    )
}

fn identifier<'a>(node: Node<'a>, preferred: &str) -> Option<Node<'a>> {
    let direct = node
        .child_by_field_name(preferred)
        .or_else(|| node.child_by_field_name("name"));
    direct.and_then(unwrap_identifier).or_else(|| {
        let mut cursor = node.walk();
        node.named_children(&mut cursor).find_map(unwrap_identifier)
    })
}

fn unwrap_identifier(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(
        node.kind(),
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "namespace_identifier"
            | "constant"
            | "operator_name"
    ) {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find_map(unwrap_identifier)
}

fn visit(node: Node<'_>, source: &[u8], flavor: Flavor, scopes: &[String], out: &mut Vec<Symbol>) {
    let inside_type = !scopes.is_empty();
    let mut child_scopes = scopes.to_vec();
    if let Some((symbol_kind, field)) = classification(node.kind(), flavor, inside_type)
        && let Some(name_node) = identifier(node, field)
        && let Ok(name) = name_node.utf8_text(source)
        && !name.is_empty()
    {
        let mut qualified = scopes.to_vec();
        qualified.push(name.to_owned());
        out.push(Symbol {
            leaf_name: name.to_owned(),
            qualified_name: qualified.join("::"),
            kind: symbol_kind.to_owned(),
            start: name_node.start_position().into(),
            start_byte: name_node.start_byte(),
            end_byte: name_node.end_byte(),
            is_definition: node.child_by_field_name("body").is_some()
                || !matches!(
                    node.kind(),
                    "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier"
                ),
        });
        if is_scope(node.kind()) {
            child_scopes.push(name.to_owned());
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit(child, source, flavor, &child_scopes, out);
    }
}

pub fn find_symbols<'a>(symbols: &'a [Symbol], query: &str) -> Vec<&'a Symbol> {
    let matcher = SkimMatcherV2::default().ignore_case();
    let query_lower = query.to_ascii_lowercase();
    let mut found: Vec<_> = symbols
        .iter()
        .filter_map(|symbol| {
            let leaf = symbol.leaf_name.to_ascii_lowercase();
            let base = if leaf == query_lower {
                1_000_000
            } else if leaf.starts_with(&query_lower) {
                500_000
            } else {
                matcher.fuzzy_match(&symbol.leaf_name, query)?
            };
            let qualified_bonus = matcher
                .fuzzy_match(&symbol.qualified_name, query)
                .unwrap_or(0);
            Some((base + qualified_bonus, symbol))
        })
        .collect();
    found.sort_by(|(ascore, a), (bscore, b)| {
        bscore
            .cmp(ascore)
            .then_with(|| a.start.line.cmp(&b.start.line))
            .then_with(|| a.start.column.cmp(&b.start.column))
    });
    found.into_iter().map(|(_, symbol)| symbol).collect()
}

pub fn resolve_unique<'a>(symbols: &'a [Symbol], query: &str) -> Result<&'a Symbol> {
    let exact: Vec<_> = symbols
        .iter()
        .filter(|s| {
            s.leaf_name.eq_ignore_ascii_case(query) || s.qualified_name.eq_ignore_ascii_case(query)
        })
        .collect();
    match exact.as_slice() {
        [_, ..] => Ok(exact
            .iter()
            .copied()
            .find(|symbol| symbol.is_definition)
            .unwrap_or(exact[0])),
        [] => find_symbols(symbols, query)
            .into_iter()
            .next()
            .context("symbol not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn points_are_one_based() {
        assert_eq!(
            SourcePoint::from(Point { row: 4, column: 7 }),
            SourcePoint { line: 5, column: 8 }
        );
    }
}
