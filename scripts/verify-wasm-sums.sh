#!/bin/sh
# Compare a built WASM package to the hashes in wasm/SHA256SUMS.
# Usage: verify-wasm-sums.sh GROUP DIR [exact|subset|committed]
#   exact     DIR has exactly the GROUP files, each byte matching (default)
#   subset    every file in DIR matches GROUP; missing GROUP files are allowed
#   committed tracked files in DIR match GROUP; gitignored binaries may be absent
set -eu
cd "$(dirname "$0")/.."
group=${1:?usage: verify-wasm-sums.sh GROUP DIR [exact|subset|committed]}
dir=${2:?}
mode=${3:-exact}
sums=wasm/SHA256SUMS
case "$mode" in
  exact|subset|committed) ;;
  *)
    echo "mode must be exact, subset, or committed" >&2
    exit 1
    ;;
esac
if [ ! -d "$dir" ]; then
  echo "WASM package directory is missing: $dir" >&2
  exit 1
fi

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

list=$(mktemp)
trap 'rm -f "$list"' EXIT
count=0
while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in
    ''|\#*) continue ;;
  esac
  sum=${line%% *}
  path=${line#*  }
  case "$path" in
    "$group"/*) ;;
    *) continue ;;
  esac
  rel=${path#"$group"/}
  printf '%s %s\n' "$sum" "$rel" >> "$list"
  count=$((count + 1))
done < "$sums"
if [ "$count" -eq 0 ]; then
  echo "wasm/SHA256SUMS has no $group entries" >&2
  exit 1
fi

status=0
hint=0
note_drift() {
  echo "built WASM does not match wasm/SHA256SUMS ($group)" >&2
  if [ "$hint" -eq 0 ]; then
    echo "a source change has to update the recorded hash: sh scripts/update-wasm-sums.sh" >&2
    echo "run that on aarch64-apple-darwin, the same host CI uses to rebuild these bytes" >&2
    hint=1
  fi
  status=1
}

while IFS= read -r row; do
  sum=${row%% *}
  rel=${row#* }
  path="$dir/$rel"
  if [ ! -f "$path" ]; then
    if [ "$mode" = subset ]; then
      continue
    fi
    if [ "$mode" = committed ] && git check-ignore -q -- "$path"; then
      continue
    fi
    echo "missing $group/$rel" >&2
    note_drift
    continue
  fi
  if [ "$mode" = committed ] && git check-ignore -q -- "$path"; then
    continue
  fi
  actual=$(hash_file "$path")
  if [ "$actual" != "$sum" ]; then
    echo "$group/$rel" >&2
    echo "  wasm/SHA256SUMS $sum" >&2
    echo "  built           $actual" >&2
    note_drift
  fi
done < "$list"

for path in "$dir"/*; do
  [ -f "$path" ] || continue
  rel=$(basename "$path")
  if [ "$mode" = committed ] && git check-ignore -q -- "$path"; then
    continue
  fi
  if ! awk -v rel="$rel" '$2 == rel { found = 1 } END { exit !found }' "$list"; then
    echo "unexpected file in $group package: $rel" >&2
    note_drift
  fi
done

exit "$status"
