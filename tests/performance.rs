use std::path::PathBuf;
use std::time::{Duration, Instant};
use tscodeselection::language;
use tscodeselection::search::{self, SearchMode};

const LLVM_INDEX_LIMIT: Duration = Duration::from_secs(8);
const LLVM_MINIMUM_SOURCE_FILES: usize = 60_000;
const LLVM_MINIMUM_SYMBOLS: usize = 1_000_000;

#[test]
#[ignore = "requires the pinned full llvm-project checkout"]
fn full_llvm_project_indexes_within_eight_seconds() {
    let root = PathBuf::from(
        std::env::var_os("TG_LLVM_ROOT").expect("TG_LLVM_ROOT must name the llvm-project checkout"),
    );
    assert!(
        root.is_dir(),
        "LLVM checkout is missing: {}",
        root.display()
    );

    let mut source_files: Vec<_> = search::walk(&root, SearchMode::GitAware)
        .into_iter()
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(extension, "c" | "cc" | "cpp" | "cxx" | "h" | "hpp")
                })
        })
        .collect();
    source_files.sort();
    assert!(
        source_files.len() >= LLVM_MINIMUM_SOURCE_FILES,
        "expected a full LLVM checkout, found only {} C/C++ source files",
        source_files.len()
    );

    let started = Instant::now();
    let symbols = language::index_symbols_parallel(&source_files);
    let elapsed = started.elapsed();

    assert!(
        symbols.len() >= LLVM_MINIMUM_SYMBOLS,
        "expected at least {LLVM_MINIMUM_SYMBOLS} LLVM symbols, indexed only {}",
        symbols.len()
    );
    assert!(
        elapsed < LLVM_INDEX_LIMIT,
        "full LLVM indexing took {elapsed:?}, exceeding the {LLVM_INDEX_LIMIT:?} sanity limit"
    );
    eprintln!(
        "LLVM performance sanity: {} files, {} symbols indexed in {elapsed:?}",
        source_files.len(),
        symbols.len()
    );
}
