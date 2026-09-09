use super::*;
use crate::references::model::CandidateTokenSource;
use crate::tokens::{
    ContextFileVersion, ContextIdentity, ContextTotal, ReferenceContext, TokenGeneration,
    TokenState, TokenSubject,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum CandidateCostSlot {
    Context,
    File,
}

pub(super) struct PendingCandidateCost {
    generation: GenerationId,
    candidate_id: CandidateId,
    slot: CandidateCostSlot,
}

pub(super) struct PendingReferenceCost {
    revision: DocumentRevision,
    identity: ContextIdentity,
}

impl ReferenceSession {
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

    pub fn reference_cost(&self, reference: &ResolvedReference) -> Option<ContextCost> {
        let identity = reference_context(reference)?.identity?;
        self.reference_contexts
            .iter()
            .find(|context| context.identity.as_ref() == Some(&identity))
            .map(|context| context_cost(context.state.clone()))
    }

    pub(super) fn schedule_candidate_costs(&mut self) {
        let generation = self.generation;
        let revision = self.document_revision;
        let existing = |id: &CandidateId, slot: CandidateCostSlot, pending: &HashMap<_, _>| {
            pending.values().any(|request: &PendingCandidateCost| {
                request.generation == generation
                    && request.candidate_id == *id
                    && request.slot == slot
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

    pub(super) fn drain_token_updates(&mut self) {
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
