use super::*;
use crate::editor::AdapterMode;
use crate::editor::command::{CommandEffect, LowerRequest};
use crate::references::activation::detect_activation;
use crate::references::model::ReferenceTarget;
use crate::references::session::{LowerPurpose, OperationKind, ReferenceEvent};

impl App {
    pub(super) fn sync_reference_state(&mut self) -> Result<()> {
        let revision = self.revision();
        self.reference_session.document_changed(revision);
        self.reference_session
            .update_references(revision, self.document.references().to_vec().into())?;
        self.editor.set_reference_ranges(
            self.document
                .references()
                .iter()
                .map(|reference| reference.range),
        )?;
        self.preview = None;
        Ok(())
    }

    pub(super) fn refresh_activation(&mut self) -> Result<()> {
        if self.editor.mode() != AdapterMode::Insert {
            self.reference_session.close();
            return Ok(());
        }
        let cursor = self.editor.cursor_char_offset();
        match detect_activation(
            self.document.text(),
            cursor,
            &self.leaders,
            &self.repo.search_root,
        )? {
            Some(activation) => {
                let kind = activation.kind;
                if let Err(error) = self
                    .reference_session
                    .activate(activation, self.search_limit)
                {
                    self.providers.record_failure(kind, &error.to_string());
                    self.status = "Provider unavailable · Space p for setup".into();
                } else {
                    self.status = "searching…".into();
                }
            }
            None => self.reference_session.close(),
        }
        Ok(())
    }

    pub(super) fn request_preview(&mut self) {
        self.preview = None;
        self.preview_scroll = 0;
        if self.preview_mode != PreviewMode::Disabled
            && self.preview_visible
            && let Err(error) = self.reference_session.begin_preview_selected()
        {
            self.status = format!("Preview unavailable: {error}");
        }
    }

    pub(super) fn schedule_preview(&mut self) {
        if self.preview_mode == PreviewMode::Automatic {
            self.preview = None;
            self.preview_due = Some(Instant::now() + self.search_debounce);
        } else {
            self.request_preview();
        }
    }

    pub(super) fn accept_selected(&mut self) {
        match self
            .reference_session
            .begin_accept_selected(self.revision())
        {
            Ok(_) => self.status = "Resolving reference…".into(),
            Err(error) => self.status = format!("Cannot accept reference: {error}"),
        }
    }

    fn begin_lower(&mut self, request: LowerRequest) {
        let snapshot = request.snapshot;
        match self.reference_session.begin_lower_snapshot(
            DocumentRevision(snapshot.revision()),
            request.purpose,
            Arc::from(snapshot.text()),
            snapshot.references().to_vec().into(),
            self.leaders.clone(),
        ) {
            Ok(_) => self.status = "Validating references…".into(),
            Err(error) => self.status = format!("Cannot lower document: {error}"),
        }
    }

    pub(super) fn dispatch_command(&mut self, command: String) {
        let line = if command.starts_with(':') {
            command
        } else {
            format!(":{command}")
        };
        match self.commands.dispatch(&line, &self.document) {
            Ok(CommandEffect::Quit) => self.should_exit = true,
            Ok(CommandEffect::Lower(request)) => self.begin_lower(request),
            Ok(CommandEffect::ReadShell(command)) => self.begin_shell_read(command),
            Ok(CommandEffect::Providers) => self.open_providers(),
            Err(error) => self.status = error.to_string(),
        }
    }

    fn finish_lower(
        &mut self,
        revision: DocumentRevision,
        purpose: LowerPurpose,
        snapshot: crate::references::activation::LoweredSnapshot,
    ) {
        if revision != self.revision() {
            return;
        }
        if let Err(error) = self.document.refresh_reference_targets(snapshot.references) {
            self.status = format!("Cannot refresh references: {error}");
            return;
        }
        match purpose {
            LowerPurpose::Copy => {
                self.pending_clipboard = Some(snapshot.text);
                self.status = "Copied lowered document".into();
            }
            LowerPurpose::Write | LowerPurpose::WriteAndQuit => {
                let Some(target) = self.save_target.as_ref() else {
                    self.status = "No file name".into();
                    return;
                };
                match self.saver.save(target, snapshot.text.as_bytes()) {
                    Ok(target) => {
                        self.save_target = Some(target);
                        self.document.mark_saved();
                        self.status = "Written".into();
                        if purpose == LowerPurpose::WriteAndQuit {
                            self.should_exit = true;
                        }
                    }
                    Err(error) => self.status = format!("Write failed: {error}"),
                }
            }
        }
    }

    pub(super) fn drain_reference_events(&mut self) {
        for event in self.reference_session.drain_events() {
            match event {
                ReferenceEvent::Query(update) => {
                    let kind = self.reference_session.active_kind();
                    if update.completed
                        && update.error.is_none()
                        && let Some(kind) = kind
                    {
                        self.providers.record_query_success(kind);
                    }
                    if let Some(error) = update.error {
                        if let Some(kind) = kind {
                            self.providers.record_failure(kind, &error);
                        }
                        self.status = self
                            .providers
                            .reports
                            .iter()
                            .find(|report| Some(report.kind) == kind)
                            .map_or(error, |report| {
                                format!("{}: {} · :providers", report.name, report.state.label())
                            });
                    } else if update.candidates_changed {
                        self.status =
                            format!("{} matches", self.reference_session.candidates().len());
                        if self.preview_mode == PreviewMode::Automatic {
                            self.preview_visible = true;
                            self.schedule_preview();
                        }
                    } else if let Some(progress) = update.progress {
                        self.status = format!("{} / {} files", progress.scanned, progress.total);
                    }
                }
                ReferenceEvent::Accepted {
                    revision, accepted, ..
                } if revision == self.revision() => {
                    let cursor = accepted.replacement_range.start
                        + accepted.replacement_text.chars().count();
                    match self
                        .document
                        .accept_reference(*accepted)
                        .and_then(|_| self.replace_widget_from_document())
                        .and_then(|_| self.editor.set_cursor_char_offset(cursor))
                        .and_then(|_| self.sync_reference_state())
                    {
                        Ok(()) => self.status = "Reference resolved".into(),
                        Err(error) => self.status = format!("Cannot accept reference: {error}"),
                    }
                }
                ReferenceEvent::PreviewReady {
                    candidate_id,
                    preview,
                    ..
                } => {
                    self.preview_scroll = preview
                        .as_ref()
                        .and_then(|preview| preview.highlighted_lines.as_ref())
                        .map_or(0, |lines| lines.start().saturating_sub(6));
                    self.preview = Some(CachedPreview {
                        candidate_id,
                        preview,
                    });
                }
                ReferenceEvent::SnapshotLowered {
                    revision,
                    purpose,
                    snapshot,
                    ..
                } => {
                    self.finish_lower(revision, purpose, snapshot);
                }
                ReferenceEvent::OperationFailed {
                    kind,
                    reference_id,
                    message,
                    ..
                } => {
                    let reference = reference_id.and_then(|id| {
                        self.document
                            .references()
                            .iter()
                            .find(|reference| reference.id == id)
                    });
                    if let Some(reference) = reference {
                        let _ = self.editor.set_cursor_char_offset(reference.range.start);
                    }
                    let provider = reference
                        .and_then(|reference| match &reference.target {
                            ReferenceTarget::ExternalUrl(target) => Some(target.kind),
                            _ => None,
                        })
                        .or_else(|| {
                            matches!(kind, OperationKind::Accept | OperationKind::Preview)
                                .then(|| self.reference_session.active_kind())
                                .flatten()
                        });
                    if let Some(provider) = provider.filter(|provider| {
                        matches!(
                            provider,
                            ReferenceKind::GitHubIssue
                                | ReferenceKind::GitHubPullRequest
                                | ReferenceKind::JiraIssue
                        )
                    }) {
                        self.providers.record_failure(provider, &message);
                    }
                    self.status = format!("Cannot {}: {message}", operation_name(kind));
                }
                ReferenceEvent::ContextTotalChanged { revision, total }
                    if revision == self.revision() =>
                {
                    self.refs_total = total;
                }
                ReferenceEvent::CandidateCostsChanged { .. }
                | ReferenceEvent::ContextTotalChanged { .. }
                | ReferenceEvent::Accepted { .. } => {}
            }
        }
    }
}

fn operation_name(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Accept => "resolve reference",
        OperationKind::Preview => "load preview",
        OperationKind::Lower => "lower document",
    }
}
