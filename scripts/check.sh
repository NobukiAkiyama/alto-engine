#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${ALTO_TARGET_DIR:-target}"
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --target wasm32-unknown-unknown -- -D warnings
pnpm --dir packages/alto-sdk build
