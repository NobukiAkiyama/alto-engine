#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${ALTO_TARGET_DIR:-target}"
cargo fmt --all -- --check
cargo test --workspace --locked -- --show-output
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --target wasm32-unknown-unknown --locked -- -D warnings
# The release WASM package is not stored in git. Rebuild it with the pinned
# rustc and wasm-bindgen CLI and fail unless every output byte matches
# wasm/SHA256SUMS. A source change without that hash update fails here.
# Glue that is still committed (.js / .d.ts) is checked first, including the
# vector-comparison package. Gitignored *.wasm files are checked on the rebuild.
sh scripts/verify-wasm-sums.sh release examples/browser-playground/pkg committed
sh scripts/verify-wasm-sums.sh release packages/alto-sdk/dist/pkg committed
sh scripts/verify-wasm-sums.sh vector-comparison examples/vector-comparison/pkg committed
bindgen_version=$(sed -n 's/^wasm-bindgen *= *"=\([0-9][0-9.]*\)".*/\1/p' crates/alto-wasm/Cargo.toml)
if [ -z "$bindgen_version" ]; then
  echo "pin wasm-bindgen with an exact '=' version in crates/alto-wasm/Cargo.toml" >&2
  exit 1
fi
if [ "$(wasm-bindgen --version 2>/dev/null || true)" != "wasm-bindgen $bindgen_version" ]; then
  cargo install wasm-bindgen-cli --version "$bindgen_version" --locked
fi
pkg_out=$(mktemp -d)
trap 'rm -rf "$pkg_out"' EXIT
sh scripts/build-wasm-pkg.sh "$pkg_out"
sh scripts/verify-wasm-sums.sh release "$pkg_out"
sh scripts/guard-pkg-replace.sh examples/browser-playground/pkg
rm -rf examples/browser-playground/pkg
mkdir -p examples/browser-playground/pkg
cp -a "$pkg_out"/. examples/browser-playground/pkg/
rm -rf "$pkg_out"
trap - EXIT
pnpm --dir packages/alto-sdk install --frozen-lockfile
pnpm --dir packages/alto-sdk build
# The public API is what the reports in packages/alto-sdk/api say. A change to it
# must update them in the same commit; `pnpm --dir packages/alto-sdk api` does.
pnpm --dir packages/alto-sdk api:check
if grep -nE '(:|<|\|) *any\b' packages/alto-sdk/api/*.api.md; then
  echo "the public API must not use any" >&2
  exit 1
fi
node --test packages/alto-sdk/test/*.test.js
node scripts/check-build-metadata.mjs
sh scripts/fetch-wasm.test.sh
sh scripts/guard-pkg-replace.test.sh
sh scripts/mark-vector-enforced.test.sh
# SHA256SUMS must not name a gitignored path. verification/*.log matches *.log
# and is not in the tree; listing it makes `sha256sum -c` fail on a clean checkout.
while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in
    ''|\#*) continue ;;
  esac
  file=$(printf '%s\n' "$line" | cut -c 67-)
  if git check-ignore -q -- "$file"; then
    echo "SHA256SUMS lists a gitignored path: $file" >&2
    exit 1
  fi
done < SHA256SUMS
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c SHA256SUMS
else
  shasum -a 256 -c SHA256SUMS
fi
