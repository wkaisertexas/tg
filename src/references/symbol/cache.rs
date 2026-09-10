use super::*;
use crate::language::ParsedFile;
use crate::references::file::read_versioned;

pub(super) struct CachedParse {
    version: FileVersion,
    parsed: Arc<ParsedFile>,
}

impl SymbolProvider {
    pub(super) fn parse(&self, path: &Path) -> Result<Option<(Arc<ParsedFile>, FileVersion)>> {
        self.parse_cancellable(path, None)
    }

    pub(super) fn parse_cancellable(
        &self,
        path: &Path,
        cancellation: Option<&CancellationFlag>,
    ) -> Result<Option<(Arc<ParsedFile>, FileVersion)>> {
        let canonical = path.canonicalize()?;
        anyhow::ensure!(
            canonical.starts_with(&self.canonical_root),
            "file is outside search root"
        );
        if cancellation.is_some_and(CancellationFlag::is_cancelled) {
            return Ok(None);
        }
        let snapshot = read_versioned(&canonical)?;
        let version = snapshot.version;
        if let Some(cached) = self.parse_cache.lock().unwrap().get(&canonical)
            && cached.version == version
        {
            return Ok(Some((Arc::clone(&cached.parsed), version)));
        }
        if cancellation.is_some_and(CancellationFlag::is_cancelled) {
            return Ok(None);
        }
        #[cfg(test)]
        self.parse_count.fetch_add(1, Ordering::Relaxed);
        let source = String::from_utf8(snapshot.bytes).context("source is not valid UTF-8")?;
        let Some(parsed) = language::parse_source(&canonical, source)? else {
            return Ok(None);
        };
        let parsed = Arc::new(parsed);
        self.parse_cache.lock().unwrap().insert(
            canonical,
            CachedParse {
                version: version.clone(),
                parsed: Arc::clone(&parsed),
            },
        );
        Ok(Some((parsed, version)))
    }
}
