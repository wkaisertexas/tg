use super::{SourcePoint, Symbol};
use std::path::Path;

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

pub(super) fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown")
        })
}

pub(super) fn parse_markdown_headings(source: &str) -> Vec<Symbol> {
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
