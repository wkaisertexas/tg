use super::*;
use crate::references::file::read_versioned;
use crate::search::{self, SearchMode};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use rayon::prelude::*;
use std::sync::mpsc::{self, Receiver, SyncSender};

#[derive(Default)]
pub(super) struct RepositoryIndex {
    started: bool,
    done: bool,
    entries: Vec<IndexedSymbol>,
    scanned: usize,
    total: usize,
    subscribers: Vec<SyncSender<()>>,
}

impl RepositoryIndex {
    fn snapshot(&self, seen: usize) -> (Vec<IndexedSymbol>, QueryProgress, bool) {
        (
            self.entries[seen.min(self.entries.len())..].to_vec(),
            QueryProgress {
                scanned: self.scanned,
                total: self.total,
                indexed_symbols: self.entries.len(),
            },
            self.done,
        )
    }

    fn notify_subscribers(&mut self) {
        self.subscribers
            .retain(|sink| !matches!(sink.try_send(()), Err(mpsc::TrySendError::Disconnected(()))));
    }
}

impl SymbolProvider {
    fn subscribe_index(&self) -> (Vec<IndexedSymbol>, QueryProgress, bool, Receiver<()>) {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut index = self.index.lock().unwrap();
        let (snapshot, progress, done) = index.snapshot(0);
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
        #[cfg(test)]
        let launches = Arc::clone(&self.index_launches);
        let batch_size = self.batch_size;
        let broad_excludes = self.broad_excludes.clone();
        std::thread::spawn(move || {
            #[cfg(test)]
            launches.fetch_add(1, Ordering::Relaxed);
            let files: Vec<_> =
                search::walk_with_excludes(&root, SearchMode::Broad, &broad_excludes)
                    .into_iter()
                    .filter(|path| language::may_support_with_source(path))
                    .collect();
            {
                let mut state = index.lock().unwrap();
                state.total = files.len();
                state.notify_subscribers();
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
                    state.notify_subscribers();
                });
            });
            let mut state = index.lock().unwrap();
            state.done = true;
            state.notify_subscribers();
            state.subscribers.clear();
        });
    }

    pub(super) fn repository_query(
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
            let disconnected = match receiver.recv_timeout(std::time::Duration::from_millis(20)) {
                Ok(()) => false,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => true,
            };
            let (new_entries, progress, done) = self.index_snapshot(entries.len());
            entries.extend(new_entries);
            emit(self.emission(&request, &entries, done, progress))?;
            if done || disconnected {
                break;
            }
        }
        Ok(())
    }

    pub(super) fn emission(
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

    pub(super) fn index_snapshot(&self, seen: usize) -> (Vec<IndexedSymbol>, QueryProgress, bool) {
        self.index.lock().unwrap().snapshot(seen)
    }
}

pub(super) fn rank_repository_symbols<'a>(
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
