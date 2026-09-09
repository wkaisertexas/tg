use super::*;
use crate::config::LeadersConfig;
use crate::references::ThreadExecutor;
use crate::references::model::{
    CandidateDisplay, CandidateTokenSource, FileOrigin, FileTarget, PreviewLine, QueryEmission,
    QueryRequest, QueryScope, ValidatedTarget,
};
use anyhow::bail;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::ThreadId;
use std::time::Duration;

mod costs;
mod operations;
mod query;

type RecordedCalls = Arc<Mutex<Vec<(&'static str, ThreadId)>>>;

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
    fn len(&self) -> usize {
        self.jobs.lock().unwrap().len()
    }
    fn run(&self, index: usize) {
        let job = self.jobs.lock().unwrap().remove(index);
        job();
    }
    fn run_on_background_thread(&self, index: usize) {
        let job = self.jobs.lock().unwrap().remove(index);
        std::thread::spawn(job).join().unwrap();
    }
}

struct RecordingProvider {
    calls: RecordedCalls,
    fail_validation_for: Option<String>,
}
impl RecordingProvider {
    fn record(&self, operation: &'static str) {
        self.calls
            .lock()
            .unwrap()
            .push((operation, std::thread::current().id()));
    }
    fn target(name: &str) -> ReferenceTarget {
        ReferenceTarget::File(FileTarget {
            canonical_path: format!("/repo/{name}").into(),
            relative_path: name.into(),
            origin: FileOrigin::GitAware,
            source_version: None,
        })
    }
}
impl ReferenceProvider for RecordingProvider {
    fn kind(&self) -> ReferenceKind {
        ReferenceKind::GitFile
    }
    fn query(
        &self,
        _request: QueryRequest,
        _cancellation: &CancellationFlag,
    ) -> Result<Vec<ReferenceCandidate>> {
        unreachable!("operation tests seed candidates directly")
    }
    fn resolve(&self, id: &CandidateId) -> Result<ReferenceTarget> {
        self.record("resolve");
        Ok(Self::target(&id.opaque))
    }
    fn validate(&self, target: &ReferenceTarget) -> Result<ValidatedTarget> {
        self.record("validate");
        if let ReferenceTarget::File(file) = target
            && self.fail_validation_for.as_deref() == Some(&file.relative_path)
        {
            bail!("target became stale");
        }
        Ok(ValidatedTarget {
            target: target.clone(),
            context_cost: ContextCost::None,
        })
    }
    fn lower(&self, target: &ValidatedTarget) -> Result<String> {
        self.record("lower");
        let ReferenceTarget::File(file) = &target.target else {
            unreachable!()
        };
        Ok(format!("lowered:{}", file.relative_path))
    }
    fn preview(&self, target: &ReferenceTarget) -> Result<Option<Preview>> {
        self.record("preview");
        let ReferenceTarget::File(file) = target else {
            unreachable!()
        };
        Ok(Some(Preview {
            title: Some(file.relative_path.clone()),
            lines: vec![PreviewLine {
                number: Some(1),
                text: "preview".into(),
            }],
            highlighted_lines: None,
        }))
    }
    fn context_cost(&self, _target: &ReferenceTarget) -> Result<ContextCost> {
        Ok(ContextCost::None)
    }
}

struct FakeProvider {
    saw_cancellation: Arc<AtomicBool>,
    thread_sender: Option<Sender<ThreadId>>,
    token_path: Option<std::path::PathBuf>,
}
impl FakeProvider {
    fn candidate(&self, request: &QueryRequest) -> ReferenceCandidate {
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
            token_source: self
                .token_path
                .clone()
                .map(|path| CandidateTokenSource::File { path }),
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
        Ok(vec![self.candidate(&request)])
    }
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
            progress: Some(QueryProgress {
                scanned: 1,
                total: 2,
                indexed_symbols: 3,
            }),
        })
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
        token_path: None,
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

fn operation_session(
    fail_validation_for: Option<&str>,
) -> (ReferenceSession, Arc<ManualExecutor>, RecordedCalls) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn ReferenceProvider> = Arc::new(RecordingProvider {
        calls: calls.clone(),
        fail_validation_for: fail_validation_for.map(str::to_owned),
    });
    let executor = Arc::new(ManualExecutor::default());
    let mut session = ReferenceSession::new([provider], executor.clone()).unwrap();
    session.generation = GenerationId(7);
    session.active_kind = Some(ReferenceKind::GitFile);
    session.active_range = Some(TextRange::new(3, 6).unwrap());
    session.candidates = vec!["naïve", "second"]
        .into_iter()
        .map(|name| ReferenceCandidate {
            id: CandidateId {
                provider: ReferenceKind::GitFile,
                opaque: name.into(),
            },
            generation: GenerationId(7),
            kind: ReferenceKind::GitFile,
            friendly_text: format!("@{name}"),
            display: CandidateDisplay {
                primary: name.into(),
                ..CandidateDisplay::default()
            },
            context_cost: ContextCost::None,
            file_context_cost: None,
            source_version: None,
            token_source: None,
        })
        .collect();
    (session, executor, calls)
}

fn resolved(id: u64, range: TextRange, friendly: &str, target: &str) -> ResolvedReference {
    ResolvedReference {
        id: ReferenceId(id),
        range,
        friendly_text: friendly.into(),
        target: RecordingProvider::target(target),
    }
}
