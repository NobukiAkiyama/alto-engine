#!/bin/sh
# Rebuild both WASM packages on aarch64-apple-darwin and rewrite wasm/SHA256SUMS
# plus the hash fields in verification/build.json. CI does not run this.
set -eu
cd "$(dirname "$0")/.."
host=$(rustc -vV | sed -n 's/^host: //p')
if [ "$host" != "aarch64-apple-darwin" ]; then
  echo "update-wasm-sums.sh must run on aarch64-apple-darwin (this host is ${host:-unknown})" >&2
  echo "CI rebuilds the release package on that host and rejects any other bytes" >&2
  exit 1
fi
rel=$(mktemp -d)
vec=$(mktemp -d)
trap 'rm -rf "$rel" "$vec"' EXIT
sh scripts/build-wasm-pkg.sh "$rel"
sh scripts/build-vector-comparison.sh "$vec"

hash_group() {
  group=$1
  dir=$2
  for path in "$dir"/*; do
    [ -f "$path" ] || continue
    basename "$path"
  done | sort | while IFS= read -r name; do
    if command -v sha256sum >/dev/null 2>&1; then
      sum=$(sha256sum "$dir/$name" | awk '{print $1}')
    else
      sum=$(shasum -a 256 "$dir/$name" | awk '{print $1}')
    fi
    printf '%s  %s/%s\n' "$sum" "$group" "$name"
  done
}

{
  echo "# SHA-256 of WASM packages rebuilt by CI. The binaries are not stored in git."
  echo "# release: scripts/build-wasm-pkg.sh. Playground and @alto/engine share these bytes."
  echo "# vector-comparison: scripts/build-vector-comparison.sh (--features vello-probe)."
  echo "# Update on aarch64-apple-darwin with: sh scripts/update-wasm-sums.sh"
  hash_group release "$rel"
  hash_group vector-comparison "$vec"
} > wasm/SHA256SUMS

install_pkg() {
  src=$1
  dest=$2
  rm -rf "$dest"
  mkdir -p "$dest"
  cp -a "$src"/. "$dest"/
}
install_pkg "$rel" examples/browser-playground/pkg
install_pkg "$rel" packages/alto-sdk/dist/pkg
install_pkg "$vec" examples/vector-comparison/pkg

node --input-type=module - "$rel/alto_wasm_bg.wasm" "$vec/alto_wasm_bg.wasm" <<'EOF'
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { gzipSync } from "node:zlib";

const wasm = readFileSync(process.argv[2]);
const comparison = readFileSync(process.argv[3]);
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const metadata = JSON.parse(readFileSync("verification/build.json", "utf8"));
metadata.wasmBytes = wasm.length;
metadata.gzipBytes = gzipSync(wasm).length;
metadata.wasmSha256 = sha(wasm);
metadata.comparisonWasm.bytes = comparison.length;
metadata.comparisonWasm.gzipBytes = gzipSync(comparison).length;
metadata.comparisonWasm.sha256 = sha(comparison);
writeFileSync("verification/build.json", JSON.stringify(metadata, null, 2) + "\n");
EOF

# Sets enforce_vector=1 and leaves tag= as it is. A published tag stays valid
# for the assets that tag already has; fetch-wasm.sh does not need this pin.
sh scripts/mark-vector-enforced.sh
echo "updated wasm/SHA256SUMS, verification/build.json, and wasm/RELEASE"
echo "enforce_vector=1 so the release workflow checks the comparison package; tag= was left unchanged"
