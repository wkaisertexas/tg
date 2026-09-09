use super::*;
use crate::references::model::{CompletionActivation, QueryEmission, QueryRequest, QueryScope};

impl ReferenceSession {
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
        scope: QueryScope,
        replacement_range: TextRange,
        limit: usize,
    ) -> Result<GenerationId> {
        self.start_query_with_leader(kind, query, scope, replacement_range, limit, String::new())
    }

    fn start_query_with_leader(
        &mut self,
        kind: ReferenceKind,
        query: String,
        scope: QueryScope,
        replacement_range: TextRange,
        limit: usize,
        typed_leader: String,
    ) -> Result<GenerationId> {
        let provider = Arc::clone(self.provider(kind)?);
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

    pub fn candidates(&self) -> &[ReferenceCandidate] {
        &self.candidates
    }
    pub fn completion_active(&self) -> bool {
        self.active_kind.is_some()
    }
    pub fn active_kind(&self) -> Option<ReferenceKind> {
        self.active_kind
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

    fn cancel_current(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.cancel();
        }
    }

    fn advance_generation(&mut self) {
        self.generation.0 = self.generation.0.wrapping_add(1);
    }
}
