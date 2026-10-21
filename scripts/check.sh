#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${ALTO_TARGET_DIR:-target}"
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --target wasm32-unknown-unknown -- -D warnings
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
