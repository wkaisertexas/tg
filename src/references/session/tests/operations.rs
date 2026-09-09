use super::*;

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
