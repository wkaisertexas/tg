mod cache;
mod context;
#[cfg(test)]
mod tests;

use crate::references::BackgroundExecutor;
use anyhow::{Context, Result};
pub use cache::FileCacheIdentity;
use cache::{TokenCache, count_file, count_range, stable_file};
pub use context::{
    ContextFileVersion, ContextIdentity, ContextTotal, ReferenceContext, context_total,
    format_tokens,
};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

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
    counter: Arc<dyn TokenCounter>,
    executor: Arc<dyn BackgroundExecutor>,
    cache: Arc<TokenCache>,
    sender: Sender<TokenUpdate>,
    receiver: Receiver<TokenUpdate>,
    next_request: AtomicU64,
}

impl TokenService {
    pub fn new(executor: Arc<dyn BackgroundExecutor>) -> Self {
        Self::with_counter(executor, Arc::new(Gpt4oCounter))
    }

    pub fn for_tokenizer(executor: Arc<dyn BackgroundExecutor>, tokenizer: &str) -> Result<Self> {
        TokenizerName::parse(tokenizer)?;
        Ok(Self::new(executor))
    }

    pub fn with_counter(
        executor: Arc<dyn BackgroundExecutor>,
        counter: Arc<dyn TokenCounter>,
    ) -> Self {
        TokenizerName::parse(counter.name())
            .expect("token counter must use a registered tokenizer name");
        let (sender, receiver) = mpsc::channel();
        Self {
            counter,
            executor,
            cache: Arc::default(),
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
        self.request(generation, revision, subject, move |counter, cache| {
            count_file(&path, counter, cache)
        })
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
        self.request(generation, revision, subject, move |counter, cache| {
            count_range(file, &source, range, counter, cache)
        })
    }

    pub fn request_file_range(
        &self,
        path: PathBuf,
        range: Range<usize>,
        generation: TokenGeneration,
        revision: DocumentRevision,
        subject: TokenSubject,
    ) -> TokenTicket {
        self.request(generation, revision, subject, move |counter, cache| {
            let (file, bytes) = stable_file(&path)?;
            let source = std::str::from_utf8(&bytes).context("source is not valid UTF-8")?;
            count_range(file, source, SymbolRange::Bytes(range), counter, cache)
        })
    }

    fn request(
        &self,
        generation: TokenGeneration,
        revision: DocumentRevision,
        subject: TokenSubject,
        count: impl FnOnce(&dyn TokenCounter, &TokenCache) -> Result<TokenState> + Send + 'static,
    ) -> TokenTicket {
        let id = self.next_id();
        let sender = self.sender.clone();
        let cache = Arc::clone(&self.cache);
        let counter = Arc::clone(&self.counter);
        self.executor.spawn(Box::new(move || {
            let state = count(counter.as_ref(), &cache).unwrap_or(TokenState::Unavailable);
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
