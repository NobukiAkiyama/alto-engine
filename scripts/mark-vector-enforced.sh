#!/bin/sh
# Record that the comparison WASM hash was rewritten on aarch64-apple-darwin.
# Leaves tag= untouched. fetch-wasm.sh selects a release by asset hash, and a
# published tag stays as the pin for the assets that tag already uploaded.
set -eu
cd "$(dirname "$0")/.."
file=wasm/RELEASE
if [ ! -f "$file" ]; then
  echo "missing $file" >&2
  exit 1
fi
if grep -q '^enforce_vector=' "$file"; then
  sed -i.bak 's/^enforce_vector=.*/enforce_vector=1/' "$file"
  rm -f "$file.bak"
else
  printf '\nenforce_vector=1\n' >> "$file"
fi
