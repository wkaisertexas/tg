use super::*;

#[test]
fn whole_file_range_keys_and_service_caches_stay_distinct() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("source.txt");
    fs::write(&path, "hello").unwrap();
    let executor = Arc::new(ManualExecutor::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::new(CountingCounter {
        calls: calls.clone(),
    });
    let first = TokenService::with_counter(executor.clone(), counter.clone());
    let second = TokenService::with_counter(executor.clone(), counter);
    request_file(&first, &path, 1, 1);
    first.request_file_range(
        path.clone(),
        0..5,
        TokenGeneration(1),
        DocumentRevision(1),
        TokenSubject("full-range".into()),
    );
    request_file(&second, &path, 1, 1);
    for _ in 0..3 {
        executor.run(0);
    }
    assert_eq!(first.cache_len(), 2);
    assert_eq!(second.cache_len(), 1);
    assert_eq!(calls.load(Ordering::Relaxed), 3);
}

#[test]
fn file_cache_hits_and_metadata_changes_invalidate_it() {
    let executor = Arc::new(ManualExecutor::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let service = TokenService::with_counter(
        executor.clone(),
        Arc::new(CountingCounter {
            calls: calls.clone(),
        }),
    );
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("cache.txt");
    fs::write(&path, "one").unwrap();
    request_file(&service, &path, 1, 1);
    executor.run(0);
    assert_eq!(
        service.drain_for(TokenGeneration(1), DocumentRevision(1))[0].state,
        TokenState::Ready(3)
    );
    request_file(&service, &path, 1, 1);
    executor.run(0);
    assert_eq!(
        service.drain_for(TokenGeneration(1), DocumentRevision(1))[0].state,
        TokenState::Ready(3)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    fs::write(&path, "different content").unwrap();
    request_file(&service, &path, 1, 2);
    executor.run(0);
    assert_eq!(
        service.drain_for(TokenGeneration(1), DocumentRevision(2))[0].state,
        TokenState::Ready(17)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(service.cache_len(), 2);
}

#[test]
fn byte_and_character_ranges_are_unicode_safe_and_share_the_cache() {
    let executor = Arc::new(ManualExecutor::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let service = TokenService::with_counter(
        executor.clone(),
        Arc::new(CountingCounter {
            calls: calls.clone(),
        }),
    );
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("unicode.txt");
    fs::write(&path, "a\u{1f980}bc").unwrap();
    let identity = FileCacheIdentity::from_path(&path).unwrap();
    let source: Arc<str> = "a\u{1f980}bc".into();
    service.request_range(
        identity.clone(),
        source.clone(),
        SymbolRange::Characters(1..3),
        TokenGeneration(1),
        DocumentRevision(1),
        TokenSubject("range".into()),
    );
    executor.run(0);
    assert_eq!(
        service.drain_for(TokenGeneration(1), DocumentRevision(1))[0].state,
        TokenState::Ready(2)
    );
    service.request_range(
        identity,
        source,
        SymbolRange::Bytes(1..6),
        TokenGeneration(1),
        DocumentRevision(1),
        TokenSubject("range".into()),
    );
    executor.run(0);
    assert_eq!(
        service.drain_for(TokenGeneration(1), DocumentRevision(1))[0].state,
        TokenState::Ready(2)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn invalid_utf8_files_report_bytes_and_invalid_ranges_are_unavailable() {
    let executor = Arc::new(ManualExecutor::default());
    let service = TokenService::new(executor.clone());
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("binary.dat");
    fs::write(&path, [0xff, 0xfe]).unwrap();
    request_file(&service, &path, 1, 1);
    executor.run(0);
    assert_eq!(
        service.drain_for(TokenGeneration(1), DocumentRevision(1))[0].state,
        TokenState::Bytes(2)
    );
    let identity = FileCacheIdentity::from_path(&path).unwrap();
    service.request_range(
        identity,
        Arc::<str>::from("\u{1f980}"),
        SymbolRange::Bytes(1..2),
        TokenGeneration(1),
        DocumentRevision(1),
        TokenSubject("bad-range".into()),
    );
    executor.run(0);
    assert_eq!(
        service.drain_for(TokenGeneration(1), DocumentRevision(1))[0].state,
        TokenState::Unavailable
    );
}
