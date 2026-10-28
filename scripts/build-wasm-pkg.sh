#!/bin/sh
# Release-build alto-wasm and alto-decode and run the pinned wasm-bindgen CLI into OUT_DIR.
# check.sh wasm-release requires wasm/SHA256SUMS to match; wasm-fast reports drift.
set -eu
cd "$(dirname "$0")/.."
# Release verification supplies a clean target; fast CI can cache this directory.
export CARGO_TARGET_DIR="${ALTO_WASM_TARGET_DIR:-${ALTO_TARGET_DIR:-target}}"
out=${1:?usage: build-wasm-pkg.sh OUT_DIR}
# shellcheck disable=SC1091
. ./scripts/wasm-repro-env.sh
wasm_require_bindgen
wasm_repro_env
# alto-decode is the decode Workers' module: LZ4 and hashing without the engine.
cargo build -p alto-wasm -p alto-decode --target wasm32-unknown-unknown --release --locked
for module in alto_wasm alto_decode; do
  wasm-bindgen "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/$module.wasm" \
    --out-dir "$out" --target web
done
