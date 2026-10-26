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
bad_vector=$(hash_file "$tmp/bad-wasm")

old_wasm=$(hash_file "$tmp/old-wasm")
old_decode=$(hash_file "$tmp/old-decode")
new_wasm=$(hash_file "$tmp/new-wasm")
new_decode=$(hash_file "$tmp/new-decode")
vector=$(hash_file "$tmp/vector")
bad_wasm=$(hash_file "$tmp/bad-wasm")
bad_decode=$(hash_file "$tmp/bad-decode")

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

python3 - "$tmp" \
  "$old_wasm" "$old_decode" "$new_wasm" "$new_decode" "$vector" \
  "$bad_wasm" "$bad_decode" "$bad_vector" << 'PY'
import json, pathlib, sys
tmp = pathlib.Path(sys.argv[1])
old_w, old_d, new_w, new_d, vec, bad_w, bad_d, bad_vec = sys.argv[2:]

def uri(path):
    return pathlib.Path(path).resolve().as_uri()

def write_sums(name, pairs):
    path = tmp / name
    path.write_text("".join(f"{digest}  {asset}\n" for asset, digest in pairs), encoding="utf-8")
    return uri(path)

sums = {
    "old": write_sums("asset-old.sums", [
        ("alto_wasm_bg.wasm", old_w),
        ("alto_decode_bg.wasm", old_d),
        ("vector-comparison-alto_wasm_bg.wasm", vec),
    ]),
    "new": write_sums("asset-new.sums", [
        ("alto_wasm_bg.wasm", new_w),
        ("alto_decode_bg.wasm", new_d),
        ("vector-comparison-alto_wasm_bg.wasm", bad_vec),
    ]),
    "bad": write_sums("asset-bad.sums", [
        ("alto_wasm_bg.wasm", bad_w),
        ("alto_decode_bg.wasm", bad_d),
    ]),
    "liar": write_sums("asset-liar.sums", [
        ("alto_wasm_bg.wasm", old_w),
        ("alto_decode_bg.wasm", old_d),
    ]),
}
missing = uri(tmp / "missing-wasm")

def rel(tag, published, assets, ident):
    return {
        "id": ident,
        "tag_name": tag,
        "draft": False,
        "published_at": published,
        "assets": [{"name": name, "browser_download_url": url} for name, url in assets],
    }

# File order is oldest-first. Selection must follow published_at, not this order.
# v0.3.0 is newest and does not match; its WASM URL is missing so a body
# download would fail. v0.2.5 advertises the old hashes but the bytes differ.
data = [
    rel("v0.1.0", "2026-01-01T00:00:00Z", [
        ("SHA256SUMS", sums["old"]),
        ("alto_wasm_bg.wasm", uri(tmp / "old-wasm")),
        ("alto_decode_bg.wasm", uri(tmp / "old-decode")),
        ("vector-comparison-alto_wasm_bg.wasm", uri(tmp / "vector")),
    ], 1),
    rel("v0.2.0", "2026-06-01T00:00:00Z", [
        ("SHA256SUMS", sums["new"]),
        ("alto_wasm_bg.wasm", uri(tmp / "new-wasm")),
        ("alto_decode_bg.wasm", uri(tmp / "new-decode")),
        ("vector-comparison-alto_wasm_bg.wasm", missing),
    ], 2),
    rel("v0.2.5", "2026-08-01T00:00:00Z", [
        ("SHA256SUMS", sums["liar"]),
        ("alto_wasm_bg.wasm", uri(tmp / "bad-wasm")),
        ("alto_decode_bg.wasm", uri(tmp / "bad-decode")),
    ], 4),
    rel("v0.3.0", "2026-10-01T00:00:00Z", [
        ("SHA256SUMS", sums["bad"]),
        ("alto_wasm_bg.wasm", missing),
        ("alto_decode_bg.wasm", missing),
    ], 3),
    rel("draft", "2026-12-01T00:00:00Z", [
        ("SHA256SUMS", sums["new"]),
        ("alto_wasm_bg.wasm", uri(tmp / "new-wasm")),
        ("alto_decode_bg.wasm", uri(tmp / "new-decode")),
    ], 9) | {"draft": True},
]
(tmp / "releases.json").write_text(json.dumps(data), encoding="utf-8")
page_match = [rel("v1.0.0", "2026-10-02T00:00:00Z", [
    ("SHA256SUMS", sums["new"]),
    ("alto_wasm_bg.wasm", uri(tmp / "new-wasm")),
    ("alto_decode_bg.wasm", uri(tmp / "new-decode")),
], 10)]
page_miss = [rel("v0.9.0", "2026-09-01T00:00:00Z", [
    ("SHA256SUMS", sums["bad"]),
    ("alto_wasm_bg.wasm", missing),
    ("alto_decode_bg.wasm", missing),
], 8)]
(tmp / "page-match.json").write_text(json.dumps(page_match), encoding="utf-8")
(tmp / "page-miss.json").write_text(json.dumps(page_miss), encoding="utf-8")
(tmp / "page-hit.json").write_text(json.dumps(page_match), encoding="utf-8")
PY

run_fetch() {
  ALTO_RELEASES_JSON="$tmp/releases.json" \
  ALTO_WASM_SUMS="$1" \
  ALTO_FETCH_ROOT="$2" \
  sh scripts/fetch-wasm.sh ${3:-}
}

# Newest release does not match, so its missing WASM body is never fetched.
# A newer release whose checksum file matches but whose bytes do not is skipped.
# wasm/RELEASE is not read, so tag=none cannot block this.
out=$tmp/dest-old
log=$tmp/old.out
err=$tmp/old.err
run_fetch "$tmp/sums-old" "$out" > "$log" 2> "$err"
cmp "$tmp/old-wasm" "$out/examples/browser-playground/pkg/alto_wasm_bg.wasm"
cmp "$tmp/old-decode" "$out/packages/alto-sdk/dist/pkg/alto_decode_bg.wasm"
cmp "$tmp/vector" "$out/examples/vector-comparison/pkg/alto_wasm_bg.wasm"
grep -q "installed release WASM from v0.1.0" "$log"
grep -q "checksum v0.3.0" "$log"
if grep -q "fetch release WASM from v0.3.0" "$log"; then
  echo "downloaded WASM for a release whose SHA256SUMS did not match" >&2
  exit 1
fi
grep -q "fetch release WASM from v0.2.5" "$log"
grep -q "WASM bytes did not" "$err"
grep -q "fetch comparison WASM from v0.1.0" "$log"

# v0.2.0 is the newest real match. Its comparison checksum does not match,
# so the missing comparison body is not downloaded.
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
if grep -q "fetch comparison WASM from v0.2.0" "$log"; then
  echo "downloaded comparison WASM after its checksum missed" >&2
  exit 1
fi
if grep -q "fetch release WASM from v0.2.5" "$log"; then
  echo "downloaded WASM after the release checksum missed" >&2
  exit 1
fi

# Nothing matches: fail closed, and do not download or install WASM bodies.
out=$tmp/dest-none
err=$tmp/none.err
log=$tmp/none.out
if run_fetch "$tmp/sums-none" "$out" > "$log" 2> "$err"; then
  echo "expected fetch-wasm.sh to fail when no release matches" >&2
  exit 1
fi
grep -q "no GitHub Release has release WASM assets matching" "$err"
grep -q "checked 4 release(s)" "$err"
if grep -q "fetch release WASM from" "$log"; then
  echo "downloaded a WASM body when no checksum matched" >&2
  exit 1
fi
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

# Stand-in GitHub API. Page 2 returns 500, so a second request fails the test.
cat > "$tmp/api-server.py" << 'PY'
import os
import sys
import traceback

port_path, hits_path, page1_path, page2_path = sys.argv[1:5]
try:
    import socket
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    from pathlib import Path
except Exception:
    traceback.print_exc()
    sys.exit(1)

class BoundServer(ThreadingHTTPServer):
    # HTTPServer.server_bind calls socket.getfqdn, which waits on a reverse
    # DNS lookup. macos-15 runners often have no answer for 127.0.0.1.
    def server_bind(self):
        self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.socket.bind(self.server_address)
        self.server_address = self.socket.getsockname()
        self.server_name = "127.0.0.1"
        self.server_port = self.server_address[1]

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        with open(hits_path, "a", encoding="utf-8") as handle:
            handle.write(self.path + "\n")
        expected = os.environ.get("EXPECT_TOKEN")
        if expected and self.headers.get("Authorization") != "Bearer " + expected:
            self.send_response(401)
            self.end_headers()
            return
        if self.path.startswith("/repos/") and "page=2" not in self.path:
            body = Path(page1_path).read_bytes()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            if os.environ.get("SEND_NEXT") == "1":
                link = f'<http://{self.headers.get("Host")}/repos/fixture/alto/releases?per_page=100&page=2>; rel="next"'
                self.send_header("Link", link)
            self.end_headers()
            self.wfile.write(body)
            return
        if "page=2" in self.path:
            if os.environ.get("PAGE2_FAIL") == "1":
                self.send_response(500)
                self.end_headers()
                return
            body = Path(page2_path).read_bytes()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_response(404)
        self.end_headers()

    def log_message(self, fmt, *args):
        return

try:
    server = BoundServer(("127.0.0.1", 0), Handler)
    Path(port_path).write_text(str(server.server_address[1]), encoding="utf-8")
except Exception:
    traceback.print_exc()
    sys.exit(1)
server.serve_forever()
PY

start_api() {
  rm -f "$tmp/port" "$tmp/hits" "$tmp/api.err"
  EXPECT_TOKEN="$1" SEND_NEXT="$2" PAGE2_FAIL="$3" \
    python3 -u "$tmp/api-server.py" "$tmp/port" "$tmp/hits" "$4" "$5" > "$tmp/api.err" 2>&1 &
  echo $! > "$tmp/api.pid"
  i=0
  while [ ! -s "$tmp/port" ]; do
    if ! kill -0 "$(cat "$tmp/api.pid")" 2>/dev/null; then
      echo "API stand-in exited before listening" >&2
      cat "$tmp/api.err" >&2 || true
      exit 1
    fi
    i=$((i + 1))
    if [ "$i" -gt 200 ]; then
      echo "API stand-in did not start" >&2
      cat "$tmp/api.err" >&2 || true
      exit 1
    fi
    sleep 0.1
  done
}
stop_api() {
  if [ -f "$tmp/api.pid" ]; then
    kill "$(cat "$tmp/api.pid")" 2>/dev/null || true
    wait "$(cat "$tmp/api.pid")" 2>/dev/null || true
    rm -f "$tmp/api.pid"
  fi
}
trap 'stop_api; rm -rf "$tmp"' EXIT

# The first page matches, so the next page (which would fail) is not requested.
start_api fixture-token 1 1 "$tmp/page-match.json" "$tmp/page-miss.json"
port=$(cat "$tmp/port")
out=$tmp/dest-api-one
GITHUB_TOKEN=fixture-token \
ALTO_RELEASES_API="http://127.0.0.1:${port}" \
ALTO_RELEASE_REPO=fixture/alto \
ALTO_WASM_SUMS="$tmp/sums-new" \
ALTO_FETCH_ROOT="$out" \
  sh scripts/fetch-wasm.sh > "$tmp/api-one.out" 2> "$tmp/api-one.err"
cmp "$tmp/new-wasm" "$out/examples/browser-playground/pkg/alto_wasm_bg.wasm"
grep -q "GitHub API requests: 1" "$tmp/api-one.err"
hits=$(wc -l < "$tmp/hits" | tr -d ' ')
if [ "$hits" -ne 1 ]; then
  echo "expected 1 API request, got $hits" >&2
  exit 1
fi
stop_api

# The first page does not match. The second page does, and the token is required.
start_api fixture-token 1 0 "$tmp/page-miss.json" "$tmp/page-hit.json"
port=$(cat "$tmp/port")
out=$tmp/dest-api-two
GITHUB_TOKEN=fixture-token \
ALTO_RELEASES_API="http://127.0.0.1:${port}" \
ALTO_RELEASE_REPO=fixture/alto \
ALTO_WASM_SUMS="$tmp/sums-new" \
ALTO_FETCH_ROOT="$out" \
  sh scripts/fetch-wasm.sh > "$tmp/api-two.out" 2> "$tmp/api-two.err"
cmp "$tmp/new-wasm" "$out/examples/browser-playground/pkg/alto_wasm_bg.wasm"
grep -q "GitHub API requests: 2" "$tmp/api-two.err"
grep -q "checksum v0.9.0" "$tmp/api-two.out"
if grep -q "fetch release WASM from v0.9.0" "$tmp/api-two.out"; then
  echo "downloaded WASM from the non-matching API page" >&2
  exit 1
fi
hits=$(wc -l < "$tmp/hits" | tr -d ' ')
if [ "$hits" -ne 2 ]; then
  echo "expected 2 API requests, got $hits" >&2
  exit 1
fi
stop_api

echo "fetch-wasm.sh mock listing tests passed"
