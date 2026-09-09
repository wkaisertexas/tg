use super::extract::{supplement_c_family_declarations, symbols};
use super::grammar::{Flavor, grammar_for_source};
use super::markdown::{is_markdown, parse_markdown_headings};
use super::{ParsedFile, Symbol};
use anyhow::{Context, Result};
use rayon::prelude::*;
use rayon::{ThreadPool, ThreadPoolBuilder};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tree_sitter::Parser;

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
        let bytes = std::fs::read(path)?;
        let source = String::from_utf8(bytes).context("source is not valid UTF-8")?;
        self.parse_source(path, source)
    }

    pub fn parse_source(&mut self, path: &Path, source: String) -> Result<Option<ParsedFile>> {
        if is_markdown(path) {
            let symbols = parse_markdown_headings(&source);
            return Ok(Some(ParsedFile { source, symbols }));
        }
        let Some((language, flavor)) = grammar_for_source(path, &source) else {
            return Ok(None);
        };
        if self.flavor != Some(flavor) {
            self.parser.set_language(&language)?;
            self.flavor = Some(flavor);
        }
        let tree = self
            .parser
            .parse(&source, None)
            .context("Tree-sitter could not parse the file")?;
        let mut symbols = symbols(tree.root_node(), source.as_bytes(), flavor);
        symbols.sort_unstable_by_key(|symbol| (symbol.name_start_byte, symbol.name_end_byte));
        if matches!(flavor, Flavor::Cpp | Flavor::C)
            && supplement_c_family_declarations(&source, &mut symbols)
        {
            symbols.sort_unstable_by_key(|symbol| (symbol.name_start_byte, symbol.name_end_byte));
        }
        Ok(Some(ParsedFile { source, symbols }))
    }
}

impl Default for SymbolParser {
    fn default() -> Self {
        Self::new()
    }
}

pub fn parse(path: &Path) -> Result<Option<ParsedFile>> {
    SymbolParser::new().parse(path)
}
pub fn parse_source(path: &Path, source: String) -> Result<Option<ParsedFile>> {
    SymbolParser::new().parse_source(path, source)
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

thread_local! { static INDEX_PARSER: RefCell<SymbolParser> = RefCell::new(SymbolParser::new()); }

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
