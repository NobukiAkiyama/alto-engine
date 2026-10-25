import { mkdir, copyFile, cp } from "node:fs/promises";
await mkdir("dist/pkg", { recursive: true });
for (const f of [
  "worker.js",
  "storage-worker.js",
  "storage-core.js",
  "storage-session.js",
  "decode-worker.js",
])
  await copyFile("src/" + f, "dist/" + f);
await cp("../../examples/browser-playground/pkg", "dist/pkg", {
  recursive: true,
});
