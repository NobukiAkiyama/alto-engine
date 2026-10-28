// CI output is a release proposal, never a render-hash baseline.
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync, mkdirSync, appendFileSync } from "node:fs";
import { gzipSync } from "node:zlib";

const out = process.argv[2];
mkdirSync(out, { recursive: true });
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const pkg = process.argv[3];
const dirs = { release: pkg || "examples/browser-playground/pkg", "vector-comparison": pkg ? `${pkg}/vector` : "examples/vector-comparison/pkg" };
const files = {};
const proposed = [];
const differences = [];
for (const line of readFileSync("wasm/SHA256SUMS", "utf8").split("\n")) {
  if (line.startsWith("#")) { proposed.push(line); continue; }
  if (!line.trim()) continue;
  const match = line.match(/^([0-9a-f]{64})  ((release|vector-comparison)\/[^/]+)$/);
  const missing = (path) => {
    files[path] = "missing";
    proposed.push(`missing  ${path}`);
    differences.push(`-${line}`, `+missing  ${path}`);
  };
  if (!match) { missing(line); continue; }
  const [, expected, path, group] = match;
  let bytes;
  try { bytes = readFileSync(`${dirs[group]}/${path.slice(group.length + 1)}`); }
  catch { missing(path); continue; }
  const actual = sha(bytes);
  files[path] = { bytes: bytes.length, gzipBytes: gzipSync(bytes).length, sha256: actual };
  proposed.push(`${actual}  ${path}`);
  if (expected !== actual) differences.push(`-${expected}  ${path}`, `+${actual}  ${path}`);
}
const release = files["release/alto_wasm_bg.wasm"];
const fields = { wasmBytes: release?.bytes ?? "missing", gzipBytes: release?.gzipBytes ?? "missing", wasmSha256: release?.sha256 ?? "missing",
  comparisonWasm: files["vector-comparison/alto_wasm_bg.wasm"] ?? "missing" };
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
