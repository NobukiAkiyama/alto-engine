#!/bin/sh
# The playground pkg wipe in check.sh must keep uncommitted user files.
set -eu
cd "$(dirname "$0")/.."
pkg=examples/browser-playground/pkg
mkdir -p "$pkg"
note=$pkg/user-notes.txt
tracked=$(git ls-files "$pkg" | head -n 1)
if [ -z "$tracked" ]; then
  echo "expected a tracked file under $pkg" >&2
  exit 1
fi
backup=$(mktemp)
cp "$tracked" "$backup"
cleanup() {
  rm -f "$note" "$pkg/guard-scratch.wasm"
  if [ -f "$backup" ]; then
    cp "$backup" "$tracked"
    rm -f "$backup"
  fi
}
trap cleanup EXIT

# Gitignored wasm is a build product and must not block replacement.
# Use a scratch name so this test does not overwrite a real package binary.
scratch=$pkg/guard-scratch.wasm
printf 'wasm' > "$scratch"
sh scripts/guard-pkg-replace.sh "$pkg"
rm -f "$scratch"

# Untracked, unignored notes abort.
printf 'local notes' > "$note"
err=$(mktemp)
if sh scripts/guard-pkg-replace.sh "$pkg" 2> "$err"; then
  echo "expected the guard to refuse an untracked file" >&2
  exit 1
fi
grep -q "refusing to replace $pkg" "$err"
grep -q "user-notes.txt" "$err"
rm -f "$note"

# A dirty tracked glue file aborts, and the file is still there afterwards.
printf '\n' >> "$tracked"
if sh scripts/guard-pkg-replace.sh "$pkg" 2> "$err"; then
  echo "expected the guard to refuse a modified tracked file" >&2
  exit 1
fi
grep -q "$tracked" "$err"
cp "$backup" "$tracked"
rm -f "$backup"

# Clean again, including the gitignored wasm.
sh scripts/guard-pkg-replace.sh "$pkg"
echo "guard-pkg-replace.sh tests passed"
