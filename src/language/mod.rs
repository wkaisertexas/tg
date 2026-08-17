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
    /// UTF-8 byte range of the identifier used for display and lowering.
    pub name_start_byte: usize,
    pub name_end_byte: usize,
    /// UTF-8 byte range of the complete declaration or structural unit.
    pub range_start_byte: usize,
    pub range_end_byte: usize,
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

    pub fn parse_source(&mut self, path: &Path, source: String) -> Result<Option<ParsedFile>> {
        parse_source_with_parser(path, source, &mut self.parser, &mut self.flavor)
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
    JavaScript,
    Rust,
    Python,
    TypeScript,
    Tsx,
}

fn grammar(path: &Path) -> Option<(Language, Flavor)> {
    if path.file_name().and_then(|name| name.to_str()) == Some("Jakefile") {
        return Some((tree_sitter_javascript::LANGUAGE.into(), Flavor::JavaScript));
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "c" => Some((tree_sitter_c::LANGUAGE.into(), Flavor::C)),
        "h" | "hh" | "hpp" | "hxx" | "cc" | "cpp" | "cxx" => {
            Some((tree_sitter_cpp::LANGUAGE.into(), Flavor::Cpp))
        }
        "rs" => Some((tree_sitter_rust::LANGUAGE.into(), Flavor::Rust)),
        "py" | "pyi" => Some((tree_sitter_python::LANGUAGE.into(), Flavor::Python)),
        "js" | "mjs" | "cjs" | "jsx" => {
            Some((tree_sitter_javascript::LANGUAGE.into(), Flavor::JavaScript))
        }
        "ts" | "mts" | "cts" => Some((
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Flavor::TypeScript,
        )),
        "tsx" => Some((tree_sitter_typescript::LANGUAGE_TSX.into(), Flavor::Tsx)),
        _ => None,
    }
}

pub fn supports(path: &Path) -> bool {
    grammar(path).is_some() || is_markdown(path)
}

pub fn parse(path: &Path) -> Result<Option<ParsedFile>> {
    SymbolParser::new().parse(path)
}

pub fn parse_source(path: &Path, source: String) -> Result<Option<ParsedFile>> {
    SymbolParser::new().parse_source(path, source)
}

fn parse_with_parser(
    path: &Path,
    parser: &mut Parser,
    current_flavor: &mut Option<Flavor>,
) -> Result<Option<ParsedFile>> {
    let bytes = std::fs::read(path)?;
    let source = String::from_utf8(bytes).context("source is not valid UTF-8")?;
    parse_source_with_parser(path, source, parser, current_flavor)
}

fn parse_source_with_parser(
    path: &Path,
    source: String,
    parser: &mut Parser,
    current_flavor: &mut Option<Flavor>,
) -> Result<Option<ParsedFile>> {
    if is_markdown(path) {
        let symbols = parse_markdown_headings(&source);
        return Ok(Some(ParsedFile { source, symbols }));
    }
    let Some((language, flavor)) = grammar(path) else {
        return Ok(None);
    };
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
    symbols.sort_unstable_by_key(|symbol| (symbol.name_start_byte, symbol.name_end_byte));
    if matches!(flavor, Flavor::Cpp | Flavor::C)
        && supplement_c_family_declarations(&source, &mut symbols)
    {
        symbols.sort_unstable_by_key(|symbol| (symbol.name_start_byte, symbol.name_end_byte));
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

pub fn parse_indexed_source(path: &Path, source: String) -> Result<Option<ParsedFile>> {
    INDEX_PARSER.with(|parser| parser.borrow_mut().parse_source(path, source))
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
                        SourcePoint {
                            line: index + 1,
                            column: indent + hashes + leading + 1,
                        },
                        offsets[index] + indent + hashes + leading,
                        offsets[index] + indent..offsets[index] + without_newline.len(),
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
                    SourcePoint {
                        line: index,
                        column,
                    },
                    start_byte,
                    offsets[index - 1] + previous.len() - previous.trim_start().len()
                        ..offsets[index] + without_newline.len(),
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
    start: SourcePoint,
    name_start_byte: usize,
    range: std::ops::Range<usize>,
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
        start,
        name_start_byte,
        name_end_byte: name_start_byte + name.len(),
        range_start_byte: range.start,
        range_end_byte: range.end,
        is_definition: true,
    });
    hierarchy.push((level, name.to_owned()));
}

fn classification(
    node: Node<'_>,
    flavor: Flavor,
    inside_type: bool,
) -> Option<(&'static str, &'static str)> {
    let kind = node.kind();
    let is_ecmascript = matches!(
        flavor,
        Flavor::JavaScript | Flavor::TypeScript | Flavor::Tsx
    );
    let is_typescript = matches!(flavor, Flavor::TypeScript | Flavor::Tsx);
    let result = match kind {
        "namespace_definition" | "mod_item" => ("module", "name"),
        "internal_module" | "module" if is_typescript => ("module", "name"),
        "class_definition" => ("class", "name"),
        "class_declaration" if is_ecmascript => ("class", "name"),
        "class" if is_ecmascript && node.child_by_field_name("name").is_some() => ("class", "name"),
        "abstract_class_declaration" if is_typescript => ("class", "name"),
        "interface_declaration" if is_typescript => ("interface", "name"),
        "class_specifier" => ("class", "name"),
        "struct_specifier" | "struct_item" => ("struct", "name"),
        "union_specifier" | "union_item" => ("union", "name"),
        "enum_specifier" | "enum_item" => ("enum", "name"),
        "enum_declaration" if is_typescript => ("enum", "name"),
        "trait_item" => ("trait", "name"),
        "type_item" | "type_definition" | "alias_declaration" => ("type alias", "name"),
        "type_alias_declaration" if is_typescript => ("type alias", "name"),
        "function_item" | "function_definition" => {
            (if inside_type { "method" } else { "function" }, "name")
        }
        "function_declaration" | "generator_function_declaration" if is_ecmascript => {
            (if inside_type { "method" } else { "function" }, "name")
        }
        "function_expression" | "generator_function"
            if is_ecmascript && node.child_by_field_name("name").is_some() =>
        {
            (if inside_type { "method" } else { "function" }, "name")
        }
        "function_signature" if is_typescript => {
            (if inside_type { "method" } else { "function" }, "name")
        }
        "method_definition" if is_ecmascript => ("method", "name"),
        "method_signature" | "abstract_method_signature" if is_typescript => ("method", "name"),
        "enumerator" | "enum_variant" => ("enum member", "name"),
        "enum_assignment" if is_typescript => ("enum member", "name"),
        "property_identifier"
            if is_typescript
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "enum_body") =>
        {
            ("enum member", "name")
        }
        "field_declaration" => ("field", "declarator"),
        "field_definition" if is_ecmascript => ("field", "property"),
        "public_field_definition" | "property_signature" if is_typescript => ("field", "name"),
        "variable_declarator" if is_ecmascript && is_callable_variable(node) => {
            ("function", "name")
        }
        _ => return None,
    };
    if matches!(flavor, Flavor::Python) && kind == "function_definition" {
        return Some((if inside_type { "method" } else { "function" }, "name"));
    }
    Some(result)
}

fn is_callable_variable(node: Node<'_>) -> bool {
    node.child_by_field_name("value").is_some_and(|value| {
        matches!(
            value.kind(),
            "arrow_function" | "function_expression" | "generator_function"
        )
    }) && node
        .child_by_field_name("name")
        .is_some_and(|name| name.kind() == "identifier")
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
                let name_start_byte = byte_offset + name.start();
                let name_end = byte_offset + name.end();
                if symbols[..original_len]
                    .binary_search_by_key(&name_start_byte, |symbol| symbol.name_start_byte)
                    .is_ok()
                    || added_offsets.contains(&name_start_byte)
                {
                    continue;
                }
                added_offsets.push(name_start_byte);
                let kind = captures.get(1).unwrap().as_str();
                let tail = &source.as_bytes()[name_end..(name_end + 500).min(source.len())];
                let brace = tail.iter().position(|byte| *byte == b'{');
                let semicolon = tail.iter().position(|byte| *byte == b';');
                let line_without_newline = line.trim_end_matches(['\r', '\n']);
                let leading = line_without_newline.len() - line_without_newline.trim_start().len();
                symbols.push(Symbol {
                    leaf_name: name.as_str().to_owned(),
                    qualified_name: name.as_str().to_owned(),
                    kind: kind.to_owned(),
                    start: SourcePoint {
                        line: row + 1,
                        column: name.start() + 1,
                    },
                    name_start_byte,
                    name_end_byte: name_end,
                    range_start_byte: byte_offset + leading,
                    range_end_byte: byte_offset + line_without_newline.len(),
                    is_definition: brace
                        .is_some_and(|brace| semicolon.is_none_or(|semicolon| brace < semicolon)),
                });
            }
        }
        byte_offset += line.len();
    }
    !added_offsets.is_empty()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    Module,
    Type,
}

struct Scope {
    name: String,
    kind: ScopeKind,
}

fn scope_kind(kind: &str) -> Option<ScopeKind> {
    match kind {
        "namespace_definition" | "mod_item" | "internal_module" | "module" => {
            Some(ScopeKind::Module)
        }
        "class_definition"
        | "class_specifier"
        | "class"
        | "class_declaration"
        | "abstract_class_declaration"
        | "interface_declaration"
        | "struct_specifier"
        | "struct_item"
        | "union_specifier"
        | "union_item"
        | "enum_specifier"
        | "enum_item"
        | "enum_declaration"
        | "trait_item" => Some(ScopeKind::Type),
        _ => None,
    }
}

fn identifier<'a>(node: Node<'a>, preferred: &str) -> Option<Node<'a>> {
    let direct = node
        .child_by_field_name(preferred)
        .or_else(|| node.child_by_field_name("name"));
    direct
        .and_then(unwrap_identifier)
        .or_else(|| unwrap_identifier(node))
}

fn unwrap_identifier(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(
        node.kind(),
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "property_identifier"
            | "private_property_identifier"
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
    scopes: &mut Vec<Scope>,
    out: &mut Vec<Symbol>,
) {
    let inside_type = scopes
        .last()
        .is_some_and(|scope| scope.kind == ScopeKind::Type);
    let original_scope_len = scopes.len();
    if node.kind() == "impl_item"
        && let Some(type_node) = node.child_by_field_name("type").and_then(unwrap_identifier)
        && let Ok(name) = type_node.utf8_text(source)
    {
        scopes.push(Scope {
            name: name.to_owned(),
            kind: ScopeKind::Type,
        });
    }
    if let Some((symbol_kind, field)) = classification(node, flavor, inside_type)
        && let Some(name_node) = identifier(node, field)
        && let Ok(name) = name_node.utf8_text(source)
        && !name.is_empty()
        && !(node.kind() == "method_definition" && name == "constructor")
    {
        let qualified_name = if scopes.is_empty() {
            name.to_owned()
        } else {
            let mut qualified = scopes
                .iter()
                .map(|scope| scope.name.as_str())
                .collect::<Vec<_>>()
                .join("::");
            qualified.push_str("::");
            qualified.push_str(name);
            qualified
        };
        out.push(Symbol {
            leaf_name: name.to_owned(),
            qualified_name,
            kind: symbol_kind.to_owned(),
            start: name_node.start_position().into(),
            name_start_byte: name_node.start_byte(),
            name_end_byte: name_node.end_byte(),
            range_start_byte: node.start_byte(),
            range_end_byte: node.end_byte(),
            is_definition: is_definition(node),
        });
        if let Some(kind) = scope_kind(node.kind()) {
            scopes.push(Scope {
                name: name.to_owned(),
                kind,
            });
        }
    }
    if is_callable(node) {
        scopes.truncate(original_scope_len);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit(child, source, flavor, scopes, out);
    }
    scopes.truncate(original_scope_len);
}

fn is_callable(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_definition"
            | "function_item"
            | "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "arrow_function"
            | "function_signature"
            | "method_definition"
            | "method_signature"
            | "abstract_method_signature"
    ) || (node.kind() == "variable_declarator" && is_callable_variable(node))
}

fn is_definition(node: Node<'_>) -> bool {
    match node.kind() {
        "class_specifier"
        | "struct_specifier"
        | "union_specifier"
        | "enum_specifier"
        | "function_signature"
        | "method_signature"
        | "abstract_method_signature"
        | "property_signature" => false,
        "function_declaration" | "generator_function_declaration" | "method_definition" => {
            node.child_by_field_name("body").is_some()
        }
        _ => true,
    }
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
}
