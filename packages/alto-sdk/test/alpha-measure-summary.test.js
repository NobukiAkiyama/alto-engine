import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

import { sceneGap, isV3IntegerCase } from "../../../tests/e2e/cpu-composite.js";

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


test("v3 overlapping semi-transparent layers with color-adjust are reported outside integer-exact enforcement", () => {
  const snapshot = {
    meta: { render_version: 3 },
    nodes: new Map([
      ["bottom", { type: "raster", props: { opacity: 0.5, transform: [1, 0, 0, 1, 0, 0] } }],
      ["top", { type: "raster", props: { opacity: 0.5, transform: [1, 0, 0, 1, 0, 0],
        effects: [{ type: "color_adjust", brightness: 0.2, contrast: 0, saturation: 0 }] } }],
    ]),
  };
  const renderPath = sceneGap(snapshot);
  assert.equal(renderPath, "v3-m5-float");
  const dir = mkdtempSync(join(tmpdir(), "alto-v3-fallback-"));
  try {
    // Even a byte-exact fallback is excluded; a mismatch is informational too.
    for (const count of [0, 1]) {
      const row = { name: "v3-overlapping-color-adjust", renderPath, status: count ? "mismatch" : "exact", count };
      assert.equal(isV3IntegerCase(row), false);
      const input = join(dir, "parity.json");
      writeFileSync(input, JSON.stringify({ alphaMeasure: [{ name: "v2", count: 0 }], compositeV3: [row] }));
      const run = spawnSync(process.execPath, [script, "test-backend", input, dir], {
        encoding: "utf8", env: { ...process.env, GITHUB_STEP_SUMMARY: "" },
      });
      assert.equal(run.status, 0, run.stderr);
      const report = JSON.parse(readFileSync(join(dir, "composite-v3.json"), "utf8"));
      assert.equal(report.integerExactCount, 0);
      assert.deepEqual(report.cases, []);
      assert.deepEqual(report.v3M5Float, [row]);
      assert.ok(readFileSync(join(dir, "alpha-measure.md"), "utf8").includes("v3-m5-float"));
    }
    // Classification must scan the entire scene and preserve pre-v3 gap names.
    snapshot.nodes.set("first-mask", { type: "raster", props: { mask: {} } });
    assert.equal(sceneGap(snapshot), "v3-m5-float");
    snapshot.meta.render_version = 2;
    assert.equal(sceneGap(snapshot), "mask");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
