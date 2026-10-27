// GPU tiles from the production compositor against the f32 CPU reference.
// A mismatch names the first pixel and the number of pixels that differ.
// Paths composite.wgsl does not draw are explicit xfails: they still read a tile,
// and a bit-exact match fails the xfail so it can be removed.
import { unstable } from "../../packages/alto-sdk/dist/unstable.js";
import { testing } from "../../packages/alto-sdk/dist/testing.js";
import { parityCases, renderCase, sceneGap, solid } from "./cpu-composite.js";

const BLENDS = [
  "normal", "multiply", "screen", "overlay", "darken", "lighten", "color_dodge", "color_burn",
  "hard_light", "soft_light", "difference", "exclusion", "hue", "saturation", "color", "luminosity", "add",
];
const assert = (ok, message) => {
  if (!ok) throw Error(message);
};
const same = (a, b) => a.length === b.length && a.every((v, i) => v === b[i]);
function diff(cpu, gpu) {
  let count = 0;
  let at = null;
  const n = Math.min(cpu.length, gpu.length) / 4;
  for (let i = 0; i < n; i++) {
    const o = i * 4;
    if (cpu[o] !== gpu[o] || cpu[o + 1] !== gpu[o + 1] || cpu[o + 2] !== gpu[o + 2] || cpu[o + 3] !== gpu[o + 3]) {
      count++;
      if (!at) at = { x: i % 256, y: (i / 256) | 0, o };
    }
  }
  if (!at) return `0 pixels differ; first none; gpu none; cpu none; lengths cpu ${cpu.length} gpu ${gpu.length}`;
  const o = at.o;
  return `${count} pixels differ; first ${at.x},${at.y}; gpu ${gpu[o]},${gpu[o + 1]},${gpu[o + 2]},${gpu[o + 3]}; cpu ${cpu[o]},${cpu[o + 1]},${cpu[o + 2]},${cpu[o + 3]}; lengths cpu ${cpu.length} gpu ${gpu.length}`;
}
async function imported(engine, image) {
  return testing(engine).request("import", {
    width: image.w,
    height: image.h,
    pixels: image.pixels,
    encoded: new Uint8Array(),
    mime: "image/png",
  });
}
async function addNode(doc, engine, node, parent) {
  if (node.type === "group") {
    let id;
    await doc.transact("group", (tx) => {
      id = tx.addGroup({ isolated: !!node.isolated, parent });
      tx.setProps(id, { opacity: node.opacity, blend: BLENDS[node.mode], visible: node.visible });
      tx.setTransform(id, node.transform);
    });
    for (const child of node.children) await addNode(doc, engine, child, id);
    return;
  }
  const asset = await imported(engine, node.image);
  await doc.transact("raster", (tx) => {
    const id = tx.addRasterFromAsset(asset, { parent });
    tx.setProps(id, { opacity: node.opacity, blend: BLENDS[node.mode], visible: node.visible });
    tx.setTransform(id, node.transform);
  });
}
async function gpuTile(engine, testCase) {
  // The CPU reference composites a level tile directly. `fresh` empties the caches in
  // the Worker step that reads it: a frame drawn after a separate reset could cache the
  // level-0 children first, and the tile would be read as their downsample.
  const raw = testCase.level
    ? await testing(engine).request("readTileLevel", { level: testCase.level, x: testCase.tx, y: testCase.ty, fresh: true })
    : await unstable(engine).readTile(testCase.tx, testCase.ty);
  return new Uint8Array(raw);
}
function square() {
  const image = solid(256, 256, [0, 0, 0, 0]);
  for (let y = 80; y < 180; y++) for (let x = 80; x < 180; x++) image.pixels.set([255, 0, 0, 255], (y * 256 + x) * 4);
  return image;
}

export async function runParityTests(engine, log) {
  const results = [];
  const test = async (name, fn) => {
    const start = performance.now();
    try {
      const detail = await fn();
      const row = { name, pass: true, ms: performance.now() - start, detail: detail || "" };
      results.push(row);
      log(`PASS ${name} ${row.ms.toFixed(1)} ms${detail ? `; ${detail}` : ""}`);
    } catch (error) {
      if (error.xfail) {
        results.push({ name, pass: true, xfail: true, reason: error.reason, ms: performance.now() - start });
        log(`XFAIL ${name}`);
        return;
      }
      results.push({ name, pass: false, error: String(error), ms: performance.now() - start });
      log(`FAIL ${name} ${error}`);
    }
  };
  const xfail = async (name, reason, fn) => {
    const start = performance.now();
    try {
      await fn();
      results.push({ name, pass: true, xfail: true, reason, ms: performance.now() - start });
      log(`XFAIL ${name} ${reason}`);
    } catch (error) {
      results.push({ name, pass: false, error: String(error), ms: performance.now() - start });
      log(`FAIL ${name} ${error}`);
    }
  };
  try {
    for (const testCase of parityCases()) {
      await test(testCase.name, async () => {
        const doc = await engine.createDocument({ width: testCase.w, height: testCase.h });
        for (const node of testCase.nodes) await addNode(doc, engine, node);
        const cpu = renderCase(testCase);
        const gpu = await gpuTile(engine, testCase);
        const gap = diff(cpu, gpu);
        log(`${testCase.name}: ${gap}`);
        assert(same(cpu, gpu), gap);
        return gap;
      });
    }
    await xfail("mask", "mask: rgba coverage, not the plain composite", async () => {
      const image = solid(256, 256, [255, 0, 0, 255]);
      const doc = await engine.createDocument({ width: 256, height: 256 });
      const asset = await imported(engine, image);
      let layer;
      await doc.transact("raster", (tx) => {
        layer = tx.addRasterFromAsset(asset);
      });
      await doc.selection.apply({ kind: "rectangle", x: 0, y: 0, width: 128, height: 256 });
      await doc.masks.fromSelection(layer);
      const gpu = await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
      assert(gpu.length === 262144 && gpu.some((v, i) => i % 4 === 3 && v > 0), "mask produced no ink");
      assert(sceneGap(doc.getSnapshot()) === "mask", sceneGap(doc.getSnapshot()));
      const plain = renderCase({ w: 256, h: 256, level: 0, tx: 0, ty: 0, nodes: [{ type: "raster", image, mode: 0, opacity: 1, visible: true, transform: [1, 0, 0, 1, 0, 0] }] });
      assert(!same(plain, gpu), "mask matched the unmasked composite bit-exactly; remove the xfail");
    });
    await xfail("gaussian-blur", "effects: rgba16float and a Gaussian, not composite.wgsl", async () => {
      const image = square();
      const doc = await engine.createDocument({ width: 256, height: 256 });
      const asset = await imported(engine, image);
      let layer;
      await doc.transact("raster", (tx) => {
        layer = tx.addRasterFromAsset(asset);
      });
      await doc.effects.set(layer, [{ type: "gaussian_blur", radius: 8 }]);
      const gpu = await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
      assert(gpu.length === 262144 && gpu.some((v, i) => i % 4 === 3 && v > 0), "blur produced no ink");
      assert(sceneGap(doc.getSnapshot()) === "effects", sceneGap(doc.getSnapshot()));
      const plain = renderCase({ w: 256, h: 256, level: 0, tx: 0, ty: 0, nodes: [{ type: "raster", image, mode: 0, opacity: 1, visible: true, transform: [1, 0, 0, 1, 0, 0] }] });
      assert(!same(plain, gpu), "blur matched the sharp composite bit-exactly; remove the xfail");
    });
    await xfail("color-adjust", "adjustment: effect pass, not composite.wgsl", async () => {
      const image = solid(256, 256, [40, 80, 120, 255]);
      const doc = await engine.createDocument({ width: 256, height: 256 });
      const asset = await imported(engine, image);
      await doc.transact("adjust", (tx) => {
        tx.addRasterFromAsset(asset);
        tx.addAdjustment({ type: "color_adjust", brightness: 0.6 });
      });
      const gpu = await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
      assert(gpu.length === 262144 && gpu.some((v, i) => i % 4 === 3 && v > 0), "adjustment produced no ink");
      assert(sceneGap(doc.getSnapshot()) === "adjustment", sceneGap(doc.getSnapshot()));
      const plain = renderCase({ w: 256, h: 256, level: 0, tx: 0, ty: 0, nodes: [{ type: "raster", image, mode: 0, opacity: 1, visible: true, transform: [1, 0, 0, 1, 0, 0] }] });
      assert(!same(plain, gpu), "color adjust matched the source bit-exactly; remove the xfail");
    });
    await xfail("vector-fill", "vector: 4x MSAA mesh, not composite.wgsl", async () => {
      const doc = await engine.createDocument({ width: 256, height: 256 });
      const data = await engine.vectors.parseSvg("M32 32 H200 V200 H32 Z");
      await doc.transact("vector", (tx) => tx.addVector([{ data, fill: [1, 0, 0, 1], stroke: null, fill_rule: "non_zero" }]));
      const gpu = await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
      assert(gpu.length === 262144 && gpu.some((v, i) => i % 4 === 3 && v > 0), "vector produced no ink");
      assert(sceneGap(doc.getSnapshot()) === "vector", sceneGap(doc.getSnapshot()));
      const empty = new Uint8Array(262144);
      assert(!same(empty, gpu), "vector matched an empty composite bit-exactly; remove the xfail");
    });
    await xfail("text", "text: glyph atlas or vector outlines, no separate CPU pixel reference", async () => {
      const doc = await engine.createDocument({ width: 256, height: 256 });
      const font = await engine.assets.importFont(await (await fetch("../../tests/fonts/AltoTestLatin.ttf")).blob());
      await doc.transact("text", (tx) => {
        const id = tx.addFont(font);
        const node = tx.addText({ text: "Ag", style: { fonts: [id], size: 72, color: [0, 0, 0, 1] }, writing_mode: "horizontal_tb", frame: { kind: "point" } });
        tx.setTransform(node, [1, 0, 0, 1, 24, 80]);
      });
      const gpu = await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
      assert(gpu.length === 262144 && gpu.some((v, i) => i % 4 === 3 && v > 0), "text produced no ink");
      assert(sceneGap(doc.getSnapshot()) === "text", sceneGap(doc.getSnapshot()));
      assert(!same(new Uint8Array(262144), gpu), "text matched an empty composite bit-exactly; remove the xfail");
    });
    await test("render upgrade is one undoable edit and load or save keeps version 1", async () => {
      const response = await fetch("../../tests/fixtures/render-v1-bilinear.alto");
      assert(response.ok, `fixture status ${response.status}`);
      let doc = await engine.open(new Blob([await response.arrayBuffer()]));
      const opened = doc.getSnapshot().meta;
      assert(opened.render_version == null || opened.render_version === 1, `opened at ${opened.render_version}`);
      assert(opened.format_version === 1, `opened format ${opened.format_version}`);
      const version1 = await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
      const saved = await testing(engine).saveBytes();
      doc = await engine.open(new Blob([saved]));
      const resaved = doc.getSnapshot().meta;
      assert(resaved.render_version == null || resaved.render_version === 1, "save upgraded the document");
      assert(resaved.format_version === 1, `save wrote format ${resaved.format_version}`);
      assert(same(version1, await gpuTile(engine, { level: 0, tx: 0, ty: 0 })), "reopened version 1 pixels changed");
      await doc.upgradeRenderVersion();
      const upgraded = doc.getSnapshot().meta;
      assert(upgraded.render_version === 2 && upgraded.format_version === 3, `upgrade meta ${upgraded.render_version} format ${upgraded.format_version}`);
      const version2 = await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
      const cpu = renderCase(parityCases().find((item) => item.name === "scale-bilinear"));
      const gap = diff(cpu, version2);
      log(`render-upgrade: ${gap}`);
      assert(same(cpu, version2), gap);
      assert(!same(version1, version2), "upgrade left the version 1 pixels");
      let rejected = false;
      try {
        await doc.upgradeRenderVersion();
      } catch (error) {
        rejected = String(error).includes("already current");
      }
      assert(rejected, "a second upgrade was accepted");
      assert(doc.getSnapshot().history.undo === 1, `undo depth ${doc.getSnapshot().history.undo}`);
      await doc.undo();
      const restored = doc.getSnapshot().meta;
      assert(restored.render_version == null || restored.render_version === 1, `undo left ${restored.render_version}`);
      assert(same(version1, await gpuTile(engine, { level: 0, tx: 0, ty: 0 })), "undo did not restore version 1 pixels");
    });
    await test("deferred present stops instead of waiting forever", async () => {
      await engine.createDocument({ width: 64, height: 64 });
      let lost = false;
      let restored = false;
      const off = unstable(engine).onGpu((event) => {
        if (event === "lost") lost = true;
        if (event === "restored") restored = true;
      });
      await testing(engine).request("stickPresent", { enabled: true, limit: 3 });
      let code = "";
      try {
        await testing(engine).request("waitFrame");
      } catch (error) {
        code = error.code;
      }
      off();
      assert(code === "GPU_LOST", `waitFrame code ${code}`);
      assert(lost && restored, `gpu events lost=${lost} restored=${restored}`);
      const tile = await unstable(engine).readTile(0, 0);
      assert(tile.length === 262144, "readTile after recovery");
    });
  } finally {
    document.querySelector("#test-results").dataset.report = JSON.stringify({
      date: new Date().toISOString(),
      milestone: "gpu-cpu-parity",
      userAgent: navigator.userAgent,
      results,
    });
  }
  const failed = results.filter((result) => result.pass === false);
  if (failed.length) throw Error(failed.map((result) => `${result.name}: ${result.error}`).join("\n"));
  log(`parity results ${results.length} failed 0`);
}
