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
