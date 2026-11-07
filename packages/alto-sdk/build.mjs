import { access, mkdir, cp } from "node:fs/promises";
import { execFileSync } from "node:child_process";
// The Workers import the WASM package next to them; their types come from it too.
// dist/ and the WASM package are gitignored build products. build-m0.sh puts the
// package in place before this build, and `files: ["dist"]` packs it on publish.
const pkg = "../../examples/browser-playground/pkg";
try {
  await access(`${pkg}/alto_wasm_bg.wasm`);
  await access(`${pkg}/alto_decode_bg.wasm`);
  await access(`${pkg}/alto_wasm.js`);
  await access(`${pkg}/alto_decode.js`);
} catch {
  console.error("WASM package is missing. From the repo root, run sh scripts/build-m0.sh");
  process.exit(1);
}
await mkdir("dist/pkg", { recursive: true });
await cp(pkg, "dist/pkg", { recursive: true });
await access("dist/pkg/alto_wasm_bg.wasm");
await access("dist/pkg/alto_decode_bg.wasm");
const tsc = (project) => execFileSync("node_modules/.bin/tsc", ["-p", project], { stdio: "inherit" });
tsc("tsconfig.json");
tsc("tsconfig.worker.json");
