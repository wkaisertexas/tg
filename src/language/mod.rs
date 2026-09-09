mod classify;
mod extract;
mod grammar;
mod markdown;
mod parser;
mod ranking;
#[cfg(test)]
mod tests;

pub(crate) use grammar::may_support_with_source;
pub use grammar::supports;
pub use markdown::{is_markdown_symbol, markdown_slug};
pub use parser::{
    SymbolParser, index_symbols_parallel, install_indexing, parse, parse_indexed,
    parse_indexed_source, parse_source,
};
pub use ranking::{find_symbols, member_score, names_equivalent, resolve_unique};
use tree_sitter::Point;

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
