use super::*;

#[test]
fn request_variants_preserve_pending_state_routing_and_failures() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("unicode.txt");
    fs::write(&path, "a\u{1f980}b").unwrap();
    let file = FileCacheIdentity::from_path(&path).unwrap();
    let executor = Arc::new(ManualExecutor::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let service = TokenService::with_counter(
        executor.clone(),
        Arc::new(CountingCounter {
            calls: calls.clone(),
        }),
    );
    let tickets = [
        service.request_file(
            path.clone(),
            TokenGeneration(1),
            DocumentRevision(11),
            TokenSubject("file".into()),
        ),
        service.request_range(
            file.clone(),
            Arc::from("a\u{1f980}b"),
            SymbolRange::Characters(1..2),
            TokenGeneration(2),
            DocumentRevision(12),
            TokenSubject("memory".into()),
        ),
        service.request_file_range(
            path,
            1..5,
            TokenGeneration(3),
            DocumentRevision(13),
            TokenSubject("disk".into()),
        ),
        service.request_file_range(
            temp.path().join("missing"),
            0..1,
            TokenGeneration(4),
            DocumentRevision(14),
            TokenSubject("missing".into()),
        ),
        service.request_range(
            file,
            Arc::from("a\u{1f980}b"),
            SymbolRange::Bytes(2..3),
            TokenGeneration(5),
            DocumentRevision(15),
            TokenSubject("invalid".into()),
        ),
    ];
    assert!(
        tickets
            .iter()
            .all(|ticket| ticket.state == TokenState::Pending)
    );
    assert!(service.drain().is_empty());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    for _ in &tickets {
        executor.run(0);
    }
    let updates = service.drain();
    assert_eq!(updates.len(), tickets.len());
    for (index, ((update, ticket), (subject, state))) in updates
        .iter()
        .zip(&tickets)
        .zip([
            ("file", TokenState::Ready(3)),
            ("memory", TokenState::Ready(1)),
            ("disk", TokenState::Ready(1)),
            ("missing", TokenState::Unavailable),
            ("invalid", TokenState::Unavailable),
        ])
        .enumerate()
    {
        assert_eq!(update.id, ticket.id);
        assert_eq!(ticket.id, TokenRequestId(index as u64 + 1));
        assert_eq!(update.generation, TokenGeneration(index as u64 + 1));
        assert_eq!(update.revision, DocumentRevision(index as u64 + 11));
        assert_eq!(update.subject.0, subject);
        assert_eq!(update.state, state);
    }
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(service.cache_len(), 2);
}

#[test]
fn known_o200k_base_fixtures_are_exact() {
    let counter = Gpt4oCounter;
    for (text, expected) in [
        ("", 0),
        ("hello", 1),
        ("hello world", 2),
        ("antidisestablishmentarianism", 6),
        ("2 + 2 = 4", 7),
        ("お誕生日おめでとう", 8),
    ] {
        assert_eq!(counter.count(text).unwrap(), expected, "{text:?}");
    }
}

#[test]
fn requests_are_pending_and_do_not_run_inline() {
    let executor = Arc::new(ManualExecutor::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let service = TokenService::with_counter(
        executor.clone(),
        Arc::new(CountingCounter {
            calls: calls.clone(),
        }),
    );
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("large.txt");
    fs::write(&path, "content").unwrap();
    assert_eq!(
        request_file(&service, &path, 1, 1).state,
        TokenState::Pending
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    executor.run(0);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn counting_runs_off_the_caller_thread() {
    let caller = std::thread::current().id();
    let executor = Arc::new(ManualExecutor::default());
    let threads = Arc::new(Mutex::new(Vec::new()));
    let service = TokenService::with_counter(
        executor.clone(),
        Arc::new(ThreadRecordingCounter {
            threads: threads.clone(),
        }),
    );
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("thread.txt");
    fs::write(&path, "content").unwrap();
    request_file(&service, &path, 1, 1);
    assert!(threads.lock().unwrap().is_empty());
    executor.run_on_background_thread(0);
    assert!(
        threads
            .lock()
            .unwrap()
            .iter()
            .all(|thread| *thread != caller)
    );
}

#[test]
fn stale_generation_and_revision_updates_are_rejected_but_cached() {
    let executor = Arc::new(ManualExecutor::default());
    let service = TokenService::new(executor.clone());
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("stale.txt");
    fs::write(&path, "hello").unwrap();
    request_file(&service, &path, 1, 4);
    executor.run(0);
    assert!(
        service
            .drain_for(TokenGeneration(2), DocumentRevision(4))
            .is_empty()
    );
    assert_eq!(service.cache_len(), 1);
}
