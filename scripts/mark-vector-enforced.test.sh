#!/bin/sh
# update-wasm-sums.sh must set enforce_vector=1 without clearing a real tag.
set -eu
cd "$(dirname "$0")/.."
file=wasm/RELEASE
backup=$(mktemp)
cp "$file" "$backup"
cleanup() {
  cp "$backup" "$file"
  rm -f "$backup" "$file.bak"
}
trap cleanup EXIT

cat > "$file" << 'EOF'
# pin
tag=v0.4.0
enforce_vector=0
EOF
sh scripts/mark-vector-enforced.sh
grep -q '^tag=v0.4.0$' "$file"
grep -q '^enforce_vector=1$' "$file"
if grep -q '^tag=none$' "$file"; then
  echo "mark-vector-enforced.sh cleared a published tag" >&2
  exit 1
fi

# A missing flag is appended. tag=none stays none; it is not rewritten.
cat > "$file" << 'EOF'
tag=none
EOF
sh scripts/mark-vector-enforced.sh
grep -q '^tag=none$' "$file"
grep -q '^enforce_vector=1$' "$file"

echo "mark-vector-enforced.sh tests passed"
