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


test("v3 classification covers every document effect, vector and mask", () => {
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
    ["disabled-mask-normal-adjust", { type: "adjustment", adjustment: { type: "color_adjust", brightness: 0, contrast: 1, saturation: 1 }, props: { mask: {}, mask_enabled: false } }],
    ["blur", { type: "raster", props: { effects: [{ type: "gaussian_blur", radius: 4 }] } }],
    ["vector", { type: "vector", paths: [{ data: { commands: [] }, fill: [1, 0, 0, 1], stroke: null }], props: {} }],
    ["vector-mask", { type: "raster", props: { vector_mask: { path: { commands: [] }, feather: 4, enabled: true } } }],
    ["text", { type: "text", props: {} }],
    ["invisible-strokes", { type: "vector", paths: [
      { data: { commands: [] }, fill: [1, 0, 0, 1], stroke: { color: [0, 0, 0, 1], width: 0 } },
      { data: { commands: [] }, fill: [1, 0, 0, 1], stroke: { color: [0, 0, 0, 2 ** -16], width: 2 } }], props: {} }],
    ["clip-path", { type: "raster", props: { clip_path: { path: { commands: [] }, enabled: true } } }],
    ["invert", { type: "raster", props: { effects: [{ type: "invert" }] } }],
    ["levels-layer", { type: "adjustment", adjustment: { type: "levels" }, props: { blend: "normal" } }],
    ["curves", { type: "raster", props: { effects: [{ type: "curves" }] } }],
    ["hue-saturation", { type: "raster", props: { effects: [{ type: "hue_saturation" }] } }],
    ["multiply-adjust", { type: "adjustment", adjustment: { type: "color_adjust" }, props: { blend: "multiply" } }],
    ["screen-adjust-disabled-mask", { type: "adjustment", adjustment: { type: "color_adjust" }, props: { blend: "screen", mask: {}, mask_enabled: false } }],
    ["vector-masked-adjust", { type: "adjustment", adjustment: { type: "color_adjust" }, props: { vector_mask: { path: { commands: [] }, enabled: true } } }],
    ["clip-path-adjust", { type: "adjustment", adjustment: { type: "gaussian_blur" }, props: { clip_path: { path: { commands: [] }, enabled: true } } }],
    ["stroked-vector", { type: "vector", paths: [{ data: { commands: [] }, fill: null, stroke: { color: [0, 0, 0, 1], width: 2 } }], props: {} }],
    ["clip", { type: "raster", props: { clip_to_below: true } }],
  ]) {
    const scene = { meta: { render_version: 3 }, nodes: new Map([[name, node]]) };
    assert.equal(sceneGap(scene), "v3-m5-integer", name);
  }
  // A raster whose mask is disabled has no mask (Rust `features.mask` is None):
  // it stays on the plain integer tile compositor.
  const disabled = { meta: { render_version: 3 }, nodes: new Map([["off", { type: "raster", props: { mask: {}, mask_enabled: false } }]]) };
  assert.equal(sceneGap(disabled), null);
  // A disabled vector mask or clip path is none (Rust filters on `enabled`).
  const offPath = { meta: { render_version: 3 }, nodes: new Map([["off", { type: "raster", props: {
    vector_mask: { path: { commands: [] }, enabled: false }, clip_path: { path: { commands: [] }, enabled: false } } }]]) };
  assert.equal(sceneGap(offPath), null);
  for (const [name, node] of [
    ["unknown-effect", { type: "raster", props: { effects: [{ type: "future_effect" }] } }],
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
    // An unknown future effect is refused even when another feature uses M5.
    snapshot.nodes.set("first-mask", { type: "raster", props: { mask: {}, effects: [{ type: "future_effect" }] } });
    assert.equal(sceneGap(snapshot), "v3-unsupported");
    snapshot.meta.render_version = 2;
    assert.equal(sceneGap(snapshot), "mask");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
