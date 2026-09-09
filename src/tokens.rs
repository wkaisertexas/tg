use crate::references::BackgroundExecutor;
use crate::references::model::char_to_byte;
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

pub const GPT4O_TOKENIZER: &str = "gpt-4o";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TokenizerName(Arc<str>);

impl TokenizerName {
    pub fn parse(value: &str) -> Result<Self> {
        anyhow::ensure!(value == GPT4O_TOKENIZER, "unknown tokenizer `{value}`");
        Ok(Self(value.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for TokenizerName {
    fn default() -> Self {
        Self(GPT4O_TOKENIZER.into())
    }
}

pub trait TokenCounter: Send + Sync {
    fn name(&self) -> &str;
    fn count(&self, text: &str) -> Result<usize>;
}

#[derive(Debug, Default)]
pub struct Gpt4oCounter;

impl TokenCounter for Gpt4oCounter {
    fn name(&self) -> &str {
        GPT4O_TOKENIZER
    }

    fn count(&self, text: &str) -> Result<usize> {
        Ok(tiktoken_rs::o200k_base_singleton()
            .encode_ordinary(text)
            .len())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenGeneration(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentRevision(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenRequestId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TokenSubject(pub String);

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
enum TokenCacheKey {
    File {
        file: FileCacheIdentity,
        tokenizer: TokenizerName,
    },
    Range {
        file: FileCacheIdentity,
        start_byte: usize,
        end_byte: usize,
        tokenizer: TokenizerName,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenState {
    Pending,
    Ready(usize),
    Bytes(u64),
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct TokenTicket {
    pub id: TokenRequestId,
    pub state: TokenState,
}

#[derive(Debug, Clone)]
pub struct TokenUpdate {
    pub id: TokenRequestId,
    pub generation: TokenGeneration,
    pub revision: DocumentRevision,
    pub subject: TokenSubject,
    pub state: TokenState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolRange {
    Bytes(Range<usize>),
    Characters(Range<usize>),
}

pub struct TokenService {
    tokenizer: TokenizerName,
    counter: Arc<dyn TokenCounter>,
    executor: Arc<dyn BackgroundExecutor>,
    cache: Arc<Mutex<HashMap<TokenCacheKey, usize>>>,
    sender: Sender<TokenUpdate>,
    receiver: Receiver<TokenUpdate>,
    next_request: AtomicU64,
}

impl TokenService {
    pub fn new(executor: Arc<dyn BackgroundExecutor>) -> Self {
        Self::with_counter(executor, Arc::new(Gpt4oCounter))
    }

    pub fn for_tokenizer(executor: Arc<dyn BackgroundExecutor>, tokenizer: &str) -> Result<Self> {
        let tokenizer = TokenizerName::parse(tokenizer)?;
        match tokenizer.as_str() {
            GPT4O_TOKENIZER => Ok(Self::new(executor)),
            _ => unreachable!("TokenizerName accepts only registered tokenizers"),
        }
    }

    pub fn with_counter(
        executor: Arc<dyn BackgroundExecutor>,
        counter: Arc<dyn TokenCounter>,
    ) -> Self {
        let tokenizer = TokenizerName::parse(counter.name())
            .expect("token counter must use a registered tokenizer name");
        let (sender, receiver) = mpsc::channel();
        Self {
            tokenizer,
            counter,
            executor,
            cache: Arc::new(Mutex::new(HashMap::new())),
            sender,
            receiver,
            next_request: AtomicU64::new(0),
        }
    }

    pub fn request_file(
        &self,
        path: PathBuf,
        generation: TokenGeneration,
        revision: DocumentRevision,
        subject: TokenSubject,
    ) -> TokenTicket {
        let id = self.next_id();
        let sender = self.sender.clone();
        let cache = Arc::clone(&self.cache);
        let counter = Arc::clone(&self.counter);
        let tokenizer = self.tokenizer.clone();
        self.executor.spawn(Box::new(move || {
            let state = count_file(&path, &tokenizer, counter.as_ref(), &cache)
                .unwrap_or(TokenState::Unavailable);
            let _ = sender.send(TokenUpdate {
                id,
                generation,
                revision,
                subject,
                state,
            });
        }));
        TokenTicket {
            id,
            state: TokenState::Pending,
        }
    }

    pub fn request_range(
        &self,
        file: FileCacheIdentity,
        source: Arc<str>,
        range: SymbolRange,
        generation: TokenGeneration,
        revision: DocumentRevision,
        subject: TokenSubject,
    ) -> TokenTicket {
        let id = self.next_id();
        let sender = self.sender.clone();
        let cache = Arc::clone(&self.cache);
        let counter = Arc::clone(&self.counter);
        let tokenizer = self.tokenizer.clone();
        self.executor.spawn(Box::new(move || {
            let state = count_range(file, &source, range, &tokenizer, counter.as_ref(), &cache)
                .unwrap_or(TokenState::Unavailable);
            let _ = sender.send(TokenUpdate {
                id,
                generation,
                revision,
                subject,
                state,
            });
        }));
        TokenTicket {
            id,
            state: TokenState::Pending,
        }
    }

    pub fn request_file_range(
        &self,
        path: PathBuf,
        range: Range<usize>,
        generation: TokenGeneration,
        revision: DocumentRevision,
        subject: TokenSubject,
    ) -> TokenTicket {
        let id = self.next_id();
        let sender = self.sender.clone();
        let cache = Arc::clone(&self.cache);
        let counter = Arc::clone(&self.counter);
        let tokenizer = self.tokenizer.clone();
        self.executor.spawn(Box::new(move || {
            let state = (|| {
                let (file, bytes) = stable_file(&path)?;
                let source = std::str::from_utf8(&bytes).context("source is not valid UTF-8")?;
                count_range(
                    file,
                    source,
                    SymbolRange::Bytes(range),
                    &tokenizer,
                    counter.as_ref(),
                    &cache,
                )
            })()
            .unwrap_or(TokenState::Unavailable);
            let _ = sender.send(TokenUpdate {
                id,
                generation,
                revision,
                subject,
                state,
            });
        }));
        TokenTicket {
            id,
            state: TokenState::Pending,
        }
    }

    /// Drains completed work and rejects updates for superseded UI/document state.
    /// Stale jobs still populate the shared cache.
    pub fn drain_for(
        &self,
        generation: TokenGeneration,
        revision: DocumentRevision,
    ) -> Vec<TokenUpdate> {
        self.drain()
            .into_iter()
            .filter(|update| update.generation == generation && update.revision == revision)
            .collect()
    }

    pub fn drain(&self) -> Vec<TokenUpdate> {
        self.receiver.try_iter().collect()
    }

    pub fn count_text(&self, text: &str) -> Result<usize> {
        self.counter.count(text)
    }

    pub fn cache_len(&self) -> usize {
        self.cache.lock().unwrap().len()
    }

    fn next_id(&self) -> TokenRequestId {
        TokenRequestId(self.next_request.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

fn stable_file(path: &Path) -> Result<(FileCacheIdentity, Vec<u8>)> {
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

fn count_file(
    path: &Path,
    tokenizer: &TokenizerName,
    counter: &dyn TokenCounter,
    cache: &Mutex<HashMap<TokenCacheKey, usize>>,
) -> Result<TokenState> {
    let initial = FileCacheIdentity::from_path(path)?;
    let initial_key = TokenCacheKey::File {
        file: initial,
        tokenizer: tokenizer.clone(),
    };
    if let Some(count) = cache.lock().unwrap().get(&initial_key).copied() {
        return Ok(TokenState::Ready(count));
    }
    let (file, bytes) = stable_file(path)?;
    let key = TokenCacheKey::File {
        file,
        tokenizer: tokenizer.clone(),
    };
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

fn count_range(
    file: FileCacheIdentity,
    source: &str,
    range: SymbolRange,
    tokenizer: &TokenizerName,
    counter: &dyn TokenCounter,
    cache: &Mutex<HashMap<TokenCacheKey, usize>>,
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
        tokenizer: tokenizer.clone(),
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
    let character_count = source.chars().count();
    anyhow::ensure!(
        range.end <= character_count,
        "character range is out of bounds"
    );
    let start = char_to_byte(source, range.start).context("invalid character range start")?;
    let end = char_to_byte(source, range.end).context("invalid character range end")?;
    Ok(start..end)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContextFileVersion {
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub content_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContextIdentity {
    File(PathBuf),
    Range {
        canonical_path: PathBuf,
        start_byte: usize,
        end_byte: usize,
        file_version: ContextFileVersion,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceContext {
    pub identity: Option<ContextIdentity>,
    pub state: TokenState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextTotal {
    pub ready_tokens: usize,
    pub pending: usize,
    pub unavailable: usize,
}

pub fn context_total<'a>(contexts: impl IntoIterator<Item = &'a ReferenceContext>) -> ContextTotal {
    let contexts: Vec<_> = contexts.into_iter().collect();
    let files: HashSet<_> = contexts
        .iter()
        .filter_map(|context| match &context.identity {
            Some(ContextIdentity::File(path)) => Some(path.clone()),
            _ => None,
        })
        .collect();
    let mut unique: HashMap<ContextIdentity, TokenState> = HashMap::new();
    for context in contexts {
        let Some(identity) = &context.identity else {
            continue;
        };
        if matches!(identity, ContextIdentity::Range { canonical_path, .. } if files.contains(canonical_path))
        {
            continue;
        }
        unique
            .entry(identity.clone())
            .and_modify(|state| *state = preferred_state(state, &context.state))
            .or_insert_with(|| context.state.clone());
    }
    unique
        .values()
        .fold(ContextTotal::default(), |mut total, state| {
            match state {
                TokenState::Ready(tokens) => total.ready_tokens += tokens,
                TokenState::Pending => total.pending += 1,
                TokenState::Bytes(_) | TokenState::Unavailable => total.unavailable += 1,
            }
            total
        })
}

fn preferred_state(left: &TokenState, right: &TokenState) -> TokenState {
    match (left, right) {
        (TokenState::Ready(left), TokenState::Ready(right)) => {
            TokenState::Ready((*left).max(*right))
        }
        (TokenState::Ready(_), _) => left.clone(),
        (_, TokenState::Ready(_)) => right.clone(),
        (TokenState::Pending, _) => left.clone(),
        (_, TokenState::Pending) => right.clone(),
        (TokenState::Bytes(left), TokenState::Bytes(right)) => {
            TokenState::Bytes((*left).max(*right))
        }
        (TokenState::Bytes(_), TokenState::Unavailable) => left.clone(),
        _ => right.clone(),
    }
}

pub fn format_tokens(tokens: usize) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }
    let tenths = tokens.saturating_add(50) / 100;
    format!("{}.{:01}k", tenths / 10, tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::BackgroundExecutor;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct ManualExecutor {
        jobs: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    }

    impl BackgroundExecutor for ManualExecutor {
        fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
            self.jobs.lock().unwrap().push(job);
        }
    }

    impl ManualExecutor {
        fn run_next(&self) {
            let job = self.jobs.lock().unwrap().remove(0);
            job();
        }

        fn run_next_on_background_thread(&self) {
            let job = self.jobs.lock().unwrap().remove(0);
            std::thread::spawn(job).join().unwrap();
        }
    }

    struct CountingCounter {
        calls: Arc<AtomicUsize>,
    }

    struct ThreadRecordingCounter {
        threads: Arc<Mutex<Vec<std::thread::ThreadId>>>,
    }

    impl TokenCounter for ThreadRecordingCounter {
        fn name(&self) -> &str {
            GPT4O_TOKENIZER
        }

        fn count(&self, text: &str) -> Result<usize> {
            self.threads
                .lock()
                .unwrap()
                .push(std::thread::current().id());
            Ok(text.chars().count())
        }
    }

    impl TokenCounter for CountingCounter {
        fn name(&self) -> &str {
            GPT4O_TOKENIZER
        }

        fn count(&self, text: &str) -> Result<usize> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(text.chars().count())
        }
    }

    fn request_file(
        service: &TokenService,
        path: &Path,
        generation: u64,
        revision: u64,
    ) -> TokenTicket {
        service.request_file(
            path.to_path_buf(),
            TokenGeneration(generation),
            DocumentRevision(revision),
            TokenSubject("file".into()),
        )
    }

    fn version(source: &str) -> ContextFileVersion {
        ContextFileVersion {
            size: source.len() as u64,
            modified: None,
            content_sha256: Sha256::digest(source).into(),
        }
    }

    #[test]
    fn known_o200k_base_fixtures_are_exact() {
        let counter = Gpt4oCounter;
        for (text, expected) in [
            ("", 0),
            ("hello", 1),
            ("hello world", 2),
            ("antidisestablishmentarianism", 6),
            ("2 + 2 = 4", 7),
            ("お誕生日おめでとう", 8),
        ] {
            assert_eq!(counter.count(text).unwrap(), expected, "{text:?}");
        }
    }

    #[test]
    fn requests_are_pending_and_do_not_run_inline() {
        let executor = Arc::new(ManualExecutor::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let service = TokenService::with_counter(
            executor.clone(),
            Arc::new(CountingCounter {
                calls: calls.clone(),
            }),
        );
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.txt");
        fs::write(&path, "content").unwrap();

        assert_eq!(
            request_file(&service, &path, 1, 1).state,
            TokenState::Pending
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        executor.run_next();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn counting_runs_off_the_caller_thread() {
        let caller = std::thread::current().id();
        let executor = Arc::new(ManualExecutor::default());
        let threads = Arc::new(Mutex::new(Vec::new()));
        let service = TokenService::with_counter(
            executor.clone(),
            Arc::new(ThreadRecordingCounter {
                threads: threads.clone(),
            }),
        );
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("thread.txt");
        fs::write(&path, "content").unwrap();

        request_file(&service, &path, 1, 1);
        assert!(threads.lock().unwrap().is_empty());
        executor.run_next_on_background_thread();
        assert!(
            threads
                .lock()
                .unwrap()
                .iter()
                .all(|thread| *thread != caller)
        );
    }

    #[test]
    fn file_cache_hits_and_metadata_changes_invalidate_it() {
        let executor = Arc::new(ManualExecutor::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let service = TokenService::with_counter(
            executor.clone(),
            Arc::new(CountingCounter {
                calls: calls.clone(),
            }),
        );
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cache.txt");
        fs::write(&path, "one").unwrap();

        request_file(&service, &path, 1, 1);
        executor.run_next();
        assert_eq!(
            service.drain_for(TokenGeneration(1), DocumentRevision(1))[0].state,
            TokenState::Ready(3)
        );
        request_file(&service, &path, 1, 1);
        executor.run_next();
        assert_eq!(
            service.drain_for(TokenGeneration(1), DocumentRevision(1))[0].state,
            TokenState::Ready(3)
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        fs::write(&path, "different content").unwrap();
        request_file(&service, &path, 1, 2);
        executor.run_next();
        assert_eq!(
            service.drain_for(TokenGeneration(1), DocumentRevision(2))[0].state,
            TokenState::Ready(17)
        );
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(service.cache_len(), 2);
    }

    #[test]
    fn stale_generation_and_revision_updates_are_rejected_but_cached() {
        let executor = Arc::new(ManualExecutor::default());
        let service = TokenService::new(executor.clone());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("stale.txt");
        fs::write(&path, "hello").unwrap();
        request_file(&service, &path, 1, 4);
        executor.run_next();

        assert!(
            service
                .drain_for(TokenGeneration(2), DocumentRevision(4))
                .is_empty()
        );
        assert_eq!(service.cache_len(), 1);
    }

    #[test]
    fn byte_and_character_ranges_are_unicode_safe_and_share_the_cache() {
        let executor = Arc::new(ManualExecutor::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let service = TokenService::with_counter(
            executor.clone(),
            Arc::new(CountingCounter {
                calls: calls.clone(),
            }),
        );
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("unicode.txt");
        fs::write(&path, "a🦀bc").unwrap();
        let identity = FileCacheIdentity::from_path(&path).unwrap();
        let source: Arc<str> = "a🦀bc".into();

        service.request_range(
            identity.clone(),
            source.clone(),
            SymbolRange::Characters(1..3),
            TokenGeneration(1),
            DocumentRevision(1),
            TokenSubject("range".into()),
        );
        executor.run_next();
        assert_eq!(
            service.drain_for(TokenGeneration(1), DocumentRevision(1))[0].state,
            TokenState::Ready(2)
        );

        service.request_range(
            identity,
            source,
            SymbolRange::Bytes(1..6),
            TokenGeneration(1),
            DocumentRevision(1),
            TokenSubject("range".into()),
        );
        executor.run_next();
        assert_eq!(
            service.drain_for(TokenGeneration(1), DocumentRevision(1))[0].state,
            TokenState::Ready(2)
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn invalid_utf8_files_report_bytes_and_invalid_ranges_are_unavailable() {
        let executor = Arc::new(ManualExecutor::default());
        let service = TokenService::new(executor.clone());
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("binary.dat");
        fs::write(&path, [0xff, 0xfe]).unwrap();
        request_file(&service, &path, 1, 1);
        executor.run_next();
        assert_eq!(
            service.drain_for(TokenGeneration(1), DocumentRevision(1))[0].state,
            TokenState::Bytes(2)
        );

        let identity = FileCacheIdentity::from_path(&path).unwrap();
        service.request_range(
            identity,
            Arc::<str>::from("🦀"),
            SymbolRange::Bytes(1..2),
            TokenGeneration(1),
            DocumentRevision(1),
            TokenSubject("bad-range".into()),
        );
        executor.run_next();
        assert_eq!(
            service.drain_for(TokenGeneration(1), DocumentRevision(1))[0].state,
            TokenState::Unavailable
        );
    }

    #[test]
    fn totals_deduplicate_and_whole_files_subsume_ranges() {
        let path = PathBuf::from("/repo/src/lib.rs");
        let range = |start, end, state| ReferenceContext {
            identity: Some(ContextIdentity::Range {
                canonical_path: path.clone(),
                start_byte: start,
                end_byte: end,
                file_version: version("source"),
            }),
            state,
        };
        let first = range(0, 10, TokenState::Ready(20));
        let duplicate = range(0, 10, TokenState::Ready(20));
        let second = range(20, 30, TokenState::Pending);
        assert_eq!(
            context_total([&first, &duplicate, &second]),
            ContextTotal {
                ready_tokens: 20,
                pending: 1,
                unavailable: 0,
            }
        );

        let whole = ReferenceContext {
            identity: Some(ContextIdentity::File(path)),
            state: TokenState::Ready(100),
        };
        assert_eq!(
            context_total([&first, &duplicate, &second, &whole]),
            ContextTotal {
                ready_tokens: 100,
                pending: 0,
                unavailable: 0,
            }
        );
    }

    #[test]
    fn pending_whole_files_also_suppress_ranges_and_non_context_is_free() {
        let path = PathBuf::from("/repo/src/lib.rs");
        let range = ReferenceContext {
            identity: Some(ContextIdentity::Range {
                canonical_path: path.clone(),
                start_byte: 0,
                end_byte: 5,
                file_version: version("hello"),
            }),
            state: TokenState::Ready(1),
        };
        let whole = ReferenceContext {
            identity: Some(ContextIdentity::File(path)),
            state: TokenState::Pending,
        };
        let skill = ReferenceContext {
            identity: None,
            state: TokenState::Ready(999),
        };
        assert_eq!(
            context_total([&range, &whole, &skill]),
            ContextTotal {
                ready_tokens: 0,
                pending: 1,
                unavailable: 0,
            }
        );
    }

    #[test]
    fn token_formatting_uses_decimal_thousands_and_half_up_rounding() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_000), "1.0k");
        assert_eq!(format_tokens(1_249), "1.2k");
        assert_eq!(format_tokens(1_250), "1.3k");
        assert_eq!(format_tokens(18_700), "18.7k");
    }
}
