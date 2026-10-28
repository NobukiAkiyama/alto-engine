import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const script = fileURLToPath(new URL("../../../scripts/alpha-measure-summary.mjs", import.meta.url));

test("alpha summary enforces v3 exactly and preserves mismatch and error reports", () => {
  const dir = mkdtempSync(join(tmpdir(), "alto-v3-summary-"));
  try {
    for (const row of [
      { name: "v3-blend-color-alpha-measure", status: "exact", count: 0 },
      { name: "v3-blend-color-alpha-measure", status: "mismatch", count: 1 },
      { name: "v3-blend-color-alpha-measure", status: "error", error: "GPU render failed" },
    ]) {
      const input = join(dir, "parity.json");
      writeFileSync(input, JSON.stringify({
        // v2 mismatches remain measured, without failing the summary command.
        alphaMeasure: [{ name: "blend-color-alpha-measure", count: 1 }],
        compositeV3: [row],
      }));
      const run = spawnSync(process.execPath, [script, "test-backend", input, dir], {
        encoding: "utf8", env: { ...process.env, GITHUB_STEP_SUMMARY: "" },
      });
      assert.equal(run.status, row.status === "exact" ? 0 : 1, run.stderr);
      const report = JSON.parse(readFileSync(join(dir, "composite-v3.json"), "utf8"));
      assert.equal(report.label, "enforced");
      assert.deepEqual(report.cases, [row]);
      const markdown = readFileSync(join(dir, "alpha-measure.md"), "utf8");
      assert.ok(markdown.includes("v3 integer composite (test-backend)"));
      assert.ok(markdown.includes(row.status));
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
