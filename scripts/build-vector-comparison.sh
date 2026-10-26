#!/bin/sh
# Build the optional vello-probe package. OUT_DIR defaults to the comparison page.
# Path remapping matches the release package so a later hash update is stable
# on aarch64-apple-darwin. The package currently recorded in wasm/SHA256SUMS
# was built without that remapping.
set -eu
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${ALTO_TARGET_DIR:-target}"
out=${1:-examples/vector-comparison/pkg}
# shellcheck disable=SC1091
. ./scripts/wasm-repro-env.sh
wasm_require_bindgen
wasm_repro_env
mkdir -p "$out"
cargo build -p alto-wasm --features vello-probe --target wasm32-unknown-unknown --release --locked
wasm-bindgen "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/alto_wasm.wasm" --out-dir "$out" --target web
