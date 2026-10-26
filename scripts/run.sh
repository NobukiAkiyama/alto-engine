#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
wasm=examples/browser-playground/pkg/alto_wasm_bg.wasm
decode=examples/browser-playground/pkg/alto_decode_bg.wasm
if [ ! -f "$wasm" ] || [ ! -f "$decode" ]; then
  echo "WASM binaries are not in git. Fetching the newest GitHub Release whose assets match wasm/SHA256SUMS."
  sh scripts/fetch-wasm.sh
fi
echo "Alto Engine: http://localhost:${ALTO_PORT:-8765}/examples/browser-playground/"
exec python3 -m http.server "${ALTO_PORT:-8765}" --bind 127.0.0.1
