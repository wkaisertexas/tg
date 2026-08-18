# v0.1 Performance Baseline

This bounded baseline characterizes the selector before the prompt-editor
migration. It is a regression reference, not a universal performance promise.

The measurements were recorded on a 14-core Apple workstation using release
builds and the shallow corpora prepared by `just bench-prepare`:

| Corpus | Parsed files | Mean symbol-index time |
| --- | ---: | ---: |
| Rust | 38,057 | 1.38 s |
| LLVM | 71,158 | 5.74 s |
| Ansible | 1,812 | 63.4 ms |

For repeatable development checks, run `just bench-quick`. It caps each corpus
at 250 source files through `TG_BENCH_FILE_LIMIT=250`. Before a delivery gate,
run `just bench` on the same machine and corpus revisions and compare the
repository walk, symbol index, and symbol query Criterion reports. Hardware,
filesystem cache state, worker count, and moving upstream corpus revisions can
materially affect absolute timings, so only like-for-like runs should be used
to diagnose regressions.

## Post-migration bounded comparison

On 2026-08-17, the quick gate was run on an Apple M4 Max (`arm64`) against the
same local shallow corpora and filesystem cache, with the pre-migration
characterization commit `6d60e69` saved as a Criterion baseline. The
post-migration working tree was based on `2c792fc`; each symbol index was capped
at 250 source files. Values below are the Criterion interval midpoint.

| Corpus | Walk before | Walk after | Index before | Index after | Query before | Query after |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Rust | 670.59 ms | 111.94 ms | 48.28 ms | 45.03 ms | 395.05 us | 394.20 us |
| LLVM | 2.168 s | 413.30 ms | 46.05 ms | 45.51 ms | 551.08 us | 547.95 us |
| Ansible | 92.99 ms | 33.58 ms | 24.31 ms | 24.84 ms | 144.90 us | 145.63 us |

Criterion classified every post-migration walk as improved (64-83%), Rust
indexing as improved, and the remaining bounded index/query comparisons as no
change. The walk improvement removes redundant per-file canonicalization while
retaining non-followed symlink and exact-resolution containment tests.

Reproduction commands:

```sh
TG_BENCH_FILE_LIMIT=250 cargo bench --bench repository -- --save-baseline pre-migration
TG_BENCH_FILE_LIMIT=250 cargo bench --bench repository -- --baseline pre-migration
```

## Full LLVM sanity gate

The release-mode sanity gate indexes every C and C++ source/header in LLVM
22.1.8 at commit `ca7933e47d3a3451d81e72ac174dcb5aa28b59d1`. It requires at least
60,000 files and 1,000,000 extracted symbols, then fails if indexing takes eight
seconds or longer.

```sh
just perf-prepare
just perf-check
```

The timed region excludes checkout and repository walking. Normal CI compiles
the ignored test, while the absolute timing assertion runs on performance
hardware: a standard shared GitHub runner measured 37.36 seconds for the same
workload and cannot represent the eight-second workstation requirement. The
Apple M4 Max reference run indexed 71,158 files and 1,456,878 symbols in 5.83
seconds.
