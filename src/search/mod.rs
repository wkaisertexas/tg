use anyhow::Result;
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use ignore::WalkBuilder;
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
    walk_with_excludes(root, mode, &[])
}

pub fn walk_with_excludes(
    root: &Path,
    mode: SearchMode,
    broad_excludes: &[String],
) -> Vec<PathBuf> {
    let filter_root = root.to_path_buf();
    let filter_excludes = broad_excludes.to_vec();
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false).follow_links(false);
    if mode == SearchMode::Broad {
        builder
            .git_ignore(false)
            .git_exclude(false)
            .ignore(false)
            .parents(false);
    }
    builder
        .filter_entry(move |entry| {
            entry.file_name() != ".git"
                && (mode != SearchMode::Broad
                    || !is_broadly_excluded(&filter_root, entry.path(), &filter_excludes))
        })
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        // Links are not followed and symlink entries are not regular files, so
        // containment and cycle safety do not require canonicalizing every hit.
        // Exact resolution still canonicalizes and verifies its target.
        .map(|entry| entry.into_path())
        .collect()
}

fn is_broadly_excluded(root: &Path, path: &Path, excludes: &[String]) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let relative = relative.to_string_lossy().replace('\\', "/");
    excludes.iter().any(|exclude| {
        let exclude = exclude.trim_matches('/').trim_start_matches("./");
        if exclude.is_empty() {
            return false;
        }
        if exclude.contains('/') {
            relative == exclude || relative.starts_with(&format!("{exclude}/"))
        } else {
            relative.split('/').any(|component| component == exclude)
        }
    })
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
