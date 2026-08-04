use anyhow::Result;
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use ignore::WalkBuilder;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    GitAware,
    Broad,
}

#[derive(Debug, Clone)]
pub struct FileMatch {
    pub relative: String,
    pub path: PathBuf,
    pub score: i64,
    pub indices: Vec<usize>,
}

pub fn walk(root: &Path, mode: SearchMode) -> Vec<PathBuf> {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false).follow_links(false);
    if mode == SearchMode::Broad {
        builder
            .git_ignore(false)
            .git_exclude(false)
            .ignore(false)
            .parents(false);
    }
    let mut seen = HashSet::new();
    builder
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let path = entry.into_path();
            let canonical = path.canonicalize().ok()?;
            (canonical.starts_with(&canonical_root) && seen.insert(canonical)).then_some(path)
        })
        .collect()
}

pub fn find(root: &Path, files: &[PathBuf], query: &str, limit: usize) -> Vec<FileMatch> {
    let matcher = SkimMatcherV2::default().ignore_case();
    let mut matches: Vec<_> = files
        .iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            let (score, indices) = if query.is_empty() {
                (0, Vec::new())
            } else {
                matcher.fuzzy_indices(&relative, query)?
            };
            Some(FileMatch {
                relative,
                path: path.clone(),
                score,
                indices,
            })
        })
        .collect();
    matches.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.relative.cmp(&b.relative))
    });
    matches.truncate(limit);
    matches
}

pub fn resolve_exact(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = root.join(relative);
    let canonical = path.canonicalize()?;
    let canonical_root = root.canonicalize()?;
    anyhow::ensure!(
        canonical.starts_with(canonical_root) && canonical.is_file(),
        "file is outside search root"
    );
    Ok(canonical)
}
