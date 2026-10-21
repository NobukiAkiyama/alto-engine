#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${ALTO_TARGET_DIR:-target}"
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --target wasm32-unknown-unknown -- -D warnings
# The shipped wasm package is a build artifact. Rebuild it with the pinned
# rustc and wasm-bindgen CLI and fail if the committed bytes differ.
bindgen_version=$(sed -n 's/^wasm-bindgen *= *"=\([0-9][0-9.]*\)".*/\1/p' crates/alto-wasm/Cargo.toml)
if [ -z "$bindgen_version" ]; then
  echo "pin wasm-bindgen with an exact '=' version in crates/alto-wasm/Cargo.toml" >&2
  exit 1
fi
if [ "$(wasm-bindgen --version 2>/dev/null || true)" != "wasm-bindgen $bindgen_version" ]; then
  cargo install wasm-bindgen-cli --version "$bindgen_version" --locked
fi
pkg_out=$(mktemp -d)
sh scripts/build-wasm-pkg.sh "$pkg_out"
pkg_status=0
for dest in examples/browser-playground/pkg packages/alto-sdk/dist/pkg; do
  if ! diff -rq "$pkg_out" "$dest"; then
    echo "committed $dest does not match a fresh release build" >&2
    echo "regenerate with: sh scripts/build-m0.sh" >&2
    pkg_status=1
  fi
done
rm -rf "$pkg_out"
[ "$pkg_status" -eq 0 ]
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
sha256sum -c SHA256SUMS
pnpm --dir packages/alto-sdk install --frozen-lockfile
pnpm --dir packages/alto-sdk build
