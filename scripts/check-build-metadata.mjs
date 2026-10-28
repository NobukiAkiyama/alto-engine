import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { gzipSync } from "node:zlib";

let failures = 0;
const check = (actual, expected, message) => {
  try { assert.deepEqual(actual, expected, message); }
  catch (error) { console.error(error.message); failures++; }
};
const fast = process.argv.includes("--fast");
const releaseDir = process.env.ALTO_METADATA_RELEASE_DIR;
const metadata = JSON.parse(readFileSync("verification/build.json", "utf8"));
const sums = readFileSync("wasm/SHA256SUMS", "utf8");
const recorded = (name) => {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = sums.match(new RegExp(`^([0-9a-f]{64})  ${escaped}$`, "m"));
  if (!match) { console.error(`wasm/SHA256SUMS is missing ${name}`); failures++; }
  return match?.[1];
};
const playground = readFileSync(`${releaseDir || "examples/browser-playground/pkg"}/alto_wasm_bg.wasm`);
const sdk = readFileSync(`${releaseDir || "packages/alto-sdk/dist/pkg"}/alto_wasm_bg.wasm`);
const hash = createHash("sha256").update(playground).digest("hex");


check(sdk, playground, "the two shipped WASM copies differ");
if (!fast) {
  const rustcInfo = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const buildHost = rustcInfo.match(/^host: (.+)$/m)?.[1];
  check(metadata.buildHost, buildHost, "build.json buildHost differs from this builder");
  check(metadata.wasmBytes, playground.length, "build.json wasmBytes is stale");
  check(metadata.wasmSha256, hash, "build.json wasmSha256 is stale");
  check(recorded("release/alto_wasm_bg.wasm"), hash, "wasm/SHA256SUMS release hash is stale");
  check(
    recorded("vector-comparison/alto_wasm_bg.wasm"),
    metadata.comparisonWasm.sha256,
    "wasm/SHA256SUMS comparison hash does not match build.json",
  );
  check(
    metadata.gzipBytes,
    gzipSync(playground).length,
    "build.json gzipBytes is stale (Node zlib default gzip)",
  );
  if (releaseDir) {
    const comparison = readFileSync(`${releaseDir}/vector/alto_wasm_bg.wasm`);
    check(metadata.comparisonWasm.bytes, comparison.length, "build.json comparisonWasm.bytes is stale");
    check(metadata.comparisonWasm.gzipBytes, gzipSync(comparison).length, "build.json comparisonWasm.gzipBytes is stale");
    check(metadata.comparisonWasm.sha256, createHash("sha256").update(comparison).digest("hex"), "build.json comparisonWasm.sha256 is stale");
  }
}
if (failures) process.exit(1);
console.log(`WASM shipped copies match${fast ? " (release metadata report-only)" : " recorded metadata"}: ${hash}`);
