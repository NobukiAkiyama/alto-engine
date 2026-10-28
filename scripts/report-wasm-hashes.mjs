// Cached CI output is a release proposal, never a render-hash baseline.
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync, mkdirSync, appendFileSync } from "node:fs";
import { gzipSync } from "node:zlib";

const out = process.argv[2];
mkdirSync(out, { recursive: true });
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const dirs = { release: "examples/browser-playground/pkg", "vector-comparison": "examples/vector-comparison/pkg" };
const files = {};
const proposed = [];
const differences = [];
for (const line of readFileSync("wasm/SHA256SUMS", "utf8").split("\n")) {
  if (!line || line.startsWith("#")) continue;
  const [, expected, path] = line.match(/^([0-9a-f]{64})  (.+)$/);
  const slash = path.indexOf("/");
  const bytes = readFileSync(`${dirs[path.slice(0, slash)]}/${path.slice(slash + 1)}`);
  const actual = sha(bytes);
  files[path] = { bytes: bytes.length, gzipBytes: gzipSync(bytes).length, sha256: actual };
  proposed.push(`${actual}  ${path}`);
  if (expected !== actual) differences.push(`-${expected}  ${path}`, `+${actual}  ${path}`);
}
const release = files["release/alto_wasm_bg.wasm"];
const fields = { wasmBytes: release.bytes, gzipBytes: release.gzipBytes, wasmSha256: release.sha256,
  comparisonWasm: files["vector-comparison/alto_wasm_bg.wasm"] };
const committed = JSON.parse(readFileSync("verification/build.json", "utf8"));
for (const [key, actual] of Object.entries(fields)) {
  if (JSON.stringify(committed[key]) !== JSON.stringify(actual)) {
    differences.push(`-build.json ${key}: ${JSON.stringify(committed[key])}`, `+build.json ${key}: ${JSON.stringify(actual)}`);
  }
}
writeFileSync(`${out}/SHA256SUMS`, proposed.join("\n") + "\n");
writeFileSync(`${out}/build-fields.json`, JSON.stringify(fields, null, 2) + "\n");
writeFileSync(`${out}/sizes.json`, JSON.stringify(files, null, 2) + "\n");
const summary = "### Release-binary hashes (report only)\n\nUnrelated to render hashes. Update committed values once at release on arm64 macOS.\n\n```diff\n" +
  (differences.join("\n") || "No differences from committed values.") + "\n```\n";
console.log(summary);
if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, summary);
