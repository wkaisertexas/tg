use super::model::{
    AcceptedReference, CompletionActivation, GenerationId, LoweredReference, QueryEmission,
    QueryRequest, ReferenceCandidate, ReferenceId, ReferenceKind, ResolvedReference, SessionUpdate,
    SharedProvider, TextRange,
};
use super::{BackgroundExecutor, CancellationFlag, ReferenceProvider};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

enum QueryMessage {
    Results {
        generation: GenerationId,
        candidates: Vec<ReferenceCandidate>,
        completed: bool,
    },
    Failed {
        generation: GenerationId,
        message: String,
    },
}

pub struct ReferenceSession {
    providers: HashMap<ReferenceKind, SharedProvider>,
    executor: Arc<dyn BackgroundExecutor>,
    sender: Sender<QueryMessage>,
    receiver: Receiver<QueryMessage>,
    generation: GenerationId,
    cancellation: Option<CancellationFlag>,
    active_kind: Option<ReferenceKind>,
    active_range: Option<TextRange>,
    candidates: Vec<ReferenceCandidate>,
    selected: usize,
    next_reference_id: u64,
}

impl ReferenceSession {
    pub fn new(
        providers: impl IntoIterator<Item = Arc<dyn ReferenceProvider>>,
        executor: Arc<dyn BackgroundExecutor>,
    ) -> Result<Self> {
        let mut registry = HashMap::new();
        for provider in providers {
            anyhow::ensure!(
                registry.insert(provider.kind(), provider).is_none(),
                "duplicate reference provider"
            );
        }
        let (sender, receiver) = mpsc::channel();
        Ok(Self {
            providers: registry,
            executor,
            sender,
            receiver,
            generation: GenerationId(0),
            cancellation: None,
            active_kind: None,
            active_range: None,
            candidates: Vec::new(),
            selected: 0,
            next_reference_id: 0,
        })
    }

    pub fn generation(&self) -> GenerationId {
        self.generation
    }

    pub fn activate(
        &mut self,
        activation: CompletionActivation,
        limit: usize,
    ) -> Result<GenerationId> {
        self.start_query_with_leader(
            activation.kind,
            activation.query,
            activation.scope,
            activation.replacement_range,
            limit,
            activation.typed_leader,
        )
    }

    pub fn start_query(
        &mut self,
        kind: ReferenceKind,
        query: String,
        scope: super::model::QueryScope,
        replacement_range: TextRange,
        limit: usize,
    ) -> Result<GenerationId> {
        self.start_query_with_leader(kind, query, scope, replacement_range, limit, String::new())
    }

    fn start_query_with_leader(
        &mut self,
        kind: ReferenceKind,
        query: String,
        scope: super::model::QueryScope,
        replacement_range: TextRange,
        limit: usize,
        typed_leader: String,
    ) -> Result<GenerationId> {
        let provider = Arc::clone(
            self.providers
                .get(&kind)
                .with_context(|| format!("no provider registered for {kind:?}"))?,
        );
        self.cancel_current();
        self.advance_generation();
        self.candidates.clear();
        self.selected = 0;
        self.active_kind = Some(kind);
        self.active_range = Some(replacement_range);

        let request = QueryRequest {
            generation: self.generation,
            query,
            scope,
            limit,
            typed_leader,
        };
        let generation = self.generation;
        let cancellation = CancellationFlag::default();
        self.cancellation = Some(cancellation.clone());
        let sender = self.sender.clone();
        self.executor.spawn(Box::new(move || {
            let mut emit = |emission: QueryEmission| {
                sender
                    .send(QueryMessage::Results {
                        generation: emission.generation,
                        candidates: emission.candidates,
                        completed: emission.completed,
                    })
                    .map_err(|_| anyhow::anyhow!("reference query receiver closed"))
            };
            if let Err(error) = provider.query_progressive(request, &cancellation, &mut emit) {
                let _ = sender.send(QueryMessage::Failed {
                    generation,
                    message: error.to_string(),
                });
            }
        }));
        Ok(generation)
    }

    pub fn close(&mut self) {
        self.cancel_current();
        self.advance_generation();
        self.active_kind = None;
        self.active_range = None;
        self.candidates.clear();
        self.selected = 0;
    }

    pub fn drain(&mut self) -> SessionUpdate {
        let mut update = SessionUpdate::default();
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                QueryMessage::Results {
                    generation,
                    candidates,
                    completed,
                } if generation == self.generation && self.active_kind.is_some() => {
                    let active_kind = self.active_kind.expect("guarded above");
                    if candidates.iter().all(|candidate| {
                        candidate.generation == generation
                            && candidate.kind == active_kind
                            && candidate.id.provider == active_kind
                    }) {
                        self.candidates = candidates;
                        self.selected = self.selected.min(self.candidates.len().saturating_sub(1));
                        update.candidates_changed = true;
                        update.completed |= completed;
                    } else {
                        update.error = Some("provider returned inconsistent candidates".into());
                        update.completed = true;
                    }
                }
                QueryMessage::Failed {
                    generation,
                    message,
                } if generation == self.generation && self.active_kind.is_some() => {
                    update.error = Some(message);
                    update.completed = true;
                }
                _ => {}
            }
        }
        update
    }

    pub fn candidates(&self) -> &[ReferenceCandidate] {
        &self.candidates
    }

    pub fn selected(&self) -> Option<&ReferenceCandidate> {
        self.candidates.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if !self.candidates.is_empty() {
            self.selected = (self.selected + 1).min(self.candidates.len() - 1);
        }
    }

    pub fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn accept_selected(&mut self) -> Result<AcceptedReference> {
        let candidate = self.selected().context("no reference candidate selected")?;
        anyhow::ensure!(
            candidate.generation == self.generation,
            "candidate belongs to a stale query"
        );
        anyhow::ensure!(
            Some(candidate.kind) == self.active_kind,
            "candidate kind does not match the active provider"
        );
        anyhow::ensure!(
            candidate.id.provider == candidate.kind,
            "candidate identifier belongs to another provider"
        );
        let provider = self.provider(candidate.id.provider)?;
        let target = provider.resolve(&candidate.id)?;
        let range = self.active_range.context("no active completion range")?;
        let friendly_text = candidate.friendly_text.clone();
        let resolved_range =
            TextRange::new(range.start, range.start + friendly_text.chars().count())?;
        self.next_reference_id += 1;
        let reference = ResolvedReference {
            id: ReferenceId(self.next_reference_id),
            range: resolved_range,
            friendly_text: friendly_text.clone(),
            target,
        };
        self.close();
        Ok(AcceptedReference {
            replacement_range: range,
            replacement_text: friendly_text,
            reference,
        })
    }

    pub fn validate_and_lower(&self, reference: &ResolvedReference) -> Result<LoweredReference> {
        let provider = self.provider(target_kind(&reference.target))?;
        let refreshed_target = provider.validate(&reference.target)?;
        let replacement = provider.lower(&refreshed_target)?;
        Ok(LoweredReference {
            replacement,
            refreshed_target,
        })
    }

    fn provider(&self, kind: ReferenceKind) -> Result<&SharedProvider> {
        self.providers
            .get(&kind)
            .with_context(|| format!("no provider registered for {kind:?}"))
    }

    fn cancel_current(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.cancel();
        }
    }

    fn advance_generation(&mut self) {
        self.generation.0 = self.generation.0.wrapping_add(1);
    }
}

fn target_kind(target: &super::model::ReferenceTarget) -> ReferenceKind {
    match target {
        super::model::ReferenceTarget::File(file) => match file.origin {
            super::model::FileOrigin::GitAware => ReferenceKind::GitFile,
            super::model::FileOrigin::Broad => ReferenceKind::BroadFile,
        },
        super::model::ReferenceTarget::Symbol(_) => ReferenceKind::Symbol,
        super::model::ReferenceTarget::Skill(_) => ReferenceKind::Skill,
        super::model::ReferenceTarget::ExternalUrl(url) => url.kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::model::{
        CandidateDisplay, CandidateId, ContextCost, Preview, QueryScope, ReferenceTarget,
        ValidatedTarget,
    };
    use crate::references::{CancellationFlag, ThreadExecutor};
    use anyhow::bail;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::ThreadId;
    use std::time::Duration;

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
        fn run(&self, index: usize) {
            let job = self.jobs.lock().unwrap().remove(index);
            job();
        }
    }

    struct FakeProvider {
        saw_cancellation: Arc<AtomicBool>,
        thread_sender: Option<Sender<ThreadId>>,
    }

    impl FakeProvider {
        fn candidate(request: &QueryRequest) -> ReferenceCandidate {
            ReferenceCandidate {
                id: CandidateId {
                    provider: ReferenceKind::GitFile,
                    opaque: request.query.clone(),
                },
                generation: request.generation,
                kind: ReferenceKind::GitFile,
                friendly_text: format!("@{}", request.query),
                display: CandidateDisplay {
                    primary: request.query.clone(),
                    ..CandidateDisplay::default()
                },
                context_cost: ContextCost::Pending,
                file_context_cost: None,
                source_version: None,
            }
        }
    }

    impl ReferenceProvider for FakeProvider {
        fn kind(&self) -> ReferenceKind {
            ReferenceKind::GitFile
        }

        fn query(
            &self,
            request: QueryRequest,
            cancellation: &CancellationFlag,
        ) -> Result<Vec<ReferenceCandidate>> {
            if let Some(sender) = &self.thread_sender {
                sender.send(std::thread::current().id()).unwrap();
            }
            if cancellation.is_cancelled() {
                self.saw_cancellation.store(true, Ordering::Release);
                return Ok(Vec::new());
            }
            Ok(vec![Self::candidate(&request)])
        }

        fn resolve(&self, _id: &CandidateId) -> Result<ReferenceTarget> {
            bail!("not needed by this test")
        }

        fn validate(&self, _target: &ReferenceTarget) -> Result<ValidatedTarget> {
            bail!("not needed by this test")
        }

        fn lower(&self, _target: &ValidatedTarget) -> Result<String> {
            bail!("not needed by this test")
        }

        fn preview(&self, _target: &ReferenceTarget) -> Result<Option<Preview>> {
            Ok(None)
        }

        fn context_cost(&self, _target: &ReferenceTarget) -> Result<ContextCost> {
            Ok(ContextCost::None)
        }
    }

    fn provider(cancelled: Arc<AtomicBool>) -> Arc<dyn ReferenceProvider> {
        Arc::new(FakeProvider {
            saw_cancellation: cancelled,
            thread_sender: None,
        })
    }

    fn start(session: &mut ReferenceSession, query: &str) -> GenerationId {
        session
            .start_query(
                ReferenceKind::GitFile,
                query.into(),
                QueryScope::Repository,
                TextRange::new(0, query.chars().count() + 1).unwrap(),
                100,
            )
            .unwrap()
    }

    #[test]
    fn stale_and_out_of_order_generations_are_discarded() {
        let executor = Arc::new(ManualExecutor::default());
        let mut session = ReferenceSession::new(
            [provider(Arc::new(AtomicBool::new(false)))],
            executor.clone(),
        )
        .unwrap();

        let old = start(&mut session, "old");
        let new = start(&mut session, "new");
        assert!(new > old);

        executor.run(1);
        assert!(session.drain().candidates_changed);
        assert_eq!(session.candidates()[0].display.primary, "new");

        executor.run(0);
        assert!(!session.drain().candidates_changed);
        assert_eq!(session.candidates()[0].display.primary, "new");
    }

    #[test]
    fn close_cancels_work_and_invalidates_its_generation() {
        let saw_cancellation = Arc::new(AtomicBool::new(false));
        let executor = Arc::new(ManualExecutor::default());
        let mut session =
            ReferenceSession::new([provider(saw_cancellation.clone())], executor.clone()).unwrap();
        let active = start(&mut session, "old");
        session.close();
        assert!(session.generation() > active);

        executor.run(0);
        assert!(saw_cancellation.load(Ordering::Acquire));
        assert!(!session.drain().candidates_changed);
        assert!(session.candidates().is_empty());
    }

    #[test]
    fn provider_query_runs_on_the_background_executor_thread() {
        let main_thread = std::thread::current().id();
        let (sender, receiver) = mpsc::channel();
        let provider: Arc<dyn ReferenceProvider> = Arc::new(FakeProvider {
            saw_cancellation: Arc::new(AtomicBool::new(false)),
            thread_sender: Some(sender),
        });
        let mut session = ReferenceSession::new([provider], Arc::new(ThreadExecutor)).unwrap();
        start(&mut session, "threaded");

        let provider_thread = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_ne!(provider_thread, main_thread);
    }
}
