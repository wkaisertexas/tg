use super::*;
use crate::config::LeadersConfig;
use crate::references::activation::lower_snapshot;
use crate::references::model::LoweredReference;

pub(super) struct PendingAccept {
    request_id: OperationRequestId,
    revision: DocumentRevision,
    cancellation: CancellationFlag,
}

pub(super) struct PendingPreview {
    request_id: OperationRequestId,
    generation: GenerationId,
    candidate_id: CandidateId,
    cancellation: CancellationFlag,
}

pub(super) struct PendingLower {
    request_id: OperationRequestId,
    revision: DocumentRevision,
    purpose: LowerPurpose,
    cancellation: CancellationFlag,
}

impl ReferenceSession {
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
                let kind = reference.target.kind();
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

    pub(super) fn cancel_accept(&mut self) {
        if let Some(pending) = self.pending_accept.take() {
            pending.cancellation.cancel();
        }
    }
    pub(super) fn cancel_preview(&mut self) {
        if let Some(pending) = self.pending_preview.take() {
            pending.cancellation.cancel();
        }
    }
    pub(super) fn cancel_lower(&mut self) {
        if let Some(pending) = self.pending_lower.take() {
            pending.cancellation.cancel();
        }
    }

    pub(super) fn accept_is_current(
        &self,
        request_id: OperationRequestId,
        revision: DocumentRevision,
    ) -> bool {
        revision == self.document_revision
            && self.pending_accept.as_ref().is_some_and(|pending| {
                pending.request_id == request_id && pending.revision == revision
            })
    }

    pub(super) fn preview_is_current(
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

    pub(super) fn lower_is_current(
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
}
