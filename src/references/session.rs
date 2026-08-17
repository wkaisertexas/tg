use super::activation::{LoweredSnapshot, lower_snapshot};
use super::model::{
    AcceptedReference, CandidateId, CandidateTokenSource, CompletionActivation, ContextCost,
    GenerationId, LoweredReference, Preview, QueryEmission, QueryProgress, QueryRequest,
    ReferenceCandidate, ReferenceId, ReferenceKind, ReferenceTarget, ResolvedReference,
    SessionUpdate, SharedProvider, TextRange,
};
use super::{BackgroundExecutor, CancellationFlag, ReferenceProvider};
use crate::config::LeadersConfig;
use crate::tokens::{
    self, ContextFileVersion, ContextIdentity, ContextTotal, ReferenceContext, TokenGeneration,
    TokenRequestId, TokenService, TokenState, TokenSubject,
};
use anyhow::{Context, Result};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OperationRequestId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentRevision(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LowerPurpose {
    Write,
    WriteAndQuit,
    Copy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationKind {
    Accept,
    Preview,
    Lower,
}

#[derive(Debug, Clone)]
pub enum ReferenceEvent {
    Query(SessionUpdate),
    Accepted {
        request_id: OperationRequestId,
        revision: DocumentRevision,
        accepted: Box<AcceptedReference>,
    },
    PreviewReady {
        request_id: OperationRequestId,
        candidate_id: CandidateId,
        preview: Option<Preview>,
    },
    SnapshotLowered {
        request_id: OperationRequestId,
        revision: DocumentRevision,
        purpose: LowerPurpose,
        snapshot: LoweredSnapshot,
    },
    OperationFailed {
        request_id: OperationRequestId,
        kind: OperationKind,
        revision: Option<DocumentRevision>,
        reference_id: Option<ReferenceId>,
        message: String,
    },
    CandidateCostsChanged {
        generation: GenerationId,
    },
    ContextTotalChanged {
        revision: DocumentRevision,
        total: ContextTotal,
    },
}

struct OperationFailure {
    reference_id: Option<ReferenceId>,
    message: String,
}

enum WorkerMessage {
    Results {
        generation: GenerationId,
        candidates: Vec<ReferenceCandidate>,
        completed: bool,
        progress: Option<QueryProgress>,
    },
    Failed {
        generation: GenerationId,
        message: String,
    },
    AcceptFinished {
        request_id: OperationRequestId,
        revision: DocumentRevision,
        replacement_range: TextRange,
        replacement_text: String,
        resolved_range: TextRange,
        result: Result<Box<super::model::ReferenceTarget>, String>,
    },
    PreviewFinished {
        request_id: OperationRequestId,
        generation: GenerationId,
        candidate_id: CandidateId,
        result: Result<Option<Preview>, String>,
    },
    LowerFinished {
        request_id: OperationRequestId,
        revision: DocumentRevision,
        purpose: LowerPurpose,
        result: Result<LoweredSnapshot, OperationFailure>,
    },
}

struct PendingAccept {
    request_id: OperationRequestId,
    revision: DocumentRevision,
    cancellation: CancellationFlag,
}

struct PendingPreview {
    request_id: OperationRequestId,
    generation: GenerationId,
    candidate_id: CandidateId,
    cancellation: CancellationFlag,
}

struct PendingLower {
    request_id: OperationRequestId,
    revision: DocumentRevision,
    purpose: LowerPurpose,
    cancellation: CancellationFlag,
}

#[derive(Clone, Copy)]
enum CandidateCostSlot {
    Context,
    File,
}

struct PendingCandidateCost {
    generation: GenerationId,
    candidate_id: CandidateId,
    slot: CandidateCostSlot,
}

struct PendingReferenceCost {
    revision: DocumentRevision,
    identity: ContextIdentity,
}

pub struct ReferenceSession {
    providers: HashMap<ReferenceKind, SharedProvider>,
    executor: Arc<dyn BackgroundExecutor>,
    sender: Sender<WorkerMessage>,
    receiver: Receiver<WorkerMessage>,
    generation: GenerationId,
    cancellation: Option<CancellationFlag>,
    active_kind: Option<ReferenceKind>,
    active_range: Option<TextRange>,
    candidates: Vec<ReferenceCandidate>,
    selected: usize,
    next_reference_id: u64,
    next_operation_id: u64,
    document_revision: DocumentRevision,
    pending_accept: Option<PendingAccept>,
    pending_preview: Option<PendingPreview>,
    pending_lower: Option<PendingLower>,
    ready_events: VecDeque<ReferenceEvent>,
    token_service: TokenService,
    pending_candidate_costs: HashMap<TokenRequestId, PendingCandidateCost>,
    reference_contexts: Vec<ReferenceContext>,
    pending_reference_costs: HashMap<TokenRequestId, PendingReferenceCost>,
}

impl ReferenceSession {
    pub fn new(
        providers: impl IntoIterator<Item = Arc<dyn ReferenceProvider>>,
        executor: Arc<dyn BackgroundExecutor>,
    ) -> Result<Self> {
        Self::with_tokenizer(providers, executor, tokens::GPT4O_TOKENIZER)
    }

    pub fn with_tokenizer(
        providers: impl IntoIterator<Item = Arc<dyn ReferenceProvider>>,
        executor: Arc<dyn BackgroundExecutor>,
        tokenizer: &str,
    ) -> Result<Self> {
        let mut registry = HashMap::new();
        for provider in providers {
            anyhow::ensure!(
                registry.insert(provider.kind(), provider).is_none(),
                "duplicate reference provider"
            );
        }
        let (sender, receiver) = mpsc::channel();
        let token_service = TokenService::for_tokenizer(Arc::clone(&executor), tokenizer)?;
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
            next_operation_id: 0,
            document_revision: DocumentRevision(0),
            pending_accept: None,
            pending_preview: None,
            pending_lower: None,
            ready_events: VecDeque::new(),
            token_service,
            pending_candidate_costs: HashMap::new(),
            reference_contexts: Vec::new(),
            pending_reference_costs: HashMap::new(),
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
        self.cancel_accept();
        self.cancel_preview();
        self.cancel_current();
        self.pending_candidate_costs.clear();
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
                    .send(WorkerMessage::Results {
                        generation: emission.generation,
                        candidates: emission.candidates,
                        completed: emission.completed,
                        progress: emission.progress,
                    })
                    .map_err(|_| anyhow::anyhow!("reference query receiver closed"))
            };
            if let Err(error) = provider.query_progressive(request, &cancellation, &mut emit) {
                let _ = sender.send(WorkerMessage::Failed {
                    generation,
                    message: error.to_string(),
                });
            }
        }));
        Ok(generation)
    }

    pub fn close(&mut self) {
        self.cancel_preview();
        self.cancel_current();
        self.advance_generation();
        self.active_kind = None;
        self.active_range = None;
        self.candidates.clear();
        self.pending_candidate_costs.clear();
        self.selected = 0;
    }

    pub fn drain(&mut self) -> SessionUpdate {
        self.drain_messages()
    }

    pub fn drain_events(&mut self) -> Vec<ReferenceEvent> {
        let update = self.drain_messages();
        let mut events = self.ready_events.drain(..).collect::<Vec<_>>();
        if update.candidates_changed
            || update.completed
            || update.error.is_some()
            || update.progress.is_some()
        {
            events.push(ReferenceEvent::Query(update));
        }
        events
    }

    fn drain_messages(&mut self) -> SessionUpdate {
        let mut update = SessionUpdate::default();
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                WorkerMessage::Results {
                    generation,
                    candidates,
                    completed,
                    progress,
                } if generation == self.generation && self.active_kind.is_some() => {
                    let active_kind = self.active_kind.expect("guarded above");
                    if candidates.iter().all(|candidate| {
                        candidate.generation == generation
                            && candidate.kind == active_kind
                            && candidate.id.provider == active_kind
                    }) {
                        let mut candidates = candidates;
                        for candidate in &mut candidates {
                            if let Some(previous) = self.candidates.iter().find(|previous| {
                                previous.id == candidate.id
                                    && previous.source_version == candidate.source_version
                            }) {
                                candidate.context_cost = previous.context_cost.clone();
                                candidate.file_context_cost = previous.file_context_cost.clone();
                            }
                        }
                        self.candidates = candidates;
                        self.selected = self.selected.min(self.candidates.len().saturating_sub(1));
                        self.cancel_preview();
                        self.schedule_candidate_costs();
                        update.candidates_changed = true;
                        update.completed |= completed;
                        update.progress = progress.or(update.progress);
                    } else {
                        update.error = Some("provider returned inconsistent candidates".into());
                        update.completed = true;
                    }
                }
                WorkerMessage::Failed {
                    generation,
                    message,
                } if generation == self.generation && self.active_kind.is_some() => {
                    update.error = Some(message);
                    update.completed = true;
                }
                WorkerMessage::AcceptFinished {
                    request_id,
                    revision,
                    replacement_range,
                    replacement_text,
                    resolved_range,
                    result,
                } if self.accept_is_current(request_id, revision) => {
                    self.pending_accept = None;
                    match result {
                        Ok(target) => match self.allocate_reference_id() {
                            Ok(id) => self.ready_events.push_back(ReferenceEvent::Accepted {
                                request_id,
                                revision,
                                accepted: Box::new(AcceptedReference {
                                    replacement_range,
                                    replacement_text: replacement_text.clone(),
                                    reference: ResolvedReference {
                                        id,
                                        range: resolved_range,
                                        friendly_text: replacement_text,
                                        target: *target,
                                    },
                                }),
                            }),
                            Err(error) => {
                                self.ready_events
                                    .push_back(ReferenceEvent::OperationFailed {
                                        request_id,
                                        kind: OperationKind::Accept,
                                        revision: Some(revision),
                                        reference_id: None,
                                        message: error.to_string(),
                                    })
                            }
                        },
                        Err(message) => {
                            self.ready_events
                                .push_back(ReferenceEvent::OperationFailed {
                                    request_id,
                                    kind: OperationKind::Accept,
                                    revision: Some(revision),
                                    reference_id: None,
                                    message,
                                })
                        }
                    }
                }
                WorkerMessage::PreviewFinished {
                    request_id,
                    generation,
                    candidate_id,
                    result,
                } if self.preview_is_current(request_id, generation, &candidate_id) => {
                    self.pending_preview = None;
                    match result {
                        Ok(preview) => self.ready_events.push_back(ReferenceEvent::PreviewReady {
                            request_id,
                            candidate_id,
                            preview,
                        }),
                        Err(message) => {
                            self.ready_events
                                .push_back(ReferenceEvent::OperationFailed {
                                    request_id,
                                    kind: OperationKind::Preview,
                                    revision: None,
                                    reference_id: None,
                                    message,
                                })
                        }
                    }
                }
                WorkerMessage::LowerFinished {
                    request_id,
                    revision,
                    purpose,
                    result,
                } if self.lower_is_current(request_id, revision, purpose) => {
                    self.pending_lower = None;
                    match result {
                        Ok(snapshot) => {
                            self.ready_events
                                .push_back(ReferenceEvent::SnapshotLowered {
                                    request_id,
                                    revision,
                                    purpose,
                                    snapshot,
                                })
                        }
                        Err(failure) => {
                            self.ready_events
                                .push_back(ReferenceEvent::OperationFailed {
                                    request_id,
                                    kind: OperationKind::Lower,
                                    revision: Some(revision),
                                    reference_id: failure.reference_id,
                                    message: failure.message,
                                })
                        }
                    }
                }
                _ => {}
            }
        }
        self.drain_token_updates();
        update
    }

    pub fn candidates(&self) -> &[ReferenceCandidate] {
        &self.candidates
    }

    pub fn selected(&self) -> Option<&ReferenceCandidate> {
        self.candidates.get(self.selected)
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn select_next(&mut self) {
        if !self.candidates.is_empty() {
            let selected = (self.selected + 1).min(self.candidates.len() - 1);
            if selected != self.selected {
                self.cancel_preview();
                self.selected = selected;
            }
        }
    }

    pub fn select_previous(&mut self) {
        let selected = self.selected.saturating_sub(1);
        if selected != self.selected {
            self.cancel_preview();
            self.selected = selected;
        }
    }

    pub fn document_revision(&self) -> DocumentRevision {
        self.document_revision
    }

    pub fn document_changed(&mut self, revision: DocumentRevision) {
        self.document_revision = revision;
        self.pending_reference_costs.clear();
        self.cancel_accept();
        self.cancel_lower();
        self.close();
    }

    pub fn update_references(
        &mut self,
        revision: DocumentRevision,
        references: Arc<[ResolvedReference]>,
    ) -> Result<()> {
        anyhow::ensure!(
            revision == self.document_revision,
            "reference update uses a stale document revision"
        );
        let old = std::mem::take(&mut self.reference_contexts);
        self.pending_reference_costs.clear();
        self.reference_contexts = references
            .iter()
            .filter_map(reference_context)
            .map(|mut context| {
                if let Some(previous) = old
                    .iter()
                    .find(|previous| previous.identity == context.identity)
                {
                    context.state = previous.state.clone();
                }
                context
            })
            .collect();
        self.schedule_reference_costs(revision);
        self.ready_events
            .push_back(ReferenceEvent::ContextTotalChanged {
                revision,
                total: self.context_total(),
            });
        Ok(())
    }

    pub fn context_total(&self) -> ContextTotal {
        tokens::context_total(&self.reference_contexts)
    }

    pub fn cancel_operation(&mut self, request_id: OperationRequestId) {
        if self
            .pending_accept
            .as_ref()
            .is_some_and(|pending| pending.request_id == request_id)
        {
            self.cancel_accept();
        }
        if self
            .pending_preview
            .as_ref()
            .is_some_and(|pending| pending.request_id == request_id)
        {
            self.cancel_preview();
        }
        if self
            .pending_lower
            .as_ref()
            .is_some_and(|pending| pending.request_id == request_id)
        {
            self.cancel_lower();
        }
    }

    pub fn begin_accept_selected(
        &mut self,
        revision: DocumentRevision,
    ) -> Result<OperationRequestId> {
        anyhow::ensure!(
            revision == self.document_revision,
            "accept request uses a stale document revision"
        );
        let candidate = self
            .selected()
            .context("no reference candidate selected")?
            .clone();
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
        let provider = Arc::clone(self.provider(candidate.id.provider)?);
        let range = self.active_range.context("no active completion range")?;
        let resolved_range = TextRange::new(
            range.start,
            range.start + candidate.friendly_text.chars().count(),
        )?;
        let request_id = self.allocate_operation_id()?;
        let cancellation = CancellationFlag::default();
        let sender = self.sender.clone();
        let candidate_id = candidate.id;
        let replacement_text = candidate.friendly_text;
        self.cancel_accept();
        self.close();
        self.pending_accept = Some(PendingAccept {
            request_id,
            revision,
            cancellation: cancellation.clone(),
        });
        self.executor.spawn(Box::new(move || {
            if cancellation.is_cancelled() {
                return;
            }
            let result = provider
                .resolve(&candidate_id)
                .map(Box::new)
                .map_err(|error| error.to_string());
            if cancellation.is_cancelled() {
                return;
            }
            let _ = sender.send(WorkerMessage::AcceptFinished {
                request_id,
                revision,
                replacement_range: range,
                replacement_text,
                resolved_range,
                result,
            });
        }));
        Ok(request_id)
    }

    pub fn begin_preview_selected(&mut self) -> Result<OperationRequestId> {
        let candidate = self
            .selected()
            .context("no reference candidate selected")?
            .clone();
        anyhow::ensure!(
            candidate.generation == self.generation,
            "candidate belongs to a stale query"
        );
        anyhow::ensure!(
            candidate.id.provider == candidate.kind && Some(candidate.kind) == self.active_kind,
            "candidate does not match the active provider"
        );
        let provider = Arc::clone(self.provider(candidate.id.provider)?);
        let request_id = self.allocate_operation_id()?;
        let generation = self.generation;
        let candidate_id = candidate.id;
        let cancellation = CancellationFlag::default();
        let sender = self.sender.clone();
        self.cancel_preview();
        self.pending_preview = Some(PendingPreview {
            request_id,
            generation,
            candidate_id: candidate_id.clone(),
            cancellation: cancellation.clone(),
        });
        self.executor.spawn(Box::new(move || {
            if cancellation.is_cancelled() {
                return;
            }
            let result = provider
                .resolve(&candidate_id)
                .and_then(|target| provider.preview(&target))
                .map_err(|error| error.to_string());
            if cancellation.is_cancelled() {
                return;
            }
            let _ = sender.send(WorkerMessage::PreviewFinished {
                request_id,
                generation,
                candidate_id,
                result,
            });
        }));
        Ok(request_id)
    }

    pub fn begin_lower_snapshot(
        &mut self,
        revision: DocumentRevision,
        purpose: LowerPurpose,
        text: Arc<str>,
        references: Arc<[ResolvedReference]>,
        leaders: LeadersConfig,
    ) -> Result<OperationRequestId> {
        anyhow::ensure!(
            revision == self.document_revision,
            "lower request uses a stale document revision"
        );
        let request_id = self.allocate_operation_id()?;
        let providers = self.providers.clone();
        let cancellation = CancellationFlag::default();
        let sender = self.sender.clone();
        self.cancel_lower();
        self.pending_lower = Some(PendingLower {
            request_id,
            revision,
            purpose,
            cancellation: cancellation.clone(),
        });
        self.executor.spawn(Box::new(move || {
            if cancellation.is_cancelled() {
                return;
            }
            let mut failed_reference_id = None;
            let result = lower_snapshot(&text, &references, &leaders, |reference| {
                if cancellation.is_cancelled() {
                    anyhow::bail!("lower operation cancelled");
                }
                failed_reference_id = Some(reference.id);
                let kind = target_kind(&reference.target);
                let provider = providers
                    .get(&kind)
                    .with_context(|| format!("no provider registered for {kind:?}"))?;
                let refreshed_target = provider.validate(&reference.target)?;
                let replacement = provider.lower(&refreshed_target)?;
                failed_reference_id = None;
                Ok(LoweredReference {
                    replacement,
                    refreshed_target,
                })
            })
            .map_err(|error| OperationFailure {
                reference_id: failed_reference_id,
                message: error.to_string(),
            });
            if cancellation.is_cancelled() {
                return;
            }
            let _ = sender.send(WorkerMessage::LowerFinished {
                request_id,
                revision,
                purpose,
                result,
            });
        }));
        Ok(request_id)
    }

    fn provider(&self, kind: ReferenceKind) -> Result<&SharedProvider> {
        self.providers
            .get(&kind)
            .with_context(|| format!("no provider registered for {kind:?}"))
    }

    fn schedule_candidate_costs(&mut self) {
        let generation = self.generation;
        let revision = self.document_revision;
        let existing = |id: &CandidateId, slot: CandidateCostSlot, pending: &HashMap<_, _>| {
            pending.values().any(|request: &PendingCandidateCost| {
                request.generation == generation
                    && request.candidate_id == *id
                    && std::mem::discriminant(&request.slot) == std::mem::discriminant(&slot)
            })
        };
        let work = self
            .candidates
            .iter()
            .filter_map(|candidate| {
                candidate
                    .token_source
                    .clone()
                    .map(|source| (candidate, source))
            })
            .flat_map(|(candidate, source)| {
                let mut work = Vec::new();
                if candidate.context_cost == ContextCost::Pending
                    && !existing(
                        &candidate.id,
                        CandidateCostSlot::Context,
                        &self.pending_candidate_costs,
                    )
                {
                    work.push((
                        candidate.id.clone(),
                        CandidateCostSlot::Context,
                        source.clone(),
                    ));
                }
                if matches!(source, CandidateTokenSource::Symbol { .. })
                    && candidate.file_context_cost == Some(ContextCost::Pending)
                    && !existing(
                        &candidate.id,
                        CandidateCostSlot::File,
                        &self.pending_candidate_costs,
                    )
                {
                    work.push((candidate.id.clone(), CandidateCostSlot::File, source));
                }
                work
            })
            .collect::<Vec<_>>();
        for (candidate_id, slot, source) in work {
            let subject = TokenSubject(candidate_id.opaque.clone());
            let ticket = match (&source, slot) {
                (CandidateTokenSource::File { path }, CandidateCostSlot::Context)
                | (CandidateTokenSource::Symbol { path, .. }, CandidateCostSlot::File) => {
                    self.token_service.request_file(
                        path.clone(),
                        TokenGeneration(generation.0),
                        tokens::DocumentRevision(revision.0),
                        subject,
                    )
                }
                (
                    CandidateTokenSource::Symbol {
                        path,
                        start_byte,
                        end_byte,
                    },
                    CandidateCostSlot::Context,
                ) => self.token_service.request_file_range(
                    path.clone(),
                    *start_byte..*end_byte,
                    TokenGeneration(generation.0),
                    tokens::DocumentRevision(revision.0),
                    subject,
                ),
                (CandidateTokenSource::File { .. }, CandidateCostSlot::File) => continue,
            };
            self.pending_candidate_costs.insert(
                ticket.id,
                PendingCandidateCost {
                    generation,
                    candidate_id,
                    slot,
                },
            );
        }
    }

    fn schedule_reference_costs(&mut self, revision: DocumentRevision) {
        let identities = self
            .reference_contexts
            .iter()
            .filter(|context| context.state == TokenState::Pending)
            .filter_map(|context| context.identity.clone())
            .collect::<std::collections::HashSet<_>>();
        for identity in identities {
            let ticket = match &identity {
                ContextIdentity::File(path) => self.token_service.request_file(
                    path.clone(),
                    TokenGeneration(0),
                    tokens::DocumentRevision(revision.0),
                    TokenSubject("reference-file".into()),
                ),
                ContextIdentity::Range {
                    canonical_path,
                    start_byte,
                    end_byte,
                    ..
                } => self.token_service.request_file_range(
                    canonical_path.clone(),
                    *start_byte..*end_byte,
                    TokenGeneration(0),
                    tokens::DocumentRevision(revision.0),
                    TokenSubject("reference-range".into()),
                ),
            };
            self.pending_reference_costs
                .insert(ticket.id, PendingReferenceCost { revision, identity });
        }
    }

    fn drain_token_updates(&mut self) {
        let mut candidates_changed = false;
        let mut total_changed = false;
        for update in self.token_service.drain() {
            if let Some(pending) = self.pending_candidate_costs.remove(&update.id)
                && pending.generation == self.generation
                && update.generation == TokenGeneration(self.generation.0)
                && update.revision == tokens::DocumentRevision(self.document_revision.0)
                && let Some(candidate) = self
                    .candidates
                    .iter_mut()
                    .find(|candidate| candidate.id == pending.candidate_id)
            {
                let cost = context_cost(update.state);
                match pending.slot {
                    CandidateCostSlot::Context => candidate.context_cost = cost,
                    CandidateCostSlot::File => candidate.file_context_cost = Some(cost),
                }
                candidates_changed = true;
                continue;
            }
            if let Some(pending) = self.pending_reference_costs.remove(&update.id)
                && pending.revision == self.document_revision
                && update.revision == tokens::DocumentRevision(self.document_revision.0)
            {
                for context in &mut self.reference_contexts {
                    if context.identity.as_ref() == Some(&pending.identity) {
                        context.state = update.state.clone();
                    }
                }
                total_changed = true;
            }
        }
        if candidates_changed {
            self.ready_events
                .push_back(ReferenceEvent::CandidateCostsChanged {
                    generation: self.generation,
                });
        }
        if total_changed {
            self.ready_events
                .push_back(ReferenceEvent::ContextTotalChanged {
                    revision: self.document_revision,
                    total: self.context_total(),
                });
        }
    }

    fn cancel_current(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.cancel();
        }
    }

    fn cancel_accept(&mut self) {
        if let Some(pending) = self.pending_accept.take() {
            pending.cancellation.cancel();
        }
    }

    fn cancel_preview(&mut self) {
        if let Some(pending) = self.pending_preview.take() {
            pending.cancellation.cancel();
        }
    }

    fn cancel_lower(&mut self) {
        if let Some(pending) = self.pending_lower.take() {
            pending.cancellation.cancel();
        }
    }

    fn accept_is_current(
        &self,
        request_id: OperationRequestId,
        revision: DocumentRevision,
    ) -> bool {
        revision == self.document_revision
            && self.pending_accept.as_ref().is_some_and(|pending| {
                pending.request_id == request_id && pending.revision == revision
            })
    }

    fn preview_is_current(
        &self,
        request_id: OperationRequestId,
        generation: GenerationId,
        candidate_id: &CandidateId,
    ) -> bool {
        self.pending_preview.as_ref().is_some_and(|pending| {
            pending.request_id == request_id
                && pending.generation == generation
                && pending.candidate_id == *candidate_id
        }) && self.generation == generation
            && self
                .selected()
                .is_some_and(|candidate| candidate.id == *candidate_id)
    }

    fn lower_is_current(
        &self,
        request_id: OperationRequestId,
        revision: DocumentRevision,
        purpose: LowerPurpose,
    ) -> bool {
        revision == self.document_revision
            && self.pending_lower.as_ref().is_some_and(|pending| {
                pending.request_id == request_id
                    && pending.revision == revision
                    && pending.purpose == purpose
            })
    }

    fn allocate_operation_id(&mut self) -> Result<OperationRequestId> {
        self.next_operation_id = self
            .next_operation_id
            .checked_add(1)
            .context("operation request identifier overflow")?;
        Ok(OperationRequestId(self.next_operation_id))
    }

    fn allocate_reference_id(&mut self) -> Result<ReferenceId> {
        self.next_reference_id = self
            .next_reference_id
            .checked_add(1)
            .context("reference identifier overflow")?;
        Ok(ReferenceId(self.next_reference_id))
    }

    fn advance_generation(&mut self) {
        self.generation.0 = self.generation.0.wrapping_add(1);
    }
}

fn context_cost(state: TokenState) -> ContextCost {
    match state {
        TokenState::Pending => ContextCost::Pending,
        TokenState::Ready(tokens) => ContextCost::Tokens(tokens),
        TokenState::Bytes(bytes) => ContextCost::Bytes(bytes),
        TokenState::Unavailable => ContextCost::Unavailable,
    }
}

fn reference_context(reference: &ResolvedReference) -> Option<ReferenceContext> {
    let identity = match &reference.target {
        ReferenceTarget::File(file) => ContextIdentity::File(file.canonical_path.clone()),
        ReferenceTarget::Symbol(symbol) => {
            let version = symbol.file.source_version.as_ref()?;
            ContextIdentity::Range {
                canonical_path: symbol.file.canonical_path.clone(),
                start_byte: symbol.start_byte,
                end_byte: symbol.end_byte,
                file_version: ContextFileVersion {
                    size: version.size,
                    modified: version.modified,
                    content_sha256: version.content_sha256,
                },
            }
        }
        ReferenceTarget::Skill(_) | ReferenceTarget::ExternalUrl(_) => return None,
    };
    Some(ReferenceContext {
        identity: Some(identity),
        state: TokenState::Pending,
    })
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
        CandidateDisplay, CandidateId, CandidateTokenSource, ContextCost, FileOrigin, FileTarget,
        Preview, PreviewLine, QueryScope, ReferenceTarget, ValidatedTarget,
    };
    use crate::references::{CancellationFlag, ThreadExecutor};
    use anyhow::bail;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::ThreadId;
    use std::time::Duration;

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
        let update = session.drain();
        assert!(update.candidates_changed);
        assert_eq!(
            update.progress,
            Some(QueryProgress {
                scanned: 1,
                total: 2,
                indexed_symbols: 3,
            })
        );
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
            token_path: None,
        });
        let mut session = ReferenceSession::new([provider], Arc::new(ThreadExecutor)).unwrap();
        start(&mut session, "threaded");

        let provider_thread = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_ne!(provider_thread, main_thread);
    }

    #[test]
    fn candidate_file_cost_arrives_as_an_async_candidate_update() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tokens.txt");
        std::fs::write(&path, "hello world").unwrap();
        let executor = Arc::new(ManualExecutor::default());
        let provider: Arc<dyn ReferenceProvider> = Arc::new(FakeProvider {
            saw_cancellation: Arc::new(AtomicBool::new(false)),
            thread_sender: None,
            token_path: Some(path),
        });
        let mut session = ReferenceSession::new([provider], executor.clone()).unwrap();

        start(&mut session, "tokens");
        executor.run_on_background_thread(0);
        session.drain_events();
        assert_eq!(session.candidates()[0].context_cost, ContextCost::Pending);

        executor.run_on_background_thread(0);
        let events = session.drain_events();
        assert!(events.iter().any(|event| matches!(
            event,
            ReferenceEvent::CandidateCostsChanged { generation }
                if *generation == session.generation()
        )));
        assert_eq!(session.candidates()[0].context_cost, ContextCost::Tokens(2));
    }

    #[test]
    fn stale_candidate_cost_cannot_update_a_new_generation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tokens.txt");
        std::fs::write(&path, "hello world").unwrap();
        let executor = Arc::new(ManualExecutor::default());
        let provider: Arc<dyn ReferenceProvider> = Arc::new(FakeProvider {
            saw_cancellation: Arc::new(AtomicBool::new(false)),
            thread_sender: None,
            token_path: Some(path),
        });
        let mut session = ReferenceSession::new([provider], executor.clone()).unwrap();

        start(&mut session, "old");
        executor.run_on_background_thread(0);
        session.drain_events();
        start(&mut session, "new");
        executor.run_on_background_thread(1);
        session.drain_events();

        executor.run_on_background_thread(0);
        assert!(
            session
                .drain_events()
                .iter()
                .all(|event| !matches!(event, ReferenceEvent::CandidateCostsChanged { .. }))
        );
        assert_eq!(session.candidates()[0].display.primary, "new");
        assert_eq!(session.candidates()[0].context_cost, ContextCost::Pending);

        executor.run_on_background_thread(0);
        session.drain_events();
        assert_eq!(session.candidates()[0].context_cost, ContextCost::Tokens(2));
    }

    #[test]
    fn live_context_total_deduplicates_and_whole_file_subsumes_symbol() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("context.rs");
        std::fs::write(&path, "hello world").unwrap();
        let version = crate::references::file::file_version(&path).unwrap();
        let file_target = FileTarget {
            canonical_path: path.clone(),
            relative_path: "context.rs".into(),
            origin: FileOrigin::GitAware,
            source_version: Some(version.clone()),
        };
        let whole = |id| ResolvedReference {
            id: ReferenceId(id),
            range: TextRange::new(0, 1).unwrap(),
            friendly_text: "@context.rs".into(),
            target: ReferenceTarget::File(file_target.clone()),
        };
        let symbol = ResolvedReference {
            id: ReferenceId(3),
            range: TextRange::new(0, 1).unwrap(),
            friendly_text: "@context.rs::hello".into(),
            target: ReferenceTarget::Symbol(crate::references::model::SymbolTarget {
                file: file_target.clone(),
                identity: crate::references::model::SymbolIdentity {
                    language: "rs".into(),
                    qualified_name: "hello".into(),
                    leaf_name: "hello".into(),
                    kind: "function".into(),
                    is_definition: true,
                },
                start_byte: 0,
                end_byte: 5,
                name_start_byte: 0,
                name_end_byte: 5,
                location: crate::references::model::SourceLocation { line: 1, column: 1 },
                markdown_anchor: None,
            }),
        };
        let executor = Arc::new(ManualExecutor::default());
        let mut session = ReferenceSession::new(
            [provider(Arc::new(AtomicBool::new(false)))],
            executor.clone(),
        )
        .unwrap();
        session
            .update_references(DocumentRevision(0), vec![whole(1), whole(2), symbol].into())
            .unwrap();
        assert_eq!(session.context_total().pending, 1);

        while executor.len() > 0 {
            executor.run_on_background_thread(0);
        }
        let events = session.drain_events();
        assert!(events.iter().any(|event| matches!(
            event,
            ReferenceEvent::ContextTotalChanged { total, .. }
                if total.ready_tokens == 2 && total.pending == 0
        )));
        assert_eq!(session.context_total().ready_tokens, 2);
        assert_eq!(session.context_total().pending, 0);
    }

    #[test]
    fn accept_runs_off_caller_and_uses_character_ranges() {
        let main_thread = std::thread::current().id();
        let (mut session, executor, calls) = operation_session(None);

        let request_id = session.begin_accept_selected(DocumentRevision(0)).unwrap();
        assert!(calls.lock().unwrap().is_empty());
        executor.run_on_background_thread(0);

        let events = session.drain_events();
        let [
            ReferenceEvent::Accepted {
                request_id: actual_request_id,
                revision,
                accepted,
            },
        ] = events.as_slice()
        else {
            panic!("expected an accepted event")
        };
        assert_eq!(*actual_request_id, request_id);
        assert_eq!(*revision, DocumentRevision(0));
        assert_eq!(accepted.reference.id, ReferenceId(1));
        assert_eq!(accepted.reference.range, TextRange::new(3, 9).unwrap());
        assert_eq!(accepted.replacement_text, "@naïve");
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|(_, thread)| *thread != main_thread)
        );
    }

    #[test]
    fn changed_document_and_changed_selection_reject_stale_results() {
        let (mut session, executor, _) = operation_session(None);
        session.begin_accept_selected(DocumentRevision(0)).unwrap();
        session.document_changed(DocumentRevision(1));
        executor.run_on_background_thread(0);
        assert!(session.drain_events().is_empty());

        // Re-open a completion at the new document revision and prove a
        // preview for the previous selected candidate cannot arrive later.
        session.generation = GenerationId(8);
        session.active_kind = Some(ReferenceKind::GitFile);
        session.active_range = Some(TextRange::new(0, 1).unwrap());
        session.candidates = vec!["first", "second"]
            .into_iter()
            .map(|name| ReferenceCandidate {
                id: CandidateId {
                    provider: ReferenceKind::GitFile,
                    opaque: name.into(),
                },
                generation: GenerationId(8),
                kind: ReferenceKind::GitFile,
                friendly_text: format!("@{name}"),
                display: CandidateDisplay::default(),
                context_cost: ContextCost::None,
                file_context_cost: None,
                source_version: None,
                token_source: None,
            })
            .collect();
        session.begin_preview_selected().unwrap();
        session.select_next();
        executor.run_on_background_thread(0);
        assert!(session.drain_events().is_empty());
    }

    #[test]
    fn preview_resolve_and_render_run_off_caller() {
        let main_thread = std::thread::current().id();
        let (mut session, executor, calls) = operation_session(None);
        let request_id = session.begin_preview_selected().unwrap();
        assert!(calls.lock().unwrap().is_empty());

        executor.run_on_background_thread(0);
        let events = session.drain_events();
        let [
            ReferenceEvent::PreviewReady {
                request_id: actual_request_id,
                candidate_id,
                preview: Some(preview),
            },
        ] = events.as_slice()
        else {
            panic!("expected a preview event")
        };
        assert_eq!(*actual_request_id, request_id);
        assert_eq!(candidate_id.opaque, "naïve");
        assert_eq!(preview.title.as_deref(), Some("naïve"));
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|(operation, _)| *operation)
                .collect::<Vec<_>>(),
            ["resolve", "preview"]
        );
        assert!(calls.iter().all(|(_, thread)| *thread != main_thread));
    }

    #[test]
    fn lowering_is_atomic_and_reports_the_failing_reference() {
        let main_thread = std::thread::current().id();
        let (mut session, executor, calls) = operation_session(Some("stale"));
        let references: Arc<[ResolvedReference]> = vec![
            resolved(4, TextRange::new(0, 4).unwrap(), "@one", "one"),
            resolved(9, TextRange::new(5, 11).unwrap(), "@stale", "stale"),
        ]
        .into();

        let request_id = session
            .begin_lower_snapshot(
                DocumentRevision(0),
                LowerPurpose::Write,
                Arc::from("@one @stale"),
                references,
                LeadersConfig::default(),
            )
            .unwrap();
        executor.run_on_background_thread(0);

        let events = session.drain_events();
        let [
            ReferenceEvent::OperationFailed {
                request_id: actual_request_id,
                kind,
                reference_id,
                ..
            },
        ] = events.as_slice()
        else {
            panic!("expected an atomic lower failure")
        };
        assert_eq!(*actual_request_id, request_id);
        assert_eq!(*kind, OperationKind::Lower);
        assert_eq!(*reference_id, Some(ReferenceId(9)));
        let calls = calls.lock().unwrap();
        assert_eq!(calls.iter().filter(|(name, _)| *name == "lower").count(), 1);
        assert!(calls.iter().all(|(_, thread)| *thread != main_thread));
    }

    #[test]
    fn newer_lower_purpose_rejects_out_of_order_completion() {
        let (mut session, executor, _) = operation_session(None);
        let references: Arc<[ResolvedReference]> =
            vec![resolved(1, TextRange::new(0, 4).unwrap(), "@one", "one")].into();
        let first = session
            .begin_lower_snapshot(
                DocumentRevision(0),
                LowerPurpose::Copy,
                Arc::from("@one"),
                references.clone(),
                LeadersConfig::default(),
            )
            .unwrap();
        let second = session
            .begin_lower_snapshot(
                DocumentRevision(0),
                LowerPurpose::WriteAndQuit,
                Arc::from("@one"),
                references,
                LeadersConfig::default(),
            )
            .unwrap();
        assert!(second > first);

        executor.run_on_background_thread(1);
        let events = session.drain_events();
        let [
            ReferenceEvent::SnapshotLowered {
                request_id,
                purpose,
                snapshot,
                ..
            },
        ] = events.as_slice()
        else {
            panic!("expected the newer lower result")
        };
        assert_eq!(*request_id, second);
        assert_eq!(*purpose, LowerPurpose::WriteAndQuit);
        assert_eq!(snapshot.text, "lowered:one");

        executor.run_on_background_thread(0);
        assert!(session.drain_events().is_empty());
    }
}
