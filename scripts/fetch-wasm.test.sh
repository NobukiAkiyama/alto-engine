#!/bin/sh
# Exercise fetch-wasm.sh against a mocked GitHub release listing.
set -eu
cd "$(dirname "$0")/.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

printf 'old-wasm' > "$tmp/old-wasm"
printf 'old-decode' > "$tmp/old-decode"
printf 'new-wasm' > "$tmp/new-wasm"
printf 'new-decode' > "$tmp/new-decode"
printf 'vector' > "$tmp/vector"
printf 'bad-wasm' > "$tmp/bad-wasm"
printf 'bad-decode' > "$tmp/bad-decode"
printf 'bad-vector' > "$tmp/bad-vector"

old_wasm=$(hash_file "$tmp/old-wasm")
old_decode=$(hash_file "$tmp/old-decode")
new_wasm=$(hash_file "$tmp/new-wasm")
new_decode=$(hash_file "$tmp/new-decode")
vector=$(hash_file "$tmp/vector")

uri() {
  python3 -c 'import pathlib, sys; print(pathlib.Path(sys.argv[1]).resolve().as_uri())' "$1"
}

cat > "$tmp/sums-old" << EOF
$old_wasm  release/alto_wasm_bg.wasm
$old_decode  release/alto_decode_bg.wasm
$vector  vector-comparison/alto_wasm_bg.wasm
EOF
cat > "$tmp/sums-new" << EOF
$new_wasm  release/alto_wasm_bg.wasm
$new_decode  release/alto_decode_bg.wasm
$vector  vector-comparison/alto_wasm_bg.wasm
EOF
cat > "$tmp/sums-none" << EOF
0000000000000000000000000000000000000000000000000000000000000000  release/alto_wasm_bg.wasm
0000000000000000000000000000000000000000000000000000000000000000  release/alto_decode_bg.wasm
EOF

python3 - "$tmp/releases.json" \
  "$(uri "$tmp/old-wasm")" "$(uri "$tmp/old-decode")" "$(uri "$tmp/vector")" \
  "$(uri "$tmp/new-wasm")" "$(uri "$tmp/new-decode")" \
  "$(uri "$tmp/bad-wasm")" "$(uri "$tmp/bad-decode")" "$(uri "$tmp/bad-vector")" << 'PY'
import json, sys
out, old_w, old_d, vec, new_w, new_d, bad_w, bad_d, bad_v = sys.argv[1:]
def rel(tag, published, assets, ident):
    return {
        "id": ident,
        "tag_name": tag,
        "draft": False,
        "published_at": published,
        "assets": [{"name": name, "browser_download_url": url} for name, url in assets],
    }
# File order is oldest-first. Selection must follow published_at, not this order.
data = [
    rel("v0.1.0", "2026-01-01T00:00:00Z", [
        ("alto_wasm_bg.wasm", old_w),
        ("alto_decode_bg.wasm", old_d),
        ("vector-comparison-alto_wasm_bg.wasm", vec),
    ], 1),
    rel("v0.2.0", "2026-06-01T00:00:00Z", [
        ("alto_wasm_bg.wasm", new_w),
        ("alto_decode_bg.wasm", new_d),
        ("vector-comparison-alto_wasm_bg.wasm", bad_v),
    ], 2),
    rel("v0.3.0", "2026-10-01T00:00:00Z", [
        ("alto_wasm_bg.wasm", bad_w),
        ("alto_decode_bg.wasm", bad_d),
    ], 3),
    rel("draft", "2026-12-01T00:00:00Z", [
        ("alto_wasm_bg.wasm", new_w),
        ("alto_decode_bg.wasm", new_d),
    ], 9) | {"draft": True},
]
with open(out, "w", encoding="utf-8") as handle:
    json.dump(data, handle)
PY

run_fetch() {
  ALTO_RELEASES_JSON="$tmp/releases.json" \
  ALTO_WASM_SUMS="$1" \
  ALTO_FETCH_ROOT="$2" \
  sh scripts/fetch-wasm.sh ${3:-}
}

# Newest release does not match. The older matching release is installed.
# wasm/RELEASE is not read, so tag=none cannot block this.
out=$tmp/dest-old
log=$tmp/old.out
run_fetch "$tmp/sums-old" "$out" > "$log"
cmp "$tmp/old-wasm" "$out/examples/browser-playground/pkg/alto_wasm_bg.wasm"
cmp "$tmp/old-decode" "$out/packages/alto-sdk/dist/pkg/alto_decode_bg.wasm"
cmp "$tmp/vector" "$out/examples/vector-comparison/pkg/alto_wasm_bg.wasm"
grep -q "installed release WASM from v0.1.0" "$log"

# Two releases match the new hashes except v0.3.0 is newer and does not.
# v0.2.0 wins over the older identical-enough v0.1.0. Comparison hash misses.
out=$tmp/dest-new
log=$tmp/new.out
err=$tmp/new.err
run_fetch "$tmp/sums-new" "$out" > "$log" 2> "$err"
cmp "$tmp/new-wasm" "$out/examples/browser-playground/pkg/alto_wasm_bg.wasm"
cmp "$tmp/new-decode" "$out/packages/alto-sdk/dist/pkg/alto_decode_bg.wasm"
if [ -e "$out/examples/vector-comparison/pkg/alto_wasm_bg.wasm" ]; then
  echo "comparison asset with the wrong hash was installed" >&2
  exit 1
fi
grep -q "installed release WASM from v0.2.0" "$log"
grep -q "comparison WASM was not installed from v0.2.0" "$err"

# Nothing matches: fail closed, and do not create the install tree.
out=$tmp/dest-none
err=$tmp/none.err
if run_fetch "$tmp/sums-none" "$out" > "$tmp/none.out" 2> "$err"; then
  echo "expected fetch-wasm.sh to fail when no release matches" >&2
  exit 1
fi
grep -q "no GitHub Release has release WASM assets matching" "$err"
grep -q "checked 3 release(s)" "$err"
if [ -e "$out/examples/browser-playground/pkg/alto_wasm_bg.wasm" ]; then
  echo "fetch installed WASM after a failed match" >&2
  exit 1
fi

# An explicit tag that does not match fails even though another release would.
err=$tmp/tag.err
if run_fetch "$tmp/sums-new" "$tmp/dest-tag" v0.1.0 > "$tmp/tag.out" 2> "$err"; then
  echo "expected an explicit non-matching tag to fail closed" >&2
  exit 1
fi
grep -q "looked only at tag v0.1.0" "$err"

# The same explicit tag succeeds when its assets match.
out=$tmp/dest-tag-ok
run_fetch "$tmp/sums-old" "$out" v0.1.0 > "$tmp/tag-ok.out"
cmp "$tmp/old-wasm" "$out/examples/browser-playground/pkg/alto_wasm_bg.wasm"

echo "fetch-wasm.sh mock listing tests passed"
