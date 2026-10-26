import { access, mkdir, cp } from "node:fs/promises";
import { execFileSync } from "node:child_process";
// The Workers import the WASM package next to them; their types come from it too.
// The .wasm files are gitignored. fetch-wasm.sh or build-m0.sh puts them here
// before this build, and `files: ["dist"]` packs them on publish.
const pkg = "../../examples/browser-playground/pkg";
try {
  await access(`${pkg}/alto_wasm_bg.wasm`);
  await access(`${pkg}/alto_decode_bg.wasm`);
} catch {
  console.error("WASM package is missing. From the repo root, run sh scripts/fetch-wasm.sh or sh scripts/build-m0.sh");
  process.exit(1);
}
await mkdir("dist/pkg", { recursive: true });
await cp(pkg, "dist/pkg", { recursive: true });
await access("dist/pkg/alto_wasm_bg.wasm");
await access("dist/pkg/alto_decode_bg.wasm");
const tsc = (project) => execFileSync("node_modules/.bin/tsc", ["-p", project], { stdio: "inherit" });
tsc("tsconfig.json");
tsc("tsconfig.worker.json");
