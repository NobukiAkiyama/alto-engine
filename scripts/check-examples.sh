#!/bin/sh
# Builds the embedding examples with Vite, as a host app would, and drives each build
# in headless Chrome through the public API (scripts/verify-browser.mjs --examples).
set -eu
cd "$(dirname "$0")/.."
pnpm --dir packages/alto-sdk build
for example in minimal-embed react-embed; do
  pnpm --dir "examples/$example" install --frozen-lockfile
  pnpm --dir "examples/$example" build
done
node scripts/verify-browser.mjs --examples
