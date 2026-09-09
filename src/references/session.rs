mod costs;
mod operations;
mod query;
#[cfg(test)]
mod tests;

use super::activation::LoweredSnapshot;
use super::model::{
    AcceptedReference, CandidateId, ContextCost, GenerationId, Preview, QueryProgress,
    ReferenceCandidate, ReferenceId, ReferenceKind, ReferenceTarget, ResolvedReference,
    SessionUpdate, SharedProvider, TextRange,
};
use super::{BackgroundExecutor, CancellationFlag, ReferenceProvider};
use crate::tokens::{self, ContextTotal, ReferenceContext, TokenRequestId, TokenService};
use anyhow::{Context, Result};
use costs::{PendingCandidateCost, PendingReferenceCost};
use operations::{PendingAccept, PendingLower, PendingPreview};
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
        result: Result<Box<ReferenceTarget>, String>,
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
                    let accepted = result.and_then(|target| {
                        self.allocate_reference_id()
                            .map(|id| AcceptedReference {
                                replacement_range,
                                replacement_text: replacement_text.clone(),
                                reference: ResolvedReference {
                                    id,
                                    range: resolved_range,
                                    friendly_text: replacement_text,
                                    target: *target,
                                },
                            })
                            .map_err(|error| error.to_string())
                    });
                    self.ready_events.push_back(match accepted {
                        Ok(accepted) => ReferenceEvent::Accepted {
                            request_id,
                            revision,
                            accepted: Box::new(accepted),
                        },
                        Err(message) => ReferenceEvent::OperationFailed {
                            request_id,
                            kind: OperationKind::Accept,
                            revision: Some(revision),
                            reference_id: None,
                            message,
                        },
                    });
                }
                WorkerMessage::PreviewFinished {
                    request_id,
                    generation,
                    candidate_id,
                    result,
                } if self.preview_is_current(request_id, generation, &candidate_id) => {
                    self.pending_preview = None;
                    self.ready_events.push_back(match result {
                        Ok(preview) => ReferenceEvent::PreviewReady {
                            request_id,
                            candidate_id,
                            preview,
                        },
                        Err(message) => ReferenceEvent::OperationFailed {
                            request_id,
                            kind: OperationKind::Preview,
                            revision: None,
                            reference_id: None,
                            message,
                        },
                    });
                }
                WorkerMessage::LowerFinished {
                    request_id,
                    revision,
                    purpose,
                    result,
                } if self.lower_is_current(request_id, revision, purpose) => {
                    self.pending_lower = None;
                    self.ready_events.push_back(match result {
                        Ok(snapshot) => ReferenceEvent::SnapshotLowered {
                            request_id,
                            revision,
                            purpose,
                            snapshot,
                        },
                        Err(failure) => ReferenceEvent::OperationFailed {
                            request_id,
                            kind: OperationKind::Lower,
                            revision: Some(revision),
                            reference_id: failure.reference_id,
                            message: failure.message,
                        },
                    });
                }
                _ => {}
            }
        }
        self.drain_token_updates();
        update
    }

    fn provider(&self, kind: ReferenceKind) -> Result<&SharedProvider> {
        self.providers
            .get(&kind)
            .with_context(|| format!("no provider registered for {kind:?}"))
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
}
