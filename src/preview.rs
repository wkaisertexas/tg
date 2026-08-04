use crate::language::{ParsedFile, Symbol};

pub fn window<'a>(parsed: &'a ParsedFile, symbol: &Symbol, radius: usize) -> Vec<(usize, &'a str)> {
    let lines: Vec<_> = parsed.source.lines().collect();
    let start = symbol.start.line.saturating_sub(radius + 1);
    let end = (symbol.start.line + radius).min(lines.len());
    lines[start..end]
        .iter()
        .enumerate()
        .map(|(index, line)| (start + index + 1, *line))
        .collect()
}
