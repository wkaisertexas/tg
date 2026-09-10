mod cache;
mod index;
mod target;
#[cfg(test)]
mod tests;

use super::file::file_version;
use super::model::{
    CandidateDisplay, CandidateId, CandidateTokenSource, ContextCost, FileOrigin, FileTarget,
    FileVersion, Preview, PreviewLine, QueryEmission, QueryProgress, QueryRequest, QueryScope,
    ReferenceCandidate, ReferenceKind, ReferenceTarget, SourceLocation, SymbolIdentity,
    SymbolTarget, ValidatedTarget,
};
use super::{CancellationFlag, ReferenceProvider};
use crate::language::{self, Symbol};
use anyhow::{Context, Result, bail};
use cache::CachedParse;
use index::RepositoryIndex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use target::{identity_for, stable_candidate_id, target_for};

#[derive(Clone)]
struct IndexedSymbol {
    relative_path: String,
    canonical_path: PathBuf,
    source_version: FileVersion,
    symbol: Symbol,
    origin: FileOrigin,
}

pub struct SymbolProvider {
    root: PathBuf,
    canonical_root: PathBuf,
    leader: String,
    batch_size: usize,
    index: Arc<Mutex<RepositoryIndex>>,
    parse_cache: Mutex<HashMap<PathBuf, CachedParse>>,
    targets: Mutex<HashMap<String, SymbolTarget>>,
    #[cfg(test)]
    index_launches: Arc<AtomicUsize>,
    #[cfg(test)]
    parse_count: AtomicUsize,
    broad_excludes: Vec<String>,
}

impl SymbolProvider {
    pub fn new(root: &Path, leader: impl Into<String>) -> Result<Self> {
        Self::with_broad_excludes(root, leader, &[])
    }

    pub fn with_broad_excludes(
        root: &Path,
        leader: impl Into<String>,
        broad_excludes: &[String],
    ) -> Result<Self> {
        Self::with_batch_size(root, leader, 64, broad_excludes)
    }

    fn with_batch_size(
        root: &Path,
        leader: impl Into<String>,
        batch_size: usize,
        broad_excludes: &[String],
    ) -> Result<Self> {
        let canonical_root = root
            .canonicalize()
            .with_context(|| format!("cannot read search root {}", root.display()))?;
        Ok(Self {
            root: root.to_path_buf(),
            canonical_root,
            leader: leader.into(),
            batch_size: batch_size.max(1),
            index: Arc::new(Mutex::new(RepositoryIndex::default())),
            parse_cache: Mutex::new(HashMap::new()),
            targets: Mutex::new(HashMap::new()),
            #[cfg(test)]
            index_launches: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            parse_count: AtomicUsize::new(0),
            broad_excludes: broad_excludes.to_vec(),
        })
    }

    fn candidate(
        &self,
        entry: &IndexedSymbol,
        generation: super::model::GenerationId,
        typed_leader: &str,
    ) -> ReferenceCandidate {
        let target = target_for(entry);
        let opaque = stable_candidate_id(&target);
        self.targets.lock().unwrap().insert(opaque.clone(), target);
        let symbol = &entry.symbol;
        ReferenceCandidate {
            id: CandidateId {
                provider: ReferenceKind::Symbol,
                opaque,
            },
            generation,
            kind: ReferenceKind::Symbol,
            friendly_text: format!(
                "{}{}::{}",
                if typed_leader.is_empty() {
                    &self.leader
                } else {
                    typed_leader
                },
                entry.relative_path,
                language::display_name(symbol)
            ),
            display: CandidateDisplay {
                primary: symbol.qualified_name.clone(),
                secondary: Some(format!(
                    "{} · {}:{}",
                    symbol.kind, symbol.start.line, symbol.start.column
                )),
                location: Some(SourceLocation {
                    line: symbol.start.line,
                    column: symbol.start.column,
                }),
                match_indices: Vec::new(),
            },
            context_cost: ContextCost::Pending,
            file_context_cost: Some(ContextCost::Pending),
            source_version: Some(entry.source_version.clone()),
            token_source: Some(CandidateTokenSource::Symbol {
                path: entry.canonical_path.clone(),
                start_byte: symbol.range_start_byte,
                end_byte: symbol.range_end_byte,
            }),
        }
    }

    fn file_query(
        &self,
        request: &QueryRequest,
        path: &Path,
        origin: FileOrigin,
        cancellation: &CancellationFlag,
    ) -> Result<Vec<ReferenceCandidate>> {
        let Some((parsed, version)) = self.parse_cancellable(path, Some(cancellation))? else {
            if cancellation.is_cancelled() {
                return Ok(Vec::new());
            }
            bail!("symbol completion is unavailable for this file")
        };
        let canonical_path = path.canonicalize()?;
        let relative = canonical_path
            .strip_prefix(&self.canonical_root)
            .context("file is outside search root")?
            .to_string_lossy()
            .replace('\\', "/");
        Ok(language::find_symbols(&parsed.symbols, &request.query)
            .into_iter()
            .take(request.limit)
            .take_while(|_| !cancellation.is_cancelled())
            .map(|symbol| IndexedSymbol {
                relative_path: relative.clone(),
                canonical_path: canonical_path.clone(),
                source_version: version.clone(),
                symbol: symbol.clone(),
                origin,
            })
            .map(|entry| self.candidate(&entry, request.generation, &request.typed_leader))
            .collect())
    }

    #[cfg(test)]
    fn index_launch_count(&self) -> usize {
        self.index_launches.load(Ordering::Relaxed)
    }
    #[cfg(test)]
    fn parse_count(&self) -> usize {
        self.parse_count.load(Ordering::Relaxed)
    }
}

impl ReferenceProvider for SymbolProvider {
    fn kind(&self) -> ReferenceKind {
        ReferenceKind::Symbol
    }

    fn query(
        &self,
        request: QueryRequest,
        cancellation: &CancellationFlag,
    ) -> Result<Vec<ReferenceCandidate>> {
        let mut latest = Vec::new();
        self.query_progressive(request, cancellation, &mut |emission| {
            latest = emission.candidates;
            Ok(())
        })?;
        Ok(latest)
    }

    fn query_progressive(
        &self,
        request: QueryRequest,
        cancellation: &CancellationFlag,
        emit: &mut dyn FnMut(QueryEmission) -> Result<()>,
    ) -> Result<()> {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        match &request.scope {
            QueryScope::File { path, origin } => {
                let candidates = self.file_query(&request, path, *origin, cancellation)?;
                emit(QueryEmission {
                    generation: request.generation,
                    candidates,
                    completed: true,
                    progress: None,
                })
            }
            QueryScope::Repository => self.repository_query(request, cancellation, emit),
        }
    }

    fn resolve(&self, id: &CandidateId) -> Result<ReferenceTarget> {
        anyhow::ensure!(
            id.provider == ReferenceKind::Symbol,
            "candidate belongs to another provider"
        );
        self.targets
            .lock()
            .unwrap()
            .get(&id.opaque)
            .cloned()
            .map(ReferenceTarget::Symbol)
            .context("symbol candidate is no longer available")
    }

    fn validate(&self, target: &ReferenceTarget) -> Result<ValidatedTarget> {
        let ReferenceTarget::Symbol(symbol) = target else {
            bail!("symbol provider cannot validate this target")
        };
        let original_version = symbol
            .file
            .source_version
            .as_ref()
            .context("symbol target has no source version")?;
        let version =
            file_version(&symbol.file.canonical_path).context("selected file no longer exists")?;
        if original_version == &version {
            return Ok(ValidatedTarget {
                target: target.clone(),
                context_cost: ContextCost::Pending,
            });
        }
        let Some((parsed, refreshed_version)) = self.parse(&symbol.file.canonical_path)? else {
            bail!("symbol completion is unavailable for this file")
        };
        let matches: Vec<_> = parsed
            .symbols
            .iter()
            .filter(|candidate| {
                identity_for(&symbol.file.canonical_path, candidate) == symbol.identity
            })
            .collect();
        let [refreshed] = matches.as_slice() else {
            if matches.is_empty() {
                bail!("selected symbol no longer exists")
            }
            bail!("selected symbol is now ambiguous")
        };
        let entry = IndexedSymbol {
            relative_path: symbol.file.relative_path.clone(),
            canonical_path: symbol.file.canonical_path.clone(),
            source_version: refreshed_version,
            symbol: (*refreshed).clone(),
            origin: symbol.file.origin,
        };
        Ok(ValidatedTarget {
            target: ReferenceTarget::Symbol(target_for(&entry)),
            context_cost: ContextCost::Pending,
        })
    }

    fn lower(&self, target: &ValidatedTarget) -> Result<String> {
        let ReferenceTarget::Symbol(symbol) = &target.target else {
            bail!("symbol provider cannot lower this target")
        };
        if let Some(anchor) = &symbol.markdown_anchor {
            Ok(format!("{}#{anchor}", symbol.file.relative_path))
        } else {
            Ok(format!(
                "{}::{}:{} {}",
                symbol.file.relative_path,
                symbol.location.line,
                symbol.location.column,
                symbol.identity.leaf_name
            ))
        }
    }

    fn preview(&self, target: &ReferenceTarget) -> Result<Option<Preview>> {
        let ReferenceTarget::Symbol(symbol) = target else {
            bail!("symbol provider cannot preview this target")
        };
        let source = std::fs::read_to_string(&symbol.file.canonical_path)?;
        Ok(Some(Preview {
            title: Some(symbol.file.relative_path.clone()),
            lines: source
                .lines()
                .enumerate()
                .map(|(index, text)| PreviewLine {
                    number: Some(index + 1),
                    text: text.into(),
                })
                .collect(),
            highlighted_lines: Some(symbol.location.line..=symbol.location.line),
        }))
    }

    fn context_cost(&self, _target: &ReferenceTarget) -> Result<ContextCost> {
        Ok(ContextCost::Pending)
    }
}
