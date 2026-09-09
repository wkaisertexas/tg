use super::Symbol;
use anyhow::{Context, Result};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};

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
