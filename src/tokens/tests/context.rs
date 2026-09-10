use super::*;
use sha2::{Digest, Sha256};

fn version(source: &str) -> ContextFileVersion {
    ContextFileVersion {
        size: source.len() as u64,
        modified: None,
        content_sha256: Sha256::digest(source).into(),
    }
}

#[test]
fn context_ranges_reuse_file_versions_without_merging_different_contents() {
    let before: crate::references::model::FileVersion = version("old");
    let after: crate::references::model::FileVersion = version("new");
    let range = |file_version, tokens| ReferenceContext {
        identity: Some(ContextIdentity::Range {
            canonical_path: "/repo/file.rs".into(),
            start_byte: 0,
            end_byte: 3,
            file_version,
        }),
        state: TokenState::Ready(tokens),
    };
    let first = range(before, 4);
    let second = range(after, 6);
    assert_eq!(
        context_total([&first, &second, &second.clone()]),
        ContextTotal {
            ready_tokens: 10,
            pending: 0,
            unavailable: 0
        }
    );
}

#[test]
fn totals_deduplicate_and_whole_files_subsume_ranges() {
    let path = PathBuf::from("/repo/src/lib.rs");
    let range = |start, end, state| ReferenceContext {
        identity: Some(ContextIdentity::Range {
            canonical_path: path.clone(),
            start_byte: start,
            end_byte: end,
            file_version: version("source"),
        }),
        state,
    };
    let first = range(0, 10, TokenState::Ready(20));
    let duplicate = range(0, 10, TokenState::Ready(20));
    let second = range(20, 30, TokenState::Pending);
    assert_eq!(
        context_total([&first, &duplicate, &second]),
        ContextTotal {
            ready_tokens: 20,
            pending: 1,
            unavailable: 0
        }
    );
    let whole = ReferenceContext {
        identity: Some(ContextIdentity::File(path)),
        state: TokenState::Ready(100),
    };
    assert_eq!(
        context_total([&first, &duplicate, &second, &whole]),
        ContextTotal {
            ready_tokens: 100,
            pending: 0,
            unavailable: 0
        }
    );
}

#[test]
fn pending_whole_files_also_suppress_ranges_and_non_context_is_free() {
    let path = PathBuf::from("/repo/src/lib.rs");
    let range = ReferenceContext {
        identity: Some(ContextIdentity::Range {
            canonical_path: path.clone(),
            start_byte: 0,
            end_byte: 5,
            file_version: version("hello"),
        }),
        state: TokenState::Ready(1),
    };
    let whole = ReferenceContext {
        identity: Some(ContextIdentity::File(path)),
        state: TokenState::Pending,
    };
    let skill = ReferenceContext {
        identity: None,
        state: TokenState::Ready(999),
    };
    assert_eq!(
        context_total([&range, &whole, &skill]),
        ContextTotal {
            ready_tokens: 0,
            pending: 1,
            unavailable: 0
        }
    );
}

#[test]
fn token_formatting_uses_decimal_thousands_and_half_up_rounding() {
    assert_eq!(format_tokens(0), "0");
    assert_eq!(format_tokens(999), "999");
    assert_eq!(format_tokens(1_000), "1.0k");
    assert_eq!(format_tokens(1_249), "1.2k");
    assert_eq!(format_tokens(1_250), "1.3k");
    assert_eq!(format_tokens(18_700), "18.7k");
}
