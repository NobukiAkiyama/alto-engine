#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${ALTO_TARGET_DIR:-target}"
cargo build -p alto-wasm --features vello-probe --target wasm32-unknown-unknown --release
wasm-bindgen "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/alto_wasm.wasm" --out-dir examples/vector-comparison/pkg --target web
