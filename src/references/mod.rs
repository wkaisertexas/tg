pub mod file;
pub mod model;
pub mod session;
pub mod symbol;

use anyhow::Result;
use model::{
    CandidateId, ContextCost, Preview, QueryEmission, QueryRequest, ReferenceCandidate,
    ReferenceKind, ReferenceTarget, ValidatedTarget,
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
        static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
        POOL.get_or_init(|| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(
                    std::thread::available_parallelism()
                        .map_or(4, usize::from)
                        .min(8),
                )
                .thread_name(|index| format!("tg-provider-{index}"))
                .build()
                .expect("could not create provider workers")
        })
        .spawn(job);
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

    fn query_progressive(
        &self,
        request: QueryRequest,
        cancellation: &CancellationFlag,
        emit: &mut dyn FnMut(QueryEmission) -> Result<()>,
    ) -> Result<()> {
        let generation = request.generation;
        let candidates = self.query(request, cancellation)?;
        emit(QueryEmission {
            generation,
            candidates,
            completed: true,
        })
    }

    fn resolve(&self, id: &CandidateId) -> Result<ReferenceTarget>;
    fn validate(&self, target: &ReferenceTarget) -> Result<ValidatedTarget>;
    fn lower(&self, target: &ValidatedTarget) -> Result<String>;
    fn preview(&self, target: &ReferenceTarget) -> Result<Option<Preview>>;
    fn context_cost(&self, target: &ReferenceTarget) -> Result<ContextCost>;
}
