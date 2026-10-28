#!/bin/sh
# Usage: check.sh [all|native|wasm|wasm-fast|wasm-release|wasm-report]
# wasm-fast uses cached builds and reports release-binary drift without gating PRs.
# wasm-release gates release hashes only; CI supplies a clean RUNNER_TEMP target.
# wasm-report builds release packages and reports metadata without checking committed hashes.
# all/wasm retain strict local checks. Render hash checks are independent.
set -eu
cd "$(dirname "$0")/.."
phase=${1:-all}
case "$phase" in
  all|native|wasm|wasm-fast|wasm-release|wasm-report) ;;
  *)
    echo "usage: check.sh [all|native|wasm|wasm-fast|wasm-release|wasm-report]" >&2
    exit 1
    ;;
esac
# Native artifacts, including the wasm-bindgen-cli install below.
export CARGO_TARGET_DIR="${ALTO_TARGET_DIR:-target}"
wasm_target="${ALTO_WASM_TARGET_DIR:-$CARGO_TARGET_DIR}"
if [ "$phase" = all ] || [ "$phase" = native ]; then
  cargo fmt --all -- --check
  cargo test --workspace --locked -- --show-output
  cargo clippy --workspace --all-targets --locked -- -D warnings
fi
if [ "$phase" = native ]; then
  exit 0
fi
if [ "$phase" != wasm-release ] && [ "$phase" != wasm-report ]; then
  CARGO_TARGET_DIR="$wasm_target" cargo clippy --workspace --target wasm32-unknown-unknown --locked -- -D warnings
fi
# Collect all package and metadata mismatches before returning a failure.
hash_status=0
check_sums() {
  sh scripts/verify-wasm-sums.sh "$@" || hash_status=1
}
if [ "$phase" != wasm-report ]; then
  check_sums release examples/browser-playground/pkg committed
  check_sums release packages/alto-sdk/dist/pkg committed
  check_sums vector-comparison examples/vector-comparison/pkg committed
fi
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
if [ "$phase" = wasm-release ] || [ "$phase" = wasm-report ]; then
  sh scripts/build-vector-comparison.sh "$pkg_out/vector"
  if [ "$phase" = wasm-report ]; then
    node scripts/report-wasm-hashes.mjs "${ALTO_WASM_HASH_REPORT_DIR:?}" "$pkg_out"
    exit 0
  fi
  check_sums release "$pkg_out"
  check_sums vector-comparison "$pkg_out/vector"
  ALTO_METADATA_RELEASE_DIR="$pkg_out" node scripts/check-build-metadata.mjs || hash_status=1
  exit "$hash_status"
fi
check_sums release "$pkg_out"
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
if [ "$phase" = wasm-fast ]; then
  node scripts/check-build-metadata.mjs --fast
else
  node scripts/check-build-metadata.mjs || hash_status=1
  [ "$hash_status" -eq 0 ]
fi
sh scripts/fetch-wasm.test.sh
sh scripts/guard-pkg-replace.test.sh
sh scripts/mark-vector-enforced.test.sh
