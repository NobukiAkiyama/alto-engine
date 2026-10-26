#!/bin/sh
# Download the newest GitHub Release whose release WASM assets match
# wasm/SHA256SUMS. wasm/RELEASE tag= is not consulted: tag=none still fetches
# when some release's assets match.
# An optional argument limits the search to that one tag.
#
# Each release is screened by its small SHA256SUMS asset. The WASM bodies are
# downloaded only after that file matches, then checked again before install.
# The release list is one GitHub API request per page of 100, newest first,
# and stops at the first match. GITHUB_TOKEN or GH_TOKEN is sent when set
# (unauthenticated calls share a 60/hour limit). Asset downloads use the
# browser URL from that list, not another API call.
#
# Tests set ALTO_RELEASES_JSON (one JSON array, no API), or ALTO_RELEASES_API
# (base URL of a stand-in for https://api.github.com). ALTO_WASM_SUMS and
# ALTO_FETCH_ROOT redirect the hashes and the install tree. Asset URLs in a
# fixture may be file://.
set -eu
cd "$(dirname "$0")/.."

root=${ALTO_FETCH_ROOT:-.}
sums=${ALTO_WASM_SUMS:-wasm/SHA256SUMS}
repo=${ALTO_RELEASE_REPO:-NobukiAkiyama/alto-engine}
only_tag=${1:-}
api_base=${ALTO_RELEASES_API:-https://api.github.com}

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

# First matching name in a sha256sum file. Leading "*" (binary mode) is ignored.
hash_named() {
  file=$1
  shift
  while [ $# -gt 0 ]; do
    found=$(awk -v name="$1" '
      NF >= 2 {
        hash = $1
        file = $2
        sub(/^\*/, "", file)
        if (file == name) { print hash; exit }
      }
    ' "$file")
    if [ -n "$found" ]; then
      printf '%s\n' "$found"
      return 0
    fi
    shift
  done
  return 1
}

want_wasm=$(sum_of release/alto_wasm_bg.wasm)
want_decode=$(sum_of release/alto_decode_bg.wasm)
want_vector=$(sum_of vector-comparison/alto_wasm_bg.wasm)
if [ -z "$want_wasm" ] || [ -z "$want_decode" ]; then
  echo "missing release hashes in $sums" >&2
  exit 1
fi

fetch_one() {
  curl --fail --location --retry 3 --silent --show-error "$1" --output "$2"
}

export ALTO_RELEASE_REPO="$repo"
export ALTO_ONLY_TAG="$only_tag"
export ALTO_NEXT_URL_FILE="$work/next-url"

load_listing() {
  python3 - > "$work/listing.tsv" << 'PY'
import json
import os
import sys
import urllib.request

def emit(releases):
    only = os.environ.get("ALTO_ONLY_TAG") or ""
    rows = []
    for rel in releases:
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
        rows.append((rel.get("published_at") or "", int(rel.get("id") or 0), tag, assets))
    rows.sort(key=lambda row: (row[0], row[1]), reverse=True)
    for index, (_, _, tag, assets) in enumerate(rows):
        sys.stdout.write(f"{index}\tTAG\t{tag.replace(chr(9), ' ')}\n")
        for name, download in assets.items():
            sys.stdout.write(f"{index}\tASSET\t{name}\t{download}\n")

def write_next(url):
    with open(os.environ["ALTO_NEXT_URL_FILE"], "w", encoding="utf-8") as handle:
        handle.write(url or "")

path = os.environ.get("ALTO_RELEASES_JSON")
if path:
    with open(path, encoding="utf-8") as handle:
        data = json.load(handle)
    if isinstance(data, dict):
        data = data.get("releases") or []
    emit(data)
    write_next("")
    sys.exit(0)

url = os.environ["ALTO_RELEASES_URL"]
request = urllib.request.Request(url, headers={
    "Accept": "application/vnd.github+json",
    "User-Agent": "alto-engine-fetch-wasm",
    "X-GitHub-Api-Version": "2022-11-28",
})
token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
if token:
    request.add_header("Authorization", "Bearer " + token)
try:
    with urllib.request.urlopen(request) as response:
        data = json.load(response)
        next_url = ""
        for part in (response.headers.get("Link") or "").split(","):
            section = part.strip()
            if section.startswith("<") and 'rel="next"' in section:
                next_url = section[1:section.index(">")]
                break
except Exception as exc:
    sys.stderr.write(f"could not list GitHub releases for {os.environ['ALTO_RELEASE_REPO']}: {exc}\n")
    sys.exit(1)
if isinstance(data, dict):
    data = data.get("releases") or []
emit(data)
write_next(next_url)
PY
}

parse_listing() {
  page_base=$count
  while IFS=$(printf '\t') read -r index kind name url; do
    [ -n "${index:-}" ] || continue
    dir="$work/rels/$((page_base + index))"
    mkdir -p "$dir"
    if [ "$kind" = TAG ]; then
      printf '%s\n' "$name" > "$dir/tag"
      count=$((count + 1))
    elif [ "$kind" = ASSET ]; then
      printf '%s\n' "$url" > "$dir/url-$name"
    fi
  done < "$work/listing.tsv"
}

consider_pending() {
  while [ "$n" -lt "$count" ]; do
    dir="$work/rels/$n"
    tag=$(cat "$dir/tag")
    n=$((n + 1))
    if [ ! -f "$dir/url-SHA256SUMS" ]; then
      continue
    fi
    if [ ! -f "$dir/url-alto_wasm_bg.wasm" ] || [ ! -f "$dir/url-alto_decode_bg.wasm" ]; then
      continue
    fi
    echo "checksum $tag"
    if ! fetch_one "$(cat "$dir/url-SHA256SUMS")" "$work/release-sums"; then
      echo "could not download SHA256SUMS from $tag" >&2
      continue
    fi
    got_wasm=$(hash_named "$work/release-sums" alto_wasm_bg.wasm release/alto_wasm_bg.wasm || true)
    got_decode=$(hash_named "$work/release-sums" alto_decode_bg.wasm release/alto_decode_bg.wasm || true)
    if [ "$got_wasm" != "$want_wasm" ] || [ "$got_decode" != "$want_decode" ]; then
      continue
    fi
    echo "fetch release WASM from $tag"
    if ! fetch_one "$(cat "$dir/url-alto_wasm_bg.wasm")" "$work/alto_wasm_bg.wasm"; then
      echo "could not download alto_wasm_bg.wasm from $tag" >&2
      continue
    fi
    if ! fetch_one "$(cat "$dir/url-alto_decode_bg.wasm")" "$work/alto_decode_bg.wasm"; then
      echo "could not download alto_decode_bg.wasm from $tag" >&2
      continue
    fi
    if [ "$(hash_file "$work/alto_wasm_bg.wasm")" != "$want_wasm" ] || [ "$(hash_file "$work/alto_decode_bg.wasm")" != "$want_decode" ]; then
      echo "release $tag SHA256SUMS matched $sums, but the WASM bytes did not; skipping" >&2
      continue
    fi
    chosen=$tag
    chosen_dir=$dir
    cp "$work/release-sums" "$work/chosen-sums"
    return 0
  done
  return 1
}

count=0
n=0
chosen=
chosen_dir=
api_calls=0
json_done=0
if [ -n "${ALTO_RELEASES_JSON:-}" ]; then
  next_url=fixture
else
  next_url="${api_base}/repos/${repo}/releases?per_page=100"
fi

while [ -n "$next_url" ]; do
  if [ -n "${ALTO_RELEASES_JSON:-}" ]; then
    if [ "$json_done" -eq 1 ]; then
      break
    fi
    json_done=1
    next_url=
    load_listing
  else
    export ALTO_RELEASES_URL="$next_url"
    api_calls=$((api_calls + 1))
    load_listing
    next_url=$(cat "$work/next-url")
  fi
  if [ -s "$work/listing.tsv" ]; then
    parse_listing
  fi
  if consider_pending; then
    break
  fi
done

if [ "$api_calls" -gt 0 ]; then
  echo "GitHub API requests: $api_calls" >&2
fi

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
got_vector=$(hash_named "$work/chosen-sums" vector-comparison-alto_wasm_bg.wasm vector-comparison/alto_wasm_bg.wasm || true)
if [ -n "$want_vector" ] && [ "$got_vector" = "$want_vector" ] && [ -f "$vector_url" ]; then
  echo "fetch comparison WASM from $chosen"
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
