import { mkdir, cp } from "node:fs/promises";
import { execFileSync } from "node:child_process";
// The Workers import the WASM package next to them; their types come from it too.
await mkdir("dist/pkg", { recursive: true });
await cp("../../examples/browser-playground/pkg", "dist/pkg", { recursive: true });
const tsc = (project) => execFileSync("node_modules/.bin/tsc", ["-p", project], { stdio: "inherit" });
tsc("tsconfig.json");
tsc("tsconfig.worker.json");
