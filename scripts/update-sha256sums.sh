#!/bin/sh
# Rewrite the root SHA256SUMS from the git index.
# The manifest lists every tracked path except itself, in `git ls-files` order.
# Gitignored paths are absent because they are not tracked. Do not edit the
# manifest by hand; run this script after the tree is otherwise ready.
set -eu
cd "$(dirname "$0")/.."
tmp=$(mktemp)
list=$(mktemp)
trap 'rm -f "$tmp" "$list"' EXIT
git -c core.quotepath=off ls-files -z > "$list"
python3 - "$list" > "$tmp" << 'PY'
import hashlib
import sys

raw = open(sys.argv[1], "rb").read().split(b"\0")
seen = set()
for item in raw:
    if not item or item == b"SHA256SUMS" or item in seen:
        continue
    seen.add(item)
    path = item.decode("utf-8")
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    sys.stdout.write(f"{digest.hexdigest()}  {path}\n")
PY
mv "$tmp" SHA256SUMS
echo "wrote SHA256SUMS ($(grep -cve '^$' SHA256SUMS) files)"
