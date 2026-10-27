#!/bin/sh
# Refuse to delete a package directory that still has user changes.
# Uncommitted tracked edits and untracked files that are not gitignored abort.
# Gitignored build products (*.wasm) may be replaced.
set -eu
dir=${1:?usage: guard-pkg-replace.sh DIR}
cd "$(dirname "$0")/.."
if [ ! -d "$dir" ]; then
  exit 0
fi
changed=$(
  {
    git diff --name-only -- "$dir"
    git diff --cached --name-only -- "$dir"
    git ls-files --others --exclude-standard -- "$dir"
  } | sed '/^$/d' | sort -u
)
if [ -n "$changed" ]; then
  echo "refusing to replace $dir; it has changes that are not gitignored:" >&2
  printf '%s\n' "$changed" >&2
  echo "check.sh deletes that directory before copying the rebuilt WASM package." >&2
  echo "Commit those files or move them out of the way, then run check.sh again." >&2
  exit 1
fi
