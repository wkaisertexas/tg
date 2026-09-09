set dotenv-load := true

default: run

run:
    cargo run --release --

edit file root=".":
    cargo run --release -- --root "{{root}}" "{{file}}"

resolve prompt root=".":
    cargo run --release -- --root "{{root}}" --resolve "{{prompt}}"

build:
    cargo build --release

test:
    cargo test --all-targets --all-features

check:
    python3 tests/test_bump_version.py
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --all-targets --all-features

fmt:
    cargo fmt
    nix fmt flake.nix

nix-build:
    nix build

bench-prepare:
    mkdir -p benchmarks/corpus
    test -d benchmarks/corpus/rust/.git || git clone --depth 1 https://github.com/rust-lang/rust.git benchmarks/corpus/rust
    test -d benchmarks/corpus/llvm-project/.git || git clone --depth 1 https://github.com/llvm/llvm-project.git benchmarks/corpus/llvm-project
    test -d benchmarks/corpus/ansible/.git || git clone --depth 1 https://github.com/ansible/ansible.git benchmarks/corpus/ansible

perf-prepare:
    mkdir -p benchmarks/corpus
    test -d benchmarks/corpus/llvm-project-22.1.8/.git || git clone --depth 1 --branch llvmorg-22.1.8 https://github.com/llvm/llvm-project.git benchmarks/corpus/llvm-project-22.1.8
    test "$(git -C benchmarks/corpus/llvm-project-22.1.8 rev-parse HEAD)" = "ca7933e47d3a3451d81e72ac174dcb5aa28b59d1"

perf-check root="benchmarks/corpus/llvm-project-22.1.8":
    TG_LLVM_ROOT="{{root}}" cargo test --locked --release --test performance -- --ignored --nocapture

bench:
    cargo bench --bench repository

bench-quick:
    TG_BENCH_FILE_LIMIT=250 cargo bench --bench repository
