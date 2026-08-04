use anyhow::{Context, Result};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use rayon::prelude::*;
use rayon::{ThreadPool, ThreadPoolBuilder};
use std::cell::RefCell;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;
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

pub struct SymbolParser {
    parser: Parser,
    flavor: Option<Flavor>,
}

impl SymbolParser {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            flavor: None,
        }
    }

    pub fn parse(&mut self, path: &Path) -> Result<Option<ParsedFile>> {
        parse_with_parser(path, &mut self.parser, &mut self.flavor)
    }
}

impl Default for SymbolParser {
    fn default() -> Self {
        Self::new()
    }
}

pub fn display_name(symbol: &Symbol) -> String {
    if is_markdown_symbol(symbol) {
        return markdown_slug(&symbol.leaf_name);
    }
    symbol
        .leaf_name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

pub fn lowered_reference(file: &str, symbol: &Symbol) -> String {
    if is_markdown_symbol(symbol) {
        format!("{file}#{}", markdown_slug(&symbol.leaf_name))
    } else {
        format!(
            "{file}::{}:{} {}",
            symbol.start.line, symbol.start.column, symbol.leaf_name
        )
    }
}

pub fn is_markdown_symbol(symbol: &Symbol) -> bool {
    symbol.kind.starts_with("heading ")
}

pub fn markdown_slug(heading: &str) -> String {
    let mut slug = String::new();
    let mut pending_separator = false;
    for character in heading.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '_' {
            if pending_separator && !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
            slug.push(character);
            pending_separator = false;
        } else if character.is_whitespace() || character == '-' {
            pending_separator = true;
        }
    }
    slug
}

pub fn names_equivalent(left: &str, right: &str) -> bool {
    let normalize = |value: &str| {
        value
            .chars()
            .filter(|character| character.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    normalize(left) == normalize(right)
}

#[derive(Clone, Copy, PartialEq, Eq)]
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

pub fn supports(path: &Path) -> bool {
    grammar(path).is_some() || is_markdown(path)
}

pub fn parse(path: &Path) -> Result<Option<ParsedFile>> {
    SymbolParser::new().parse(path)
}

fn parse_with_parser(
    path: &Path,
    parser: &mut Parser,
    current_flavor: &mut Option<Flavor>,
) -> Result<Option<ParsedFile>> {
    if is_markdown(path) {
        let bytes = std::fs::read(path)?;
        let source = String::from_utf8(bytes).context("source is not valid UTF-8")?;
        let symbols = parse_markdown_headings(&source);
        return Ok(Some(ParsedFile { source, symbols }));
    }
    let Some((language, flavor)) = grammar(path) else {
        return Ok(None);
    };
    let bytes = std::fs::read(path)?;
    let source = String::from_utf8(bytes).context("source is not valid UTF-8")?;
    if *current_flavor != Some(flavor) {
        parser.set_language(&language)?;
        *current_flavor = Some(flavor);
    }
    let tree = parser
        .parse(&source, None)
        .context("Tree-sitter could not parse the file")?;
    let mut symbols = Vec::new();
    visit(
        tree.root_node(),
        source.as_bytes(),
        flavor,
        &mut Vec::new(),
        &mut symbols,
    );
    symbols.sort_unstable_by_key(|symbol| (symbol.start_byte, symbol.end_byte));
    if matches!(flavor, Flavor::Cpp | Flavor::C)
        && supplement_c_family_declarations(&source, &mut symbols)
    {
        symbols.sort_unstable_by_key(|symbol| (symbol.start_byte, symbol.end_byte));
    }
    Ok(Some(ParsedFile { source, symbols }))
}

pub fn index_symbols_parallel(paths: &[PathBuf]) -> Vec<Symbol> {
    install_indexing(|| {
        paths
            .par_iter()
            .map(|path| {
                parse_indexed(path)
                    .ok()
                    .flatten()
                    .map(|parsed| parsed.symbols)
                    .unwrap_or_default()
            })
            .flatten()
            .collect()
    })
}

thread_local! {
    static INDEX_PARSER: RefCell<SymbolParser> = RefCell::new(SymbolParser::new());
}

pub fn parse_indexed(path: &Path) -> Result<Option<ParsedFile>> {
    INDEX_PARSER.with(|parser| parser.borrow_mut().parse(path))
}

pub fn install_indexing<R: Send>(operation: impl FnOnce() -> R + Send) -> R {
    static INDEX_POOL: OnceLock<ThreadPool> = OnceLock::new();
    INDEX_POOL
        .get_or_init(|| {
            let worker_count = std::thread::available_parallelism()
                .map_or(4, usize::from)
                .saturating_mul(4)
                .min(56);
            ThreadPoolBuilder::new()
                .num_threads(worker_count)
                .thread_name(|index| format!("tg-index-{index}"))
                .stack_size(8 * 1024 * 1024)
                .build()
                .expect("could not create symbol indexing workers")
        })
        .install(operation)
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown")
        })
}

fn parse_markdown_headings(source: &str) -> Vec<Symbol> {
    let lines: Vec<_> = source.split_inclusive('\n').collect();
    let mut offsets = Vec::with_capacity(lines.len());
    let mut offset = 0;
    for line in &lines {
        offsets.push(offset);
        offset += line.len();
    }
    let mut symbols = Vec::new();
    let mut hierarchy: Vec<(usize, String)> = Vec::new();
    let mut fenced = false;
    for (index, line) in lines.iter().enumerate() {
        let without_newline = line.trim_end_matches(['\r', '\n']);
        let trimmed = without_newline.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let indent = without_newline.len() - trimmed.len();
        if indent <= 3 {
            let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
            if (1..=6).contains(&hashes)
                && trimmed
                    .as_bytes()
                    .get(hashes)
                    .is_none_or(u8::is_ascii_whitespace)
            {
                let after_hashes = &trimmed[hashes..];
                let leading = after_hashes.len() - after_hashes.trim_start().len();
                let raw_name = after_hashes.trim();
                let name = raw_name.trim_end_matches('#').trim_end();
                if !name.is_empty() {
                    push_markdown_heading(
                        &mut symbols,
                        &mut hierarchy,
                        hashes,
                        name,
                        index + 1,
                        indent + hashes + leading + 1,
                        offsets[index] + indent + hashes + leading,
                    );
                }
                continue;
            }
        }
        if index > 0 && is_setext_underline(trimmed) {
            let previous = lines[index - 1].trim_end_matches(['\r', '\n']);
            let name = previous.trim();
            if !name.is_empty() {
                let column = previous.len() - previous.trim_start().len() + 1;
                let start_byte = offsets[index - 1] + column - 1;
                push_markdown_heading(
                    &mut symbols,
                    &mut hierarchy,
                    if trimmed.starts_with('=') { 1 } else { 2 },
                    name,
                    index,
                    column,
                    start_byte,
                );
            }
        }
    }
    symbols
}

fn is_setext_underline(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 3
        && (line.bytes().all(|byte| byte == b'=') || line.bytes().all(|byte| byte == b'-'))
}

fn push_markdown_heading(
    symbols: &mut Vec<Symbol>,
    hierarchy: &mut Vec<(usize, String)>,
    level: usize,
    name: &str,
    line: usize,
    column: usize,
    start_byte: usize,
) {
    while hierarchy
        .last()
        .is_some_and(|(parent_level, _)| *parent_level >= level)
    {
        hierarchy.pop();
    }
    let mut qualified: Vec<_> = hierarchy.iter().map(|(_, name)| name.clone()).collect();
    qualified.push(name.to_owned());
    symbols.push(Symbol {
        leaf_name: name.to_owned(),
        qualified_name: qualified.join("::"),
        kind: format!("heading {level}"),
        start: SourcePoint { line, column },
        start_byte,
        end_byte: start_byte + name.len(),
        is_definition: true,
    });
    hierarchy.push((level, name.to_owned()));
}

fn classification(
    kind: &str,
    flavor: Flavor,
    inside_type: bool,
) -> Option<(&'static str, &'static str)> {
    let result = match kind {
        "namespace_definition" | "mod_item" => ("module", "name"),
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

fn supplement_c_family_declarations(source: &str, symbols: &mut Vec<Symbol>) -> bool {
    static DECLARATION: OnceLock<regex::Regex> = OnceLock::new();
    let declaration = DECLARATION.get_or_init(|| {
        regex::Regex::new(r"\b(class|struct|union|enum)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap()
    });
    let original_len = symbols.len();
    let mut added_offsets = Vec::new();
    let mut byte_offset = 0;
    for (row, line) in source.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("//") && !trimmed.starts_with("/*") && !trimmed.starts_with('*') {
            for captures in declaration.captures_iter(line) {
                let whole = captures.get(0).unwrap();
                let name = captures.get(2).unwrap();
                if line[..whole.start()].trim_end().ends_with("template<") {
                    continue;
                }
                let start_byte = byte_offset + name.start();
                let name_end = byte_offset + name.end();
                if symbols[..original_len]
                    .binary_search_by_key(&start_byte, |symbol| symbol.start_byte)
                    .is_ok()
                    || added_offsets.contains(&start_byte)
                {
                    continue;
                }
                added_offsets.push(start_byte);
                let kind = captures.get(1).unwrap().as_str();
                let tail = &source.as_bytes()[name_end..(name_end + 500).min(source.len())];
                let brace = tail.iter().position(|byte| *byte == b'{');
                let semicolon = tail.iter().position(|byte| *byte == b';');
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
    !added_offsets.is_empty()
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
            | "mod_item"
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

fn visit(
    node: Node<'_>,
    source: &[u8],
    flavor: Flavor,
    scopes: &mut Vec<String>,
    out: &mut Vec<Symbol>,
) {
    let inside_type = !scopes.is_empty();
    let original_scope_len = scopes.len();
    if node.kind() == "impl_item"
        && let Some(type_node) = node.child_by_field_name("type").and_then(unwrap_identifier)
        && let Ok(name) = type_node.utf8_text(source)
    {
        scopes.push(name.to_owned());
    }
    if let Some((symbol_kind, field)) = classification(node.kind(), flavor, inside_type)
        && let Some(name_node) = identifier(node, field)
        && let Ok(name) = name_node.utf8_text(source)
        && !name.is_empty()
    {
        let qualified_name = if scopes.is_empty() {
            name.to_owned()
        } else {
            let mut qualified = scopes.join("::");
            qualified.push_str("::");
            qualified.push_str(name);
            qualified
        };
        out.push(Symbol {
            leaf_name: name.to_owned(),
            qualified_name,
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
            scopes.push(name.to_owned());
        }
    }
    if matches!(node.kind(), "function_definition" | "function_item") {
        scopes.truncate(original_scope_len);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit(child, source, flavor, scopes, out);
    }
    scopes.truncate(original_scope_len);
}

pub fn find_symbols<'a>(symbols: &'a [Symbol], query: &str) -> Vec<&'a Symbol> {
    let matcher = SkimMatcherV2::default().ignore_case();
    if let Some((parent, member)) = query.rsplit_once('.')
        && symbols.iter().any(|symbol| {
            names_equivalent(&symbol.leaf_name, parent)
                || names_equivalent(&symbol.qualified_name, parent)
        })
    {
        let mut found: Vec<_> = symbols
            .iter()
            .filter_map(|symbol| {
                member_score(symbol, parent, member, &matcher).map(|score| (score, symbol))
            })
            .collect();
        found.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.start.line.cmp(&right.start.line))
        });
        return found.into_iter().map(|(_, symbol)| symbol).collect();
    }
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

pub fn member_score(
    symbol: &Symbol,
    parent: &str,
    member: &str,
    matcher: &SkimMatcherV2,
) -> Option<i64> {
    let segments: Vec<_> = symbol.qualified_name.split("::").collect();
    let parent_index = segments
        .iter()
        .rposition(|segment| names_equivalent(segment, parent))?;
    if parent_index + 1 >= segments.len() {
        return None;
    }
    let leaf = symbol.leaf_name.to_ascii_lowercase();
    let member_lower = member.to_ascii_lowercase();
    let name_score = if member.is_empty() {
        10_000
    } else if leaf == member_lower {
        1_000_000
    } else if leaf.starts_with(&member_lower) {
        500_000
    } else {
        matcher.fuzzy_match(&symbol.leaf_name, member)?
    };
    let distance = segments.len() - parent_index - 1;
    Some(name_score + if distance == 1 { 100_000 } else { 0 })
}

pub fn resolve_unique<'a>(symbols: &'a [Symbol], query: &str) -> Result<&'a Symbol> {
    let exact: Vec<_> = symbols
        .iter()
        .filter(|s| {
            names_equivalent(&s.leaf_name, query) || names_equivalent(&s.qualified_name, query)
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
