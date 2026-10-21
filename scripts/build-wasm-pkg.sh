#!/bin/sh
# Release-build alto-wasm and run the pinned wasm-bindgen CLI into OUT_DIR.
# check.sh rebuilds into a temp directory and requires the committed packages to match.
set -eu
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${ALTO_TARGET_DIR:-target}"
out=${1:?usage: build-wasm-pkg.sh OUT_DIR}
bindgen_version=$(sed -n 's/^wasm-bindgen *= *"=\([0-9][0-9.]*\)".*/\1/p' crates/alto-wasm/Cargo.toml)
if [ -z "$bindgen_version" ]; then
  echo "pin wasm-bindgen with an exact '=' version in crates/alto-wasm/Cargo.toml" >&2
  exit 1
fi
have=$(wasm-bindgen --version 2>/dev/null || true)
if [ "$have" != "wasm-bindgen $bindgen_version" ]; then
  echo "wasm-bindgen-cli $bindgen_version required (have: ${have:-none})" >&2
  echo "cargo install wasm-bindgen-cli --version $bindgen_version --locked" >&2
  exit 1
fi
channel=$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' rust-toolchain.toml)
case "$channel" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *)
    echo "rust-toolchain.toml must pin an exact rustc version (have: ${channel:-missing})" >&2
    exit 1
    ;;
esac
rustc_have=$(rustc --version)
case "$rustc_have" in
  "rustc $channel "*) ;;
  *)
    echo "rustc $channel required (have: $rustc_have)" >&2
    exit 1
    ;;
esac
# Absolute paths differ between machines. Remap them to fixed prefixes.
# rustc uses the last matching prefix, so the target directory (often inside
# the checkout) comes last. Callers' RUSTFLAGS are not inherited: an extra
# flag would make the committed wasm unreproducible.
root=$(pwd)
sysroot=$(rustc --print sysroot)
cargo_home=${CARGO_HOME:-$HOME/.cargo}
mkdir -p "$CARGO_TARGET_DIR"
target_dir=$(CDPATH= cd "$CARGO_TARGET_DIR" && pwd)
export RUSTFLAGS="--remap-path-prefix=${cargo_home}=/cargo --remap-path-prefix=${sysroot}=/sysroot --remap-path-prefix=${root}=/src --remap-path-prefix=${target_dir}=/target"
cargo build -p alto-wasm --target wasm32-unknown-unknown --release
wasm-bindgen "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/alto_wasm.wasm" \
  --out-dir "$out" --target web
