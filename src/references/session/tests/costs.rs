use super::*;

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
    assert!(events.iter().any(|event| matches!(event, ReferenceEvent::CandidateCostsChanged { generation } if *generation == session.generation())));
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
    assert!(events.iter().any(|event| matches!(event, ReferenceEvent::ContextTotalChanged { total, .. } if total.ready_tokens == 2 && total.pending == 0)));
    assert_eq!(session.context_total().ready_tokens, 2);
    assert_eq!(session.context_total().pending, 0);
}
