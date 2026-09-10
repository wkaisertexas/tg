use super::*;
use crate::references::file::read_versioned;
use sha2::{Digest, Sha256};

#[test]
fn file_parse_cache_uses_content_versions_not_just_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("cache.rs");
    fs::write(&path, "fn before() {}\n").unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let provider = SymbolProvider::new(temp.path(), "@").unwrap();
    assert_eq!(
        query_file(&provider, &path, "before")[0].display.primary,
        "before"
    );
    fs::write(&path, "fn afterx() {}\n").unwrap();
    fs::File::open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(
        query_file(&provider, &path, "afterx")[0].display.primary,
        "afterx"
    );
    assert_eq!(provider.parse_count(), 2);
}

#[test]
fn file_queries_preserve_exact_and_direct_child_ranking_and_ranges() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("model.rs");
    fs::write(
        &path,
        "struct User;\nimpl User { fn render(&self) {} fn renderer(&self) {} }\nfn render() {}\n",
    )
    .unwrap();
    let provider = SymbolProvider::new(temp.path(), "@").unwrap();
    let exact = query_file(&provider, &path, "render");
    assert_eq!(exact[0].display.primary, "render");
    let child = query_file(&provider, &path, "User.render");
    assert_eq!(child[0].display.primary, "User::render");
    let target = resolved(&provider, &child[0]);
    assert!(target.start_byte < target.name_start_byte);
    assert!(target.name_end_byte <= target.end_byte);
    assert_eq!(target.file.origin, FileOrigin::GitAware);
    assert_eq!(child[0].source_version, target.file.source_version);
    assert_eq!(child[0].context_cost, ContextCost::Pending);
    assert_eq!(child[0].file_context_cost, Some(ContextCost::Pending));
    assert!(matches!(
        &child[0].token_source,
        Some(CandidateTokenSource::Symbol { path: candidate_path, start_byte, end_byte })
            if candidate_path == &path.canonicalize().unwrap() && *start_byte == target.start_byte && *end_byte == target.end_byte
    ));
}

#[test]
fn unchanged_targets_use_fast_path_and_moved_targets_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("move.rs");
    fs::write(&path, "fn naïve() {}\n").unwrap();
    let provider = SymbolProvider::new(temp.path(), "@").unwrap();
    let candidates = query_file(&provider, &path, "naïve");
    let target = ReferenceTarget::Symbol(resolved(&provider, &candidates[0]));
    let parsed = provider.parse_count();
    provider.validate(&target).unwrap();
    assert_eq!(provider.parse_count(), parsed);
    fs::write(&path, "\n\nfn naïve() {}\n").unwrap();
    let validated = provider.validate(&target).unwrap();
    let ReferenceTarget::Symbol(refreshed) = validated.target else {
        panic!()
    };
    assert_eq!(refreshed.location.line, 3);
    assert_eq!(
        &fs::read_to_string(&path).unwrap()[refreshed.name_start_byte..refreshed.name_end_byte],
        "naïve"
    );
}

#[test]
fn parsed_symbols_and_version_share_the_same_byte_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("snapshot.rs");
    fs::write(&path, "fn before() {}\n").unwrap();
    let snapshot = read_versioned(&path).unwrap();
    fs::write(&path, "fn after() {}\n").unwrap();
    let parsed = language::parse_source(&path, String::from_utf8(snapshot.bytes.clone()).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(parsed.symbols[0].leaf_name, "before");
    assert_eq!(
        snapshot.version.content_sha256,
        Sha256::digest(&snapshot.bytes).as_slice()
    );
    assert_ne!(snapshot.version, file_version(&path).unwrap());
}

#[test]
fn changed_missing_ambiguous_and_deleted_targets_fail() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("stale.rs");
    fs::write(&path, "fn target() {}\n").unwrap();
    let provider = SymbolProvider::new(temp.path(), "@").unwrap();
    let candidate = query_file(&provider, &path, "target").remove(0);
    let target = ReferenceTarget::Symbol(resolved(&provider, &candidate));
    fs::write(&path, "fn renamed() {}\n").unwrap();
    assert!(
        provider
            .validate(&target)
            .unwrap_err()
            .to_string()
            .contains("no longer exists")
    );
    fs::write(&path, "fn target() {}\nfn target() {}\n").unwrap();
    assert!(
        provider
            .validate(&target)
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    fs::remove_file(&path).unwrap();
    assert!(
        provider
            .validate(&target)
            .unwrap_err()
            .to_string()
            .contains("selected file no longer exists")
    );
}

#[test]
fn markdown_targets_refresh_and_lower_to_anchors() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("guide.md");
    fs::write(&path, "# Guide\n\n## Quick Start\n").unwrap();
    let provider = SymbolProvider::new(temp.path(), "@").unwrap();
    let candidate = query_file(&provider, &path, "QuickStart").remove(0);
    let target = ReferenceTarget::Symbol(resolved(&provider, &candidate));
    let lowered = provider
        .lower(&provider.validate(&target).unwrap())
        .unwrap();
    assert_eq!(lowered, "guide.md#quick-start");
    fs::write(&path, "\n# Guide\n\n## Quick Start\n").unwrap();
    let refreshed = provider.validate(&target).unwrap();
    let ReferenceTarget::Symbol(symbol) = &refreshed.target else {
        panic!()
    };
    assert_eq!(symbol.location.line, 4);
    fs::write(&path, "# Guide\n\n## Setup\n").unwrap();
    assert!(provider.validate(&target).is_err());
}
