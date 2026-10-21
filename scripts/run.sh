#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
echo "Alto Engine: http://localhost:${ALTO_PORT:-8765}/examples/browser-playground/"
exec python3 -m http.server "${ALTO_PORT:-8765}" --bind 127.0.0.1
