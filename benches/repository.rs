use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tscodeselection::language;
use tscodeselection::search::{self, SearchMode};

struct Corpus {
    name: &'static str,
    directory: &'static str,
    query: &'static str,
    extensions: &'static [&'static str],
}

const CORPORA: &[Corpus] = &[
    Corpus {
        name: "rust",
        directory: "rust",
        query: "Compiler",
        extensions: &["rs"],
    },
    Corpus {
        name: "llvm",
        directory: "llvm-project",
        query: "Instruction",
        extensions: &["c", "cc", "cpp", "cxx", "h", "hpp"],
    },
    Corpus {
        name: "ansible",
        directory: "ansible",
        query: "TaskExecutor",
        extensions: &["py"],
    },
];

fn benchmark(c: &mut Criterion) {
    let corpus_root = std::env::var_os("TG_BENCH_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("benchmarks/corpus"));
    let limit = std::env::var("TG_BENCH_FILE_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);

    for corpus in CORPORA {
        let root = corpus_root.join(corpus.directory);
        if !root.is_dir() {
            eprintln!("skipping {}: run `just bench-prepare`", corpus.name);
            continue;
        }
        benchmark_corpus(c, corpus, &root, limit);
    }
}

fn benchmark_corpus(c: &mut Criterion, corpus: &Corpus, root: &Path, limit: usize) {
    let all_files = search::walk(root, SearchMode::GitAware);
    let mut source_files: Vec<_> = all_files
        .iter()
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| corpus.extensions.contains(&extension))
        })
        .cloned()
        .collect();
    source_files.sort();
    if limit > 0 {
        source_files.truncate(limit);
    }

    let indexing_started = Instant::now();
    let symbols = language::index_symbols_parallel(&source_files);
    let indexing_elapsed = indexing_started.elapsed();
    eprintln!(
        "{}: {} files discovered, {} files parsed, {} symbols indexed in {:.3}s",
        corpus.name,
        all_files.len(),
        source_files.len(),
        symbols.len(),
        indexing_elapsed.as_secs_f64(),
    );

    let mut walk_group = c.benchmark_group("repository_walk");
    walk_group.sample_size(10);
    walk_group.measurement_time(Duration::from_secs(5));
    walk_group.bench_with_input(BenchmarkId::from_parameter(corpus.name), root, |b, root| {
        b.iter(|| black_box(search::walk(root, SearchMode::GitAware).len()));
    });
    walk_group.finish();

    let mut parse_group = c.benchmark_group("symbol_index");
    parse_group.sample_size(10);
    parse_group.measurement_time(Duration::from_secs(8));
    parse_group.throughput(Throughput::Elements(source_files.len() as u64));
    parse_group.bench_function(corpus.name, |b| {
        b.iter(|| black_box(language::index_symbols_parallel(&source_files).len()));
    });
    parse_group.finish();

    let mut search_group = c.benchmark_group("symbol_query");
    search_group.throughput(Throughput::Elements(symbols.len() as u64));
    search_group.bench_function(corpus.name, |b| {
        b.iter(|| black_box(language::find_symbols(&symbols, corpus.query).len()));
    });
    search_group.finish();
}

criterion_group!(benches, benchmark);
criterion_main!(benches);
