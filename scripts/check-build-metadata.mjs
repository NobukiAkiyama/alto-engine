import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { gzipSync } from "node:zlib";

const metadata = JSON.parse(readFileSync("verification/build.json", "utf8"));
const playground = readFileSync("examples/browser-playground/pkg/alto_wasm_bg.wasm");
const sdk = readFileSync("packages/alto-sdk/dist/pkg/alto_wasm_bg.wasm");
const hash = createHash("sha256").update(playground).digest("hex");
const rustcInfo = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
const buildHost = rustcInfo.match(/^host: (.+)$/m)?.[1];

assert.equal(metadata.buildHost, buildHost, "build.json buildHost differs from this builder");
assert.deepEqual(sdk, playground, "the two shipped WASM copies differ");
assert.equal(metadata.wasmBytes, playground.length, "build.json wasmBytes is stale");
assert.equal(metadata.wasmSha256, hash, "build.json wasmSha256 is stale");
assert.equal(
  metadata.gzipBytes,
  gzipSync(playground).length,
  "build.json gzipBytes is stale (Node zlib default gzip)",
);
console.log(`WASM build metadata matches both shipped copies: ${hash}`);
