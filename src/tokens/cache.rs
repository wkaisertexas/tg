use super::{SymbolRange, TokenCounter, TokenState};
use crate::references::model::char_to_byte;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileCacheIdentity {
    pub canonical_path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}

impl FileCacheIdentity {
    pub fn from_path(path: &Path) -> Result<Self> {
        let canonical_path = path.canonicalize()?;
        let metadata = std::fs::metadata(&canonical_path)?;
        Ok(Self {
            canonical_path,
            size: metadata.len(),
            modified: metadata.modified()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum TokenCacheKey {
    File(FileCacheIdentity),
    Range {
        file: FileCacheIdentity,
        start_byte: usize,
        end_byte: usize,
    },
}

pub(super) type TokenCache = Mutex<HashMap<TokenCacheKey, usize>>;

pub(super) fn stable_file(path: &Path) -> Result<(FileCacheIdentity, Vec<u8>)> {
    for _ in 0..2 {
        let canonical_path = path.canonicalize()?;
        let before = std::fs::metadata(&canonical_path)?;
        let bytes = std::fs::read(&canonical_path)?;
        let after = std::fs::metadata(&canonical_path)?;
        if before.len() == after.len()
            && before.modified().ok() == after.modified().ok()
            && bytes.len() as u64 == after.len()
        {
            return Ok((
                FileCacheIdentity {
                    canonical_path,
                    size: after.len(),
                    modified: after.modified()?,
                },
                bytes,
            ));
        }
    }
    bail!("file changed while it was being counted")
}

pub(super) fn count_file(
    path: &Path,
    counter: &dyn TokenCounter,
    cache: &TokenCache,
) -> Result<TokenState> {
    let initial_key = TokenCacheKey::File(FileCacheIdentity::from_path(path)?);
    if let Some(count) = cache.lock().unwrap().get(&initial_key).copied() {
        return Ok(TokenState::Ready(count));
    }
    let (file, bytes) = stable_file(path)?;
    let key = TokenCacheKey::File(file);
    if let Some(count) = cache.lock().unwrap().get(&key).copied() {
        return Ok(TokenState::Ready(count));
    }
    let Ok(source) = std::str::from_utf8(&bytes) else {
        return Ok(TokenState::Bytes(bytes.len() as u64));
    };
    let count = counter.count(source)?;
    cache.lock().unwrap().insert(key, count);
    Ok(TokenState::Ready(count))
}

pub(super) fn count_range(
    file: FileCacheIdentity,
    source: &str,
    range: SymbolRange,
    counter: &dyn TokenCounter,
    cache: &TokenCache,
) -> Result<TokenState> {
    let byte_range = match range {
        SymbolRange::Bytes(range) => {
            anyhow::ensure!(
                source.get(range.clone()).is_some(),
                "invalid UTF-8 byte range"
            );
            range
        }
        SymbolRange::Characters(range) => character_range_to_bytes(source, range)?,
    };
    let key = TokenCacheKey::Range {
        file,
        start_byte: byte_range.start,
        end_byte: byte_range.end,
    };
    if let Some(count) = cache.lock().unwrap().get(&key).copied() {
        return Ok(TokenState::Ready(count));
    }
    let slice = source
        .get(byte_range)
        .context("invalid UTF-8 symbol range")?;
    let count = counter.count(slice)?;
    cache.lock().unwrap().insert(key, count);
    Ok(TokenState::Ready(count))
}

fn character_range_to_bytes(source: &str, range: Range<usize>) -> Result<Range<usize>> {
    anyhow::ensure!(
        range.start <= range.end,
        "character range starts after it ends"
    );
    anyhow::ensure!(
        range.end <= source.chars().count(),
        "character range is out of bounds"
    );
    let start = char_to_byte(source, range.start).context("invalid character range start")?;
    let end = char_to_byte(source, range.end).context("invalid character range end")?;
    Ok(start..end)
}
