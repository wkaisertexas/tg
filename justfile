set dotenv-load := true

default: run

run root=".":
    cargo run --release -- "{{root}}"

resolve prompt root=".":
    cargo run --release -- "{{root}}" --resolve "{{prompt}}"

build:
    cargo build --release

test:
    cargo test --all-targets --all-features

check:
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test --all-targets --all-features

fmt:
    cargo fmt
    nix fmt flake.nix

nix-build:
    nix build
