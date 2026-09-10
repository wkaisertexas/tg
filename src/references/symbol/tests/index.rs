use super::super::index::rank_repository_symbols;
use super::*;

#[test]
fn cancelling_a_subscriber_does_not_cancel_the_shared_repository_index() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("first.rs"), "struct First;\n").unwrap();
    fs::write(temp.path().join("second.rs"), "struct Second;\n").unwrap();
    let provider = SymbolProvider::with_batch_size(temp.path(), "@", 1, &[]).unwrap();
    let cancellation = CancellationFlag::default();
    let mut first_emissions = 0;
    provider
        .query_progressive(
            request(1, "", QueryScope::Repository),
            &cancellation,
            &mut |_| {
                first_emissions += 1;
                cancellation.cancel();
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(first_emissions, 1);
    let mut final_emission = None;
    provider
        .query_progressive(
            request(2, "", QueryScope::Repository),
            &CancellationFlag::default(),
            &mut |emission| {
                final_emission = Some(emission);
                Ok(())
            },
        )
        .unwrap();
    let final_emission = final_emission.unwrap();
    assert!(final_emission.completed);
    assert_eq!(final_emission.generation, GenerationId(2));
    assert_eq!(final_emission.candidates.len(), 2);
    assert_eq!(final_emission.progress.unwrap().scanned, 2);
    assert_eq!(provider.index_launch_count(), 1);
}

#[test]
fn repository_ranking_caps_results_and_uses_path_then_line_ties() {
    let version = FileVersion {
        size: 0,
        modified: None,
        content_sha256: [0; 32],
    };
    let entries: Vec<_> = (0..110)
        .map(|index| IndexedSymbol {
            relative_path: format!("{index:03}.rs"),
            canonical_path: PathBuf::from(format!("{index:03}.rs")),
            source_version: version.clone(),
            origin: FileOrigin::Broad,
            symbol: Symbol {
                leaf_name: "render".into(),
                qualified_name: "render".into(),
                kind: "function".into(),
                start: language::SourcePoint { line: 2, column: 1 },
                name_start_byte: 0,
                name_end_byte: 6,
                range_start_byte: 0,
                range_end_byte: 8,
                is_definition: true,
            },
        })
        .collect();
    let ranked = rank_repository_symbols(&entries, "render", 100);
    assert_eq!(ranked.len(), 100);
    assert_eq!(ranked[0].relative_path, "000.rs");
    assert_eq!(ranked[99].relative_path, "099.rs");
    assert_eq!(
        rank_repository_symbols(&entries, "ren", 1)[0]
            .symbol
            .leaf_name,
        "render"
    );
    let mut qualified = entries[0].clone();
    qualified.symbol.leaf_name = "work".into();
    qualified.symbol.qualified_name = "SpecialModule::work".into();
    assert_eq!(
        rank_repository_symbols(&[qualified], "SpecialModule", 1)[0]
            .symbol
            .leaf_name,
        "work"
    );
}

#[test]
fn repository_index_is_lazy_progressive_includes_ignored_files_and_reuses_one_launch() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join(".git")).unwrap();
    fs::write(temp.path().join(".gitignore"), "ignored.rs\n").unwrap();
    fs::write(temp.path().join("visible.rs"), "struct Visible;\n").unwrap();
    fs::write(temp.path().join("ignored.rs"), "struct Ignored;\n").unwrap();
    let provider = SymbolProvider::with_batch_size(temp.path(), "@", 1, &[]).unwrap();
    assert_eq!(provider.index_launch_count(), 0);
    let mut emissions = Vec::new();
    provider
        .query_progressive(
            request(7, "", QueryScope::Repository),
            &CancellationFlag::default(),
            &mut |emission| {
                emissions.push(emission);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(provider.index_launch_count(), 1);
    assert!(emissions.len() >= 2);
    assert!(emissions.last().unwrap().completed);
    let progress = emissions.last().unwrap().progress.unwrap();
    assert_eq!(progress.scanned, 2);
    assert_eq!(progress.total, 2);
    assert_eq!(
        progress.indexed_symbols,
        emissions.last().unwrap().candidates.len()
    );
    assert!(
        emissions
            .iter()
            .all(|emission| emission.generation == GenerationId(7))
    );
    let ignored_id = emissions
        .last()
        .unwrap()
        .candidates
        .iter()
        .find(|candidate| candidate.friendly_text.contains("ignored.rs"))
        .unwrap()
        .id
        .clone();
    let entries = provider.index_snapshot(0).0;
    let repeated = provider.emission(
        &request(7, "", QueryScope::Repository),
        &entries,
        true,
        progress,
    );
    assert!(
        repeated
            .candidates
            .iter()
            .any(|candidate| candidate.id == ignored_id)
    );
    provider.resolve(&ignored_id).unwrap();
    assert!(
        emissions
            .last()
            .unwrap()
            .candidates
            .iter()
            .any(|candidate| candidate.friendly_text.contains("ignored.rs"))
    );
    provider
        .query(
            request(8, "Visible", QueryScope::Repository),
            &CancellationFlag::default(),
        )
        .unwrap();
    assert_eq!(provider.index_launch_count(), 1);
}

#[test]
fn repository_index_lazily_detects_extensionless_shell_scripts() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("deploy"),
        "#!/usr/bin/env bash\ndeploy_app() { :; }\n",
    )
    .unwrap();
    fs::write(temp.path().join("README"), "plain extensionless text\n").unwrap();
    let provider = SymbolProvider::with_batch_size(temp.path(), "@", 1, &[]).unwrap();
    assert_eq!(provider.index_launch_count(), 0);
    let mut emissions = Vec::new();
    provider
        .query_progressive(
            request(9, "deploy_app", QueryScope::Repository),
            &CancellationFlag::default(),
            &mut |emission| {
                emissions.push(emission);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(provider.index_launch_count(), 1);
    let final_emission = emissions.last().unwrap();
    assert!(final_emission.completed);
    assert_eq!(final_emission.progress.unwrap().scanned, 2);
    assert_eq!(final_emission.progress.unwrap().total, 2);
    assert!(
        final_emission
            .candidates
            .iter()
            .any(|candidate| candidate.display.primary == "deploy_app"
                && candidate.friendly_text.contains("deploy"))
    );
    assert!(
        provider
            .index_snapshot(0)
            .0
            .iter()
            .all(|entry| entry.relative_path != "README")
    );
}
