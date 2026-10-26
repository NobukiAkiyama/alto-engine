# Shared setup for the release WASM package and the vector-comparison package.
# Source this file; do not execute it. Callers must already be at the repo root.
# The release package is byte-checked, so wasm_repro_env must keep the same
# remapped rustc command the recorded hashes were built with.

wasm_bindgen_version() {
  sed -n 's/^wasm-bindgen *= *"=\([0-9][0-9.]*\)".*/\1/p' crates/alto-wasm/Cargo.toml
}

wasm_require_bindgen() {
  bindgen_version=$(wasm_bindgen_version)
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
}

wasm_repro_env() {
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
  # flag would make the recorded wasm unreproducible.
  root=$(pwd)
  sysroot=$(rustc --print sysroot)
  cargo_home=${CARGO_HOME:-$HOME/.cargo}
  mkdir -p "$CARGO_TARGET_DIR"
  target_dir=$(CDPATH= cd "$CARGO_TARGET_DIR" && pwd)
  export RUSTFLAGS="--remap-path-prefix=${cargo_home}=/cargo --remap-path-prefix=${sysroot}=/sysroot --remap-path-prefix=${root}=/src --remap-path-prefix=${target_dir}=/target"
}
