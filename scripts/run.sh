#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
# The WASM packages and the SDK dist are build products and are not in git.
missing=
for file in \
  examples/browser-playground/pkg/alto_wasm_bg.wasm \
  examples/browser-playground/pkg/alto_wasm.js \
  examples/browser-playground/pkg/alto_decode_bg.wasm \
  examples/browser-playground/pkg/alto_decode.js \
  packages/alto-sdk/dist/index.js \
  packages/alto-sdk/dist/worker.js; do
  if [ ! -f "$file" ]; then
    missing="$missing $file"
  fi
done
if [ -n "$missing" ]; then
  echo "build products are missing:$missing" >&2
  echo "build them from this checkout first:" >&2
  echo "  sh scripts/build.sh" >&2
  exit 1
fi
echo "Alto Engine: http://localhost:${ALTO_PORT:-8765}/examples/browser-playground/"
exec python3 -m http.server "${ALTO_PORT:-8765}" --bind 127.0.0.1
