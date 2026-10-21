#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
sh scripts/build-wasm-pkg.sh examples/browser-playground/pkg
rm -rf packages/alto-sdk/dist/pkg
mkdir -p packages/alto-sdk/dist/pkg
cp -a examples/browser-playground/pkg/. packages/alto-sdk/dist/pkg/
ls -lh examples/browser-playground/pkg/alto_wasm_bg.wasm
