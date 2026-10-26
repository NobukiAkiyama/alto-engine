#!/bin/sh
# Download the newest GitHub Release whose release WASM assets match
# wasm/SHA256SUMS. wasm/RELEASE tag= is not consulted: tag=none still fetches
# when some release's alto_wasm_bg.wasm and alto_decode_bg.wasm match.
# An optional argument limits the search to that one tag.
#
# Tests set ALTO_RELEASES_JSON (GitHub releases API JSON), ALTO_WASM_SUMS,
# and ALTO_FETCH_ROOT. Asset URLs in the fixture may be file://.
set -eu
cd "$(dirname "$0")/.."

root=${ALTO_FETCH_ROOT:-.}
sums=${ALTO_WASM_SUMS:-wasm/SHA256SUMS}
repo=${ALTO_RELEASE_REPO:-NobukiAkiyama/alto-engine}
only_tag=${1:-}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

sum_of() {
  awk -v key="$1" '$2 == key { print $1; exit }' "$sums"
}

want_wasm=$(sum_of release/alto_wasm_bg.wasm)
want_decode=$(sum_of release/alto_decode_bg.wasm)
want_vector=$(sum_of vector-comparison/alto_wasm_bg.wasm)
if [ -z "$want_wasm" ] || [ -z "$want_decode" ]; then
  echo "missing release hashes in $sums" >&2
  exit 1
fi

export ALTO_RELEASE_REPO="$repo"
export ALTO_ONLY_TAG="$only_tag"
python3 - > "$work/listing.tsv" << 'PY'
import json
import os
import sys
import urllib.request

def load():
    path = os.environ.get("ALTO_RELEASES_JSON")
    if path:
        with open(path, encoding="utf-8") as handle:
            data = json.load(handle)
        if isinstance(data, dict):
            data = data.get("releases") or []
        return data
    repo = os.environ["ALTO_RELEASE_REPO"]
    url = f"https://api.github.com/repos/{repo}/releases?per_page=100"
    found = []
    try:
        while url:
            request = urllib.request.Request(url, headers={
                "Accept": "application/vnd.github+json",
                "User-Agent": "alto-engine-fetch-wasm",
                "X-GitHub-Api-Version": "2022-11-28",
            })
            token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
            if token:
                request.add_header("Authorization", "Bearer " + token)
            with urllib.request.urlopen(request) as response:
                found.extend(json.load(response))
                url = None
                for part in (response.headers.get("Link") or "").split(","):
                    section = part.strip()
                    if section.startswith("<") and section.endswith('rel="next"'):
                        url = section[1:section.index(">")]
    except Exception as exc:
        sys.stderr.write(f"could not list GitHub releases for {repo}: {exc}\n")
        sys.exit(1)
    return found

only = os.environ.get("ALTO_ONLY_TAG") or ""
releases = []
for rel in load():
    if rel.get("draft"):
        continue
    tag = rel.get("tag_name") or ""
    if not tag or (only and tag != only):
        continue
    assets = {}
    for asset in rel.get("assets") or []:
        name = asset.get("name")
        download = asset.get("browser_download_url")
        if name and download:
            assets[name] = download
    releases.append((rel.get("published_at") or "", int(rel.get("id") or 0), tag, assets))

releases.sort(key=lambda row: (row[0], row[1]), reverse=True)
for index, (_, _, tag, assets) in enumerate(releases):
    sys.stdout.write(f"{index}\tTAG\t{tag.replace(chr(9), ' ')}\n")
    for name, download in assets.items():
        sys.stdout.write(f"{index}\tASSET\t{name}\t{download}\n")
PY

count=0
while IFS=$(printf '\t') read -r index kind name url; do
  [ -n "${index:-}" ] || continue
  dir="$work/rels/$index"
  mkdir -p "$dir"
  if [ "$kind" = TAG ]; then
    printf '%s\n' "$name" > "$dir/tag"
    count=$((count + 1))
  elif [ "$kind" = ASSET ]; then
    printf '%s\n' "$url" > "$dir/url-$name"
  fi
done < "$work/listing.tsv"

fetch_one() {
  curl --fail --location --retry 3 --silent --show-error "$1" --output "$2"
}

chosen=
chosen_dir=
n=0
while [ "$n" -lt "$count" ]; do
  dir="$work/rels/$n"
  tag=$(cat "$dir/tag")
  n=$((n + 1))
  if [ ! -f "$dir/url-alto_wasm_bg.wasm" ] || [ ! -f "$dir/url-alto_decode_bg.wasm" ]; then
    continue
  fi
  if ! fetch_one "$(cat "$dir/url-alto_wasm_bg.wasm")" "$work/alto_wasm_bg.wasm"; then
    continue
  fi
  if ! fetch_one "$(cat "$dir/url-alto_decode_bg.wasm")" "$work/alto_decode_bg.wasm"; then
    continue
  fi
  if [ "$(hash_file "$work/alto_wasm_bg.wasm")" != "$want_wasm" ]; then
    continue
  fi
  if [ "$(hash_file "$work/alto_decode_bg.wasm")" != "$want_decode" ]; then
    continue
  fi
  chosen=$tag
  chosen_dir=$dir
  break
done

if [ -z "$chosen" ]; then
  echo "no GitHub Release has release WASM assets matching $sums" >&2
  if [ -n "$only_tag" ]; then
    echo "looked only at tag $only_tag on $repo ($count release listing row(s) after that filter)" >&2
  else
    echo "checked $count release(s) on $repo, newest published first" >&2
  fi
  echo "nothing was installed. Build locally instead:" >&2
  echo "  sh scripts/build-m0.sh" >&2
  echo "  sh scripts/build-vector-comparison.sh" >&2
  exit 1
fi

install_wasm() {
  mkdir -p "$(dirname "$2")"
  cp "$1" "$2"
}
install_wasm "$work/alto_wasm_bg.wasm" "$root/examples/browser-playground/pkg/alto_wasm_bg.wasm"
install_wasm "$work/alto_decode_bg.wasm" "$root/examples/browser-playground/pkg/alto_decode_bg.wasm"
install_wasm "$work/alto_wasm_bg.wasm" "$root/packages/alto-sdk/dist/pkg/alto_wasm_bg.wasm"
install_wasm "$work/alto_decode_bg.wasm" "$root/packages/alto-sdk/dist/pkg/alto_decode_bg.wasm"

vector_ok=0
vector_url="$chosen_dir/url-vector-comparison-alto_wasm_bg.wasm"
if [ -n "$want_vector" ] && [ -f "$vector_url" ]; then
  if fetch_one "$(cat "$vector_url")" "$work/vector.wasm"; then
    if [ "$(hash_file "$work/vector.wasm")" = "$want_vector" ]; then
      install_wasm "$work/vector.wasm" "$root/examples/vector-comparison/pkg/alto_wasm_bg.wasm"
      vector_ok=1
    fi
  fi
fi
if [ "$vector_ok" -ne 1 ]; then
  echo "comparison WASM was not installed from $chosen (missing asset, or SHA-256 does not match $sums)" >&2
  echo "build it locally: sh scripts/build-vector-comparison.sh" >&2
fi
echo "installed release WASM from $chosen"
