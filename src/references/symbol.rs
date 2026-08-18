use super::file::{file_version, read_versioned};
use super::model::{
    CandidateDisplay, CandidateId, CandidateTokenSource, ContextCost, FileOrigin, FileTarget,
    FileVersion, Preview, PreviewLine, QueryEmission, QueryProgress, QueryRequest, QueryScope,
    ReferenceCandidate, ReferenceKind, ReferenceTarget, SourceLocation, SymbolIdentity,
    SymbolTarget, ValidatedTarget,
};
use super::{CancellationFlag, ReferenceProvider};
use crate::language::{self, ParsedFile, Symbol};
use crate::search::{self, SearchMode};
use anyhow::{Context, Result, bail};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct IndexedSymbol {
    relative_path: String,
    canonical_path: PathBuf,
    source_version: FileVersion,
    symbol: Symbol,
    origin: FileOrigin,
}

#[derive(Default)]
struct RepositoryIndex {
    started: bool,
    done: bool,
    entries: Vec<IndexedSymbol>,
    scanned: usize,
    total: usize,
    subscribers: Vec<SyncSender<()>>,
}

struct CachedParse {
    version: FileVersion,
    parsed: Arc<ParsedFile>,
}

fn notify_subscribers(subscribers: &mut Vec<SyncSender<()>>) {
    subscribers.retain(|sink| match sink.try_send(()) {
        Ok(()) | Err(mpsc::TrySendError::Full(())) => true,
        Err(mpsc::TrySendError::Disconnected(())) => false,
    });
}

pub struct SymbolProvider {
    root: PathBuf,
    canonical_root: PathBuf,
    leader: String,
    batch_size: usize,
    index: Arc<Mutex<RepositoryIndex>>,
    parse_cache: Mutex<HashMap<PathBuf, CachedParse>>,
    targets: Mutex<HashMap<String, SymbolTarget>>,
    index_launches: Arc<AtomicUsize>,
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
            index_launches: Arc::new(AtomicUsize::new(0)),
            parse_count: AtomicUsize::new(0),
            broad_excludes: broad_excludes.to_vec(),
        })
    }

    fn parse(&self, path: &Path) -> Result<Option<(Arc<ParsedFile>, FileVersion)>> {
        self.parse_cancellable(path, None)
    }

    fn parse_cancellable(
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

    fn subscribe_index(&self) -> (Vec<IndexedSymbol>, QueryProgress, bool, Receiver<()>) {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut index = self.index.lock().unwrap();
        let snapshot = index.entries.clone();
        let progress = QueryProgress {
            scanned: index.scanned,
            total: index.total,
            indexed_symbols: index.entries.len(),
        };
        let done = index.done;
        if !done {
            index.subscribers.push(sender);
        }
        if !index.started {
            index.started = true;
            self.start_index_worker();
        }
        (snapshot, progress, done, receiver)
    }

    fn start_index_worker(&self) {
        let root = self.root.clone();
        let canonical_root = self.canonical_root.clone();
        let index = Arc::clone(&self.index);
        let launches = Arc::clone(&self.index_launches);
        let batch_size = self.batch_size;
        let broad_excludes = self.broad_excludes.clone();
        std::thread::spawn(move || {
            launches.fetch_add(1, Ordering::Relaxed);
            let files: Vec<_> =
                search::walk_with_excludes(&root, SearchMode::Broad, &broad_excludes)
                    .into_iter()
                    .filter(|path| language::may_support_with_source(path))
                    .collect();
            {
                let mut state = index.lock().unwrap();
                state.total = files.len();
                notify_subscribers(&mut state.subscribers);
            }
            language::install_indexing(|| {
                files.par_chunks(batch_size).for_each(|paths| {
                    let mut entries = Vec::new();
                    for path in paths {
                        let Ok(canonical_path) = path.canonicalize() else {
                            continue;
                        };
                        if !canonical_path.starts_with(&canonical_root) {
                            continue;
                        }
                        let Ok(snapshot) = read_versioned(&canonical_path) else {
                            continue;
                        };
                        let Ok(source) = String::from_utf8(snapshot.bytes) else {
                            continue;
                        };
                        let Ok(Some(parsed)) =
                            language::parse_indexed_source(&canonical_path, source)
                        else {
                            continue;
                        };
                        let source_version = snapshot.version;
                        let relative_path = path
                            .strip_prefix(&root)
                            .unwrap_or(path)
                            .to_string_lossy()
                            .replace('\\', "/");
                        entries.extend(parsed.symbols.into_iter().map(|symbol| IndexedSymbol {
                            relative_path: relative_path.clone(),
                            canonical_path: canonical_path.clone(),
                            source_version: source_version.clone(),
                            symbol,
                            origin: FileOrigin::Broad,
                        }));
                    }
                    let mut state = index.lock().unwrap();
                    state.scanned += paths.len();
                    state.entries.extend(entries);
                    notify_subscribers(&mut state.subscribers);
                });
            });
            let mut state = index.lock().unwrap();
            state.done = true;
            notify_subscribers(&mut state.subscribers);
            state.subscribers.clear();
        });
    }

    fn repository_query(
        &self,
        request: QueryRequest,
        cancellation: &CancellationFlag,
        emit: &mut dyn FnMut(QueryEmission) -> Result<()>,
    ) -> Result<()> {
        let (mut entries, progress, done, receiver) = self.subscribe_index();
        emit(self.emission(&request, &entries, done, progress))?;
        if done {
            return Ok(());
        }
        while !cancellation.is_cancelled() {
            match receiver.recv_timeout(std::time::Duration::from_millis(20)) {
                Ok(()) => {
                    let (new_entries, progress, done) = self.index_snapshot(entries.len());
                    entries.extend(new_entries);
                    emit(self.emission(&request, &entries, done, progress))?;
                    if done {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let (new_entries, progress, done) = self.index_snapshot(entries.len());
                    entries.extend(new_entries);
                    emit(self.emission(&request, &entries, done, progress))?;
                    break;
                }
            }
        }
        Ok(())
    }

    fn emission(
        &self,
        request: &QueryRequest,
        entries: &[IndexedSymbol],
        completed: bool,
        progress: QueryProgress,
    ) -> QueryEmission {
        let candidates = rank_repository_symbols(entries, &request.query, request.limit)
            .into_iter()
            // Standalone `::` results become ordinary file-symbol references;
            // the provider's configured file leader is therefore used here.
            .map(|entry| self.candidate(entry, request.generation, ""))
            .collect();
        QueryEmission {
            generation: request.generation,
            candidates,
            completed,
            progress: Some(progress),
        }
    }

    fn index_snapshot(&self, seen: usize) -> (Vec<IndexedSymbol>, QueryProgress, bool) {
        let state = self.index.lock().unwrap();
        (
            state.entries[seen.min(state.entries.len())..].to_vec(),
            QueryProgress {
                scanned: state.scanned,
                total: state.total,
                indexed_symbols: state.entries.len(),
            },
            state.done,
        )
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

fn identity_for(path: &Path, symbol: &Symbol) -> SymbolIdentity {
    let language = if path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "md" | "markdown"))
    {
        "markdown".into()
    } else {
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown")
            .to_ascii_lowercase()
    };
    SymbolIdentity {
        language,
        qualified_name: symbol.qualified_name.clone(),
        leaf_name: symbol.leaf_name.clone(),
        kind: symbol.kind.clone(),
        is_definition: symbol.is_definition,
    }
}

fn target_for(entry: &IndexedSymbol) -> SymbolTarget {
    SymbolTarget {
        file: FileTarget {
            canonical_path: entry.canonical_path.clone(),
            relative_path: entry.relative_path.clone(),
            origin: entry.origin,
            source_version: Some(entry.source_version.clone()),
        },
        identity: identity_for(&entry.canonical_path, &entry.symbol),
        start_byte: entry.symbol.range_start_byte,
        end_byte: entry.symbol.range_end_byte,
        name_start_byte: entry.symbol.name_start_byte,
        name_end_byte: entry.symbol.name_end_byte,
        location: SourceLocation {
            line: entry.symbol.start.line,
            column: entry.symbol.start.column,
        },
        markdown_anchor: language::is_markdown_symbol(&entry.symbol)
            .then(|| language::markdown_slug(&entry.symbol.leaf_name)),
    }
}

fn stable_candidate_id(target: &SymbolTarget) -> String {
    let mut digest = Sha256::new();
    digest.update(target.file.canonical_path.to_string_lossy().as_bytes());
    digest.update(target.file.relative_path.as_bytes());
    digest.update([match target.file.origin {
        FileOrigin::GitAware => 0,
        FileOrigin::Broad => 1,
    }]);
    digest.update(target.identity.language.as_bytes());
    digest.update(target.identity.qualified_name.as_bytes());
    digest.update(target.identity.leaf_name.as_bytes());
    digest.update(target.identity.kind.as_bytes());
    digest.update([u8::from(target.identity.is_definition)]);
    digest.update(target.start_byte.to_le_bytes());
    digest.update(target.end_byte.to_le_bytes());
    if let Some(version) = &target.file.source_version {
        digest.update(version.content_sha256);
    }
    format!("{:x}", digest.finalize())
}

fn rank_repository_symbols<'a>(
    index: &'a [IndexedSymbol],
    query: &str,
    limit: usize,
) -> Vec<&'a IndexedSymbol> {
    let matcher = SkimMatcherV2::default().ignore_case();
    let member_query = query.rsplit_once('.').filter(|(parent, _)| {
        index.iter().any(|candidate| {
            language::names_equivalent(&candidate.symbol.leaf_name, parent)
                || language::names_equivalent(&candidate.symbol.qualified_name, parent)
        })
    });
    let query_lower = query.to_ascii_lowercase();
    let mut matches: Vec<_> = index
        .iter()
        .filter_map(|candidate| {
            let leaf = candidate.symbol.leaf_name.to_ascii_lowercase();
            let score = if let Some((parent, member)) = member_query {
                language::member_score(&candidate.symbol, parent, member, &matcher)?
            } else if query.is_empty() {
                0
            } else if leaf == query_lower {
                1_000_000
            } else if leaf.starts_with(&query_lower) {
                500_000
            } else {
                matcher
                    .fuzzy_match(&candidate.symbol.leaf_name, query)
                    .or_else(|| matcher.fuzzy_match(&candidate.symbol.qualified_name, query))?
            };
            Some((score, candidate))
        })
        .collect();
    let compare = |(left_score, left): &(i64, &IndexedSymbol),
                   (right_score, right): &(i64, &IndexedSymbol)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
            .then_with(|| left.symbol.start.line.cmp(&right.symbol.start.line))
    };
    if matches.len() > limit {
        matches.select_nth_unstable_by(limit, compare);
        matches.truncate(limit);
    }
    matches.sort_by(compare);
    matches
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::model::GenerationId;
    use std::fs;

    fn request(generation: u64, query: &str, scope: QueryScope) -> QueryRequest {
        QueryRequest {
            generation: GenerationId(generation),
            query: query.into(),
            scope,
            limit: 100,
            typed_leader: "@".into(),
        }
    }

    fn file_scope(path: &Path, relative: &str) -> QueryScope {
        let _ = relative;
        QueryScope::File {
            path: path.to_path_buf(),
            origin: FileOrigin::GitAware,
        }
    }

    fn query_file(
        provider: &SymbolProvider,
        path: &Path,
        relative: &str,
        query: &str,
    ) -> Vec<ReferenceCandidate> {
        provider
            .query(
                request(1, query, file_scope(path, relative)),
                &CancellationFlag::default(),
            )
            .unwrap()
    }

    fn resolved(provider: &SymbolProvider, candidate: &ReferenceCandidate) -> SymbolTarget {
        let ReferenceTarget::Symbol(target) = provider.resolve(&candidate.id).unwrap() else {
            panic!("expected symbol")
        };
        target
    }

    #[test]
    fn file_queries_preserve_exact_and_direct_child_ranking_and_ranges() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("model.rs");
        fs::write(
            &path,
            "struct User;\nimpl User { fn render(&self) {} fn renderer(&self) {} }\nfn render() {}\n",
        )
        .unwrap();
        let provider = SymbolProvider::new(temp.path(), "@").unwrap();
        let exact = query_file(&provider, &path, "model.rs", "render");
        assert_eq!(exact[0].display.primary, "render");
        let child = query_file(&provider, &path, "model.rs", "User.render");
        assert_eq!(child[0].display.primary, "User::render");
        let target = resolved(&provider, &child[0]);
        assert!(target.start_byte < target.name_start_byte);
        assert!(target.name_end_byte <= target.end_byte);
        assert_eq!(target.file.origin, FileOrigin::GitAware);
        assert_eq!(child[0].source_version, target.file.source_version);
        assert_eq!(child[0].context_cost, ContextCost::Pending);
        assert_eq!(child[0].file_context_cost, Some(ContextCost::Pending));
        assert!(matches!(
            &child[0].token_source,
            Some(CandidateTokenSource::Symbol {
                path: candidate_path,
                start_byte,
                end_byte,
            }) if candidate_path == &path.canonicalize().unwrap()
                && *start_byte == target.start_byte
                && *end_byte == target.end_byte
        ));
    }

    #[test]
    fn repository_ranking_caps_results_and_uses_path_then_line_ties() {
        let version = FileVersion {
            size: 0,
            modified: None,
            content_sha256: [0; 32],
        };
        let entries: Vec<_> = (0..110)
            .map(|index| IndexedSymbol {
                relative_path: format!("{index:03}.rs"),
                canonical_path: PathBuf::from(format!("{index:03}.rs")),
                source_version: version.clone(),
                origin: FileOrigin::Broad,
                symbol: Symbol {
                    leaf_name: "render".into(),
                    qualified_name: "render".into(),
                    kind: "function".into(),
                    start: language::SourcePoint { line: 2, column: 1 },
                    name_start_byte: 0,
                    name_end_byte: 6,
                    range_start_byte: 0,
                    range_end_byte: 8,
                    is_definition: true,
                },
            })
            .collect();
        let ranked = rank_repository_symbols(&entries, "render", 100);
        assert_eq!(ranked.len(), 100);
        assert_eq!(ranked[0].relative_path, "000.rs");
        assert_eq!(ranked[99].relative_path, "099.rs");
        assert_eq!(
            rank_repository_symbols(&entries, "ren", 1)[0]
                .symbol
                .leaf_name,
            "render"
        );

        let mut qualified = entries[0].clone();
        qualified.symbol.leaf_name = "work".into();
        qualified.symbol.qualified_name = "SpecialModule::work".into();
        assert_eq!(
            rank_repository_symbols(&[qualified], "SpecialModule", 1)[0]
                .symbol
                .leaf_name,
            "work"
        );
    }

    #[test]
    fn repository_index_is_lazy_progressive_includes_ignored_files_and_reuses_one_launch() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join(".gitignore"), "ignored.rs\n").unwrap();
        fs::write(temp.path().join("visible.rs"), "struct Visible;\n").unwrap();
        fs::write(temp.path().join("ignored.rs"), "struct Ignored;\n").unwrap();
        let provider = SymbolProvider::with_batch_size(temp.path(), "@", 1, &[]).unwrap();
        assert_eq!(provider.index_launch_count(), 0);

        let mut emissions = Vec::new();
        provider
            .query_progressive(
                request(7, "", QueryScope::Repository),
                &CancellationFlag::default(),
                &mut |emission| {
                    emissions.push(emission);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(provider.index_launch_count(), 1);
        assert!(emissions.len() >= 2);
        assert!(emissions.last().unwrap().completed);
        let progress = emissions.last().unwrap().progress.unwrap();
        assert_eq!(progress.scanned, 2);
        assert_eq!(progress.total, 2);
        assert_eq!(
            progress.indexed_symbols,
            emissions.last().unwrap().candidates.len()
        );
        assert!(
            emissions
                .iter()
                .all(|emission| emission.generation == GenerationId(7))
        );
        let ignored_id = emissions
            .last()
            .unwrap()
            .candidates
            .iter()
            .find(|candidate| candidate.friendly_text.contains("ignored.rs"))
            .unwrap()
            .id
            .clone();
        let entries = provider.index.lock().unwrap().entries.clone();
        let repeated = provider.emission(
            &request(7, "", QueryScope::Repository),
            &entries,
            true,
            progress,
        );
        assert!(
            repeated
                .candidates
                .iter()
                .any(|candidate| candidate.id == ignored_id)
        );
        provider.resolve(&ignored_id).unwrap();
        assert!(
            emissions
                .last()
                .unwrap()
                .candidates
                .iter()
                .any(|candidate| candidate.friendly_text.contains("ignored.rs"))
        );

        provider
            .query(
                request(8, "Visible", QueryScope::Repository),
                &CancellationFlag::default(),
            )
            .unwrap();
        assert_eq!(provider.index_launch_count(), 1);
    }

    #[test]
    fn repository_index_lazily_detects_extensionless_shell_scripts() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("deploy"),
            "#!/usr/bin/env bash\ndeploy_app() { :; }\n",
        )
        .unwrap();
        fs::write(temp.path().join("README"), "plain extensionless text\n").unwrap();
        let provider = SymbolProvider::with_batch_size(temp.path(), "@", 1, &[]).unwrap();
        assert_eq!(provider.index_launch_count(), 0);

        let mut emissions = Vec::new();
        provider
            .query_progressive(
                request(9, "deploy_app", QueryScope::Repository),
                &CancellationFlag::default(),
                &mut |emission| {
                    emissions.push(emission);
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(provider.index_launch_count(), 1);
        let final_emission = emissions.last().unwrap();
        assert!(final_emission.completed);
        assert_eq!(final_emission.progress.unwrap().scanned, 2);
        assert_eq!(final_emission.progress.unwrap().total, 2);
        assert!(final_emission.candidates.iter().any(|candidate| {
            candidate.display.primary == "deploy_app" && candidate.friendly_text.contains("deploy")
        }));
        assert!(
            provider
                .index
                .lock()
                .unwrap()
                .entries
                .iter()
                .all(|entry| entry.relative_path != "README")
        );
    }

    #[test]
    fn unchanged_targets_use_fast_path_and_moved_targets_refresh() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("move.rs");
        fs::write(&path, "fn naïve() {}\n").unwrap();
        let provider = SymbolProvider::new(temp.path(), "@").unwrap();
        let candidates = query_file(&provider, &path, "move.rs", "naïve");
        let target = ReferenceTarget::Symbol(resolved(&provider, &candidates[0]));
        let parsed = provider.parse_count();
        provider.validate(&target).unwrap();
        assert_eq!(provider.parse_count(), parsed);

        fs::write(&path, "\n\nfn naïve() {}\n").unwrap();
        let validated = provider.validate(&target).unwrap();
        let ReferenceTarget::Symbol(refreshed) = validated.target else {
            panic!()
        };
        assert_eq!(refreshed.location.line, 3);
        assert_eq!(
            &fs::read_to_string(&path).unwrap()[refreshed.name_start_byte..refreshed.name_end_byte],
            "naïve"
        );
    }

    #[test]
    fn parsed_symbols_and_version_share_the_same_byte_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("snapshot.rs");
        fs::write(&path, "fn before() {}\n").unwrap();
        let snapshot = read_versioned(&path).unwrap();
        fs::write(&path, "fn after() {}\n").unwrap();
        let parsed =
            language::parse_source(&path, String::from_utf8(snapshot.bytes.clone()).unwrap())
                .unwrap()
                .unwrap();
        assert_eq!(parsed.symbols[0].leaf_name, "before");
        assert_eq!(
            snapshot.version.content_sha256,
            Sha256::digest(&snapshot.bytes).as_slice()
        );
        assert_ne!(snapshot.version, file_version(&path).unwrap());
    }

    #[test]
    fn changed_missing_ambiguous_and_deleted_targets_fail() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("stale.rs");
        fs::write(&path, "fn target() {}\n").unwrap();
        let provider = SymbolProvider::new(temp.path(), "@").unwrap();
        let candidate = query_file(&provider, &path, "stale.rs", "target").remove(0);
        let target = ReferenceTarget::Symbol(resolved(&provider, &candidate));

        fs::write(&path, "fn renamed() {}\n").unwrap();
        assert!(
            provider
                .validate(&target)
                .unwrap_err()
                .to_string()
                .contains("no longer exists")
        );
        fs::write(&path, "fn target() {}\nfn target() {}\n").unwrap();
        assert!(
            provider
                .validate(&target)
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        fs::remove_file(&path).unwrap();
        assert!(
            provider
                .validate(&target)
                .unwrap_err()
                .to_string()
                .contains("selected file no longer exists")
        );
    }

    #[test]
    fn markdown_targets_refresh_and_lower_to_anchors() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("guide.md");
        fs::write(&path, "# Guide\n\n## Quick Start\n").unwrap();
        let provider = SymbolProvider::new(temp.path(), "@").unwrap();
        let candidate = query_file(&provider, &path, "guide.md", "QuickStart").remove(0);
        let target = ReferenceTarget::Symbol(resolved(&provider, &candidate));
        let lowered = provider
            .lower(&provider.validate(&target).unwrap())
            .unwrap();
        assert_eq!(lowered, "guide.md#quick-start");

        fs::write(&path, "\n# Guide\n\n## Quick Start\n").unwrap();
        let refreshed = provider.validate(&target).unwrap();
        let ReferenceTarget::Symbol(symbol) = &refreshed.target else {
            panic!()
        };
        assert_eq!(symbol.location.line, 4);
        fs::write(&path, "# Guide\n\n## Setup\n").unwrap();
        assert!(provider.validate(&target).is_err());
    }
}
