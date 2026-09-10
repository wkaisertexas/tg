use super::*;
use crate::test_support::ManualExecutor;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

mod cache;
mod context;
mod service;

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
