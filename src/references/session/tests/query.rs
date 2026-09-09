use super::*;

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
            indexed_symbols: 3
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
