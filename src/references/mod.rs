pub mod file;
pub mod model;
pub mod session;

use anyhow::Result;
use model::{
    CandidateId, ContextCost, Preview, QueryRequest, ReferenceCandidate, ReferenceKind,
    ReferenceTarget, ValidatedTarget,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Default)]
pub struct CancellationFlag(Arc<AtomicBool>);

impl CancellationFlag {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub trait BackgroundExecutor: Send + Sync {
    fn spawn(&self, job: Box<dyn FnOnce() + Send>);
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadExecutor;

impl BackgroundExecutor for ThreadExecutor {
    fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(job);
    }
}

pub trait ReferenceProvider: Send + Sync {
    fn kind(&self) -> ReferenceKind;

    /// Performs blocking provider work. The session always invokes this on its executor.
    fn query(
        &self,
        request: QueryRequest,
        cancellation: &CancellationFlag,
    ) -> Result<Vec<ReferenceCandidate>>;

    fn resolve(&self, id: &CandidateId) -> Result<ReferenceTarget>;
    fn validate(&self, target: &ReferenceTarget) -> Result<ValidatedTarget>;
    fn lower(&self, target: &ValidatedTarget) -> Result<String>;
    fn preview(&self, target: &ReferenceTarget) -> Result<Option<Preview>>;
    fn context_cost(&self, target: &ReferenceTarget) -> Result<ContextCost>;
}
