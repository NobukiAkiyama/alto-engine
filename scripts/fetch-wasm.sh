#!/bin/sh
# Download the pinned GitHub Release WASM assets into the package directories.
# Hashes are checked against wasm/SHA256SUMS. Glue (.js/.d.ts) stays in git.
set -eu
cd "$(dirname "$0")/.."
tag=${1:-}
if [ -z "$tag" ]; then
  tag=$(sed -n 's/^tag=//p' wasm/RELEASE | head -n 1)
fi
if [ -z "$tag" ] || [ "$tag" = none ]; then
  echo "wasm/RELEASE has no tag. Build locally instead:" >&2
  echo "  sh scripts/build-m0.sh" >&2
  echo "  sh scripts/build-vector-comparison.sh" >&2
  exit 1
fi
repo=${ALTO_RELEASE_REPO:-NobukiAkiyama/alto-engine}
base="https://github.com/${repo}/releases/download/${tag}"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fetch() {
  name=$1
  echo "fetch $name from $tag"
  curl --fail --location --retry 3 --silent --show-error \
    "$base/$name" --output "$work/$name"
}

fetch alto_wasm_bg.wasm
fetch alto_decode_bg.wasm
mkdir -p "$work/release"
cp "$work/alto_wasm_bg.wasm" "$work/alto_decode_bg.wasm" "$work/release/"
sh scripts/verify-wasm-sums.sh release "$work/release" subset

install_wasm() {
  src=$1
  dest=$2
  mkdir -p "$(dirname "$dest")"
  cp "$src" "$dest"
}
install_wasm "$work/alto_wasm_bg.wasm" examples/browser-playground/pkg/alto_wasm_bg.wasm
install_wasm "$work/alto_decode_bg.wasm" examples/browser-playground/pkg/alto_decode_bg.wasm
install_wasm "$work/alto_wasm_bg.wasm" packages/alto-sdk/dist/pkg/alto_wasm_bg.wasm
install_wasm "$work/alto_decode_bg.wasm" packages/alto-sdk/dist/pkg/alto_decode_bg.wasm

vector_ok=0
if curl --fail --location --retry 3 --silent --show-error \
  "$base/vector-comparison-alto_wasm_bg.wasm" --output "$work/vector-comparison-alto_wasm_bg.wasm"
then
  mkdir -p "$work/vector"
  cp "$work/vector-comparison-alto_wasm_bg.wasm" "$work/vector/alto_wasm_bg.wasm"
  if sh scripts/verify-wasm-sums.sh vector-comparison "$work/vector" subset; then
    install_wasm "$work/vector/alto_wasm_bg.wasm" examples/vector-comparison/pkg/alto_wasm_bg.wasm
    vector_ok=1
  fi
fi
if [ "$vector_ok" -ne 1 ]; then
  echo "comparison WASM was not installed from $tag (missing asset, or SHA-256 does not match wasm/SHA256SUMS)" >&2
  echo "build it locally: sh scripts/build-vector-comparison.sh" >&2
fi
echo "installed release WASM from $tag"
