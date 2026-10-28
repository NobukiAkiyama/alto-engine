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


test("v3 classification is closed: color-adjust is integer M5, unspecified features are refused", () => {
  const snapshot = {
    meta: { render_version: 3 },
    nodes: new Map([
      ["bottom", { type: "raster", props: { opacity: 0.5, transform: [1, 0, 0, 1, 0, 0] } }],
      ["top", { type: "raster", props: { opacity: 0.5, transform: [1, 0, 0, 1, 0, 0],
        effects: [{ type: "color_adjust", brightness: 0.2, contrast: 0, saturation: 0 }] } }],
    ]),
  };
  const renderPath = sceneGap(snapshot);
  assert.equal(renderPath, "v3-m5-integer");
  assert.equal(isV3IntegerCase({ renderPath }), true);
  const plain = { meta: { render_version: 3 }, nodes: new Map([["only", { type: "raster", props: {} }]]) };
  assert.equal(sceneGap(plain), null);
  const adjust = { meta: { render_version: 3 }, nodes: new Map([
    ["adj", { type: "adjustment", adjustment: { type: "color_adjust", brightness: 0, contrast: 1, saturation: 1 }, props: { blend: "normal", opacity: 0.5 } }],
  ]) };
  assert.equal(sceneGap(adjust), "v3-m5-integer");
  for (const [name, node] of [
    ["selected", { type: "raster", props: { effects: [{ type: "color_adjust", brightness: 0, contrast: 1, saturation: 1, selection: { tiles: {} } }] } }],
    ["masked-multiply-adjust", { type: "adjustment", adjustment: { type: "color_adjust", brightness: 0, contrast: 1, saturation: 1 }, props: { blend: "multiply", mask: {} } }],
    ["mask", { type: "raster", props: { mask: {} } }],
  ]) {
    const scene = { meta: { render_version: 3 }, nodes: new Map([[name, node]]) };
    assert.equal(sceneGap(scene), "v3-m5-integer", name);
  }
  for (const [name, node] of [
    ["blur", { type: "raster", props: { effects: [{ type: "gaussian_blur", radius: 4 }] } }],
    ["levels-layer", { type: "adjustment", adjustment: { type: "invert" }, props: { blend: "normal" } }],
    // Unmasked non-normal adjustment blends stay refused until the v3 float purge.
    ["multiply-adjust", { type: "adjustment", adjustment: { type: "color_adjust", brightness: 0, contrast: 1, saturation: 1 }, props: { blend: "multiply" } }],
    ["screen-adjust-disabled-mask", { type: "adjustment", adjustment: { type: "color_adjust", brightness: 0, contrast: 1, saturation: 1 }, props: { blend: "screen", mask: {}, mask_enabled: false } }],
    ["vector", { type: "vector", props: {} }],
    ["text", { type: "text", props: {} }],
    ["clip", { type: "raster", props: { clip_to_below: true } }],
  ]) {
    const scene = { meta: { render_version: 3 }, nodes: new Map([[name, node]]) };
    assert.equal(sceneGap(scene), "v3-unsupported", name);
  }
  const dir = mkdtempSync(join(tmpdir(), "alto-v3-unsupported-"));
  try {
    const row = { name: "v3-masked", renderPath: "v3-unsupported", status: "error", error: "refused" };
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
    assert.deepEqual(report.v3Unsupported, [row]);
    assert.ok(readFileSync(join(dir, "alpha-measure.md"), "utf8").includes("v3-unsupported"));
    // Classification must scan the entire scene and preserve pre-v3 gap names.
    // A mask alone is integer M5; combined with blur it is still refused, never float-drawn.
    snapshot.nodes.set("first-mask", { type: "raster", props: { mask: {}, effects: [{ type: "gaussian_blur", radius: 4 }] } });
    assert.equal(sceneGap(snapshot), "v3-unsupported");
    snapshot.meta.render_version = 2;
    assert.equal(sceneGap(snapshot), "mask");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
