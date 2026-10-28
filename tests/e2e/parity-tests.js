// GPU tiles from the production compositor against the f32 CPU reference.
// A mismatch names the first pixel and the number of pixels that differ.
// Paths composite.wgsl does not draw are explicit xfails: they still read a tile,
// and a bit-exact match fails the xfail so it can be removed.
import { unstable } from "../../packages/alto-sdk/dist/unstable.js";
import { testing } from "../../packages/alto-sdk/dist/testing.js";
import { alphaMeasureCases, assertDistinctAlphaLayers, colorAdjustGradient, colorAdjustGrid, parityCases, renderCase, renderColorAdjustV3, sceneGap, solid } from "./cpu-composite.js";

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
function sourcesAt(testCase, x, y) {
  const scale = 2 ** (testCase.level || 0);
  const docX = testCase.tx * 256 * scale + x * scale;
  const docY = testCase.ty * 256 * scale + y * scale;
  const sources = [];
  const walk = (node) => {
    if (node.type === "group") {
      node.children.forEach(walk);
      return;
    }
    const image = node.image;
    if (docX >= 0 && docY >= 0 && docX < image.w && docY < image.h) {
      const i = (docY * image.w + docX) * 4;
      sources.push([image.pixels[i], image.pixels[i + 1], image.pixels[i + 2], image.pixels[i + 3]]);
    }
  };
  testCase.nodes.forEach(walk);
  return sources;
}
function measureRow(testCase, cpu, gpu) {
  let count = 0;
  const maxAbs = [0, 0, 0, 0];
  const mismatches = [];
  const n = Math.min(cpu.length, gpu.length) / 4;
  for (let i = 0; i < n; i++) {
    const o = i * 4;
    if (cpu[o] === gpu[o] && cpu[o + 1] === gpu[o + 1] && cpu[o + 2] === gpu[o + 2] && cpu[o + 3] === gpu[o + 3]) continue;
    count++;
    for (let channel = 0; channel < 4; channel++) {
      maxAbs[channel] = Math.max(maxAbs[channel], Math.abs(cpu[o + channel] - gpu[o + channel]));
    }
    if (mismatches.length < 5) {
      const x = i % 256;
      const y = (i / 256) | 0;
      mismatches.push({
        x,
        y,
        sources: sourcesAt(testCase, x, y),
        gpu: [gpu[o], gpu[o + 1], gpu[o + 2], gpu[o + 3]],
        cpu: [cpu[o], cpu[o + 1], cpu[o + 2], cpu[o + 3]],
      });
    }
  }
  return {
    name: testCase.name,
    enforced: false,
    count,
    maxAbs,
    mismatches,
    lengths: { cpu: cpu.length, gpu: gpu.length },
  };
}
function formatMeasure(row) {
  const pixels = row.mismatches.map((pixel) => {
    const sources = pixel.sources.length ? pixel.sources.map((px) => px.join(",")).join(" | ") : "none";
    return `pixel ${pixel.x},${pixel.y} source ${sources} gpu ${pixel.gpu.join(",")} cpu ${pixel.cpu.join(",")}`;
  });
  return `${row.count} pixels differ; max abs ${row.maxAbs.join(",")}${pixels.length ? "; " + pixels.join("; ") : ""}`;
}
function square() {
  const image = solid(256, 256, [0, 0, 0, 0]);
  for (let y = 80; y < 180; y++) for (let x = 80; x < 180; x++) image.pixels.set([255, 0, 0, 255], (y * 256 + x) * 4);
  return image;
}


// Measured-not-enforced: all 256 bytes in an Rgba8Unorm texture, read with
// textureLoad + unorm_byte (the effects.wgsl text). Records, never fails.
async function measureAtlasRead() {
  try {
    const adapter = await navigator.gpu.requestAdapter();
    const device = await adapter.requestDevice();
    const texel = (i) => [i, 255 - i, i ^ 0x55, (i + 128) & 255];
    const data = new Uint8Array(1024);
    for (let i = 0; i < 256; i++) data.set(texel(i), i * 4);
    const texture = device.createTexture({ size: [256, 1], format: "rgba8unorm", usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST });
    device.queue.writeTexture({ texture }, data, { bytesPerRow: 1024 }, [256, 1]);
    const code = `@group(0) @binding(0) var atlas: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> out: array<u32, 1024>;
fn unorm_byte(v: f32) -> u32 {
  return u32(clamp(v, 0.0, 1.0) * 255.0 + 0.5);
}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3u) {
  if (id.x >= 256u) { return; }
  let v = textureLoad(atlas, vec2u(id.x, 0u), 0);
  out[id.x * 4u] = unorm_byte(v.r);
  out[id.x * 4u + 1u] = unorm_byte(v.g);
  out[id.x * 4u + 2u] = unorm_byte(v.b);
  out[id.x * 4u + 3u] = unorm_byte(v.a);
}`;
    const pipeline = device.createComputePipeline({ layout: "auto", compute: { module: device.createShaderModule({ code }), entryPoint: "main" } });
    const out = device.createBuffer({ size: 4096, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC });
    const read = device.createBuffer({ size: 4096, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ });
    const bind = device.createBindGroup({ layout: pipeline.getBindGroupLayout(0), entries: [{ binding: 0, resource: texture.createView() }, { binding: 1, resource: { buffer: out } }] });
    const encoder = device.createCommandEncoder();
    const pass = encoder.beginComputePass();
    pass.setPipeline(pipeline);
    pass.setBindGroup(0, bind);
    pass.dispatchWorkgroups(4);
    pass.end();
    encoder.copyBufferToBuffer(out, 0, read, 0, 4096);
    device.queue.submit([encoder.finish()]);
    await read.mapAsync(GPUMapMode.READ);
    const got = new Uint32Array(read.getMappedRange().slice(0));
    read.unmap();
    device.destroy();
    const mismatches = [];
    for (let i = 0; i < 256; i++) {
      const want = texel(i);
      for (let ch = 0; ch < 4; ch++) {
        if (got[i * 4 + ch] !== want[ch]) mismatches.push({ texel: i, channel: ch, byte: want[ch], read: got[i * 4 + ch] });
      }
    }
    return { label: "measured-not-enforced", format: "Rgba8Unorm", read: "textureLoad + unorm_byte", values: 256, status: mismatches.length ? "mismatch" : "exact", mismatchedChannels: mismatches.length, mismatches };
  } catch (error) {
    return { label: "measured-not-enforced", format: "Rgba8Unorm", read: "textureLoad + unorm_byte", values: 256, status: "error", error: String(error), mismatchedChannels: 0, mismatches: [] };
  }
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
  const alphaMeasure = [];
  let atlasRead = null;
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
    atlasRead = await measureAtlasRead();
    log(`atlas-read (measured-not-enforced): ${atlasRead.status} ${atlasRead.mismatchedChannels} channel mismatches${atlasRead.error ? ` ${atlasRead.error}` : ""}`);
    await test("alpha-measure layers are distinct", async () => {
      assertDistinctAlphaLayers();
    });
    for (const testCase of alphaMeasureCases()) {
      const start = performance.now();
      try {
        const doc = await engine.createDocument({ width: testCase.w, height: testCase.h });
        for (const node of testCase.nodes) await addNode(doc, engine, node);
        const cpu = renderCase(testCase);
        const gpu = await gpuTile(engine, testCase);
        const row = measureRow(testCase, cpu, gpu);
        alphaMeasure.push(row);
        const detail = formatMeasure(row);
        results.push({
          name: testCase.name,
          pass: true,
          measure: true,
          enforced: false,
          ms: performance.now() - start,
          detail,
          count: row.count,
          maxAbs: row.maxAbs,
          mismatches: row.mismatches,
        });
        log(`MEASURED-NOT-ENFORCED ${testCase.name}: ${detail}`);
      } catch (error) {
        results.push({
          name: testCase.name,
          pass: false,
          measure: true,
          enforced: false,
          error: String(error),
          ms: performance.now() - start,
        });
        log(`FAIL ${testCase.name} ${error}`);
      }
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
    await test("color-adjust", async () => {
      const image = colorAdjustGradient();
      const fixture = await fetch("../../tests/fixtures/render-v3-empty.alto");
      assert(fixture.ok, `render-v3 fixture status ${fixture.status}`);
      const packageBytes = await fixture.arrayBuffer();
      const failures = [];
      for (const settings of colorAdjustGrid()) {
        const name = `color-adjust c=${settings.contrastLabel} s=${settings.saturationLabel} b=${settings.brightnessLabel}`;
        // createDocument is render version 2. Color-adjust's integer formula
        // runs only when the opened document is render version 3.
        const doc = await engine.open(new Blob([packageBytes]));
        const opened = doc.getSnapshot().meta;
        if (opened.render_version !== 3 || opened.format_version !== 4) {
          failures.push(`${name}: opened render ${opened.render_version} format ${opened.format_version}`);
        }
        const asset = await imported(engine, image);
        await doc.transact("adjust", (tx) => {
          tx.addRasterFromAsset(asset);
          tx.addAdjustment({
            type: "color_adjust",
            brightness: settings.brightness,
            contrast: settings.contrast,
            saturation: settings.saturation,
          });
        });
        const gpu = await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
        const cpu = renderColorAdjustV3(image, settings);
        const gap = diff(cpu, gpu);
        log(`${name}: ${gap}`);
        const gapName = sceneGap(doc.getSnapshot());
        if (gapName !== "adjustment") failures.push(`${name}: scene gap ${gapName}`);
        if (!same(cpu, gpu)) failures.push(`${name}: ${gap}`);
      }
      assert(failures.length === 0, failures.join("\n"));
      return `${colorAdjustGrid().length} settings, 0 pixels differ`;
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
      alphaMeasureLabel: "measured-not-enforced",
      alphaMeasure,
      atlasRead,
      results,
    });
  }
  const failed = results.filter((result) => result.pass === false);
  if (failed.length) throw Error(failed.map((result) => `${result.name}: ${result.error}`).join("\n"));
  log(`parity results ${results.length} failed 0`);
}
