// GPU tiles from the production compositor against the f32 CPU reference.
// A mismatch names the first pixel and the number of pixels that differ.
// Paths composite.wgsl does not draw are explicit xfails: they still read a tile,
// and a bit-exact match fails the xfail so it can be removed.
import { unstable } from "../../packages/alto-sdk/dist/unstable.js";
import { testing } from "../../packages/alto-sdk/dist/testing.js";
import { alphaMeasureCases, assertDistinctAlphaLayers, colorAdjustGradient, colorAdjustGrid, featheredRectangle, parityCases, renderCase, renderPyramidCase, renderColorAdjustV3, renderM5V3, isV3IntegerCase, sceneGap, solid } from "./cpu-composite.js";

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


// Enforced: all 256 bytes in an Rgba8Unorm texture, read with
// textureLoad + unorm_byte (the q14.wgsl text). Recorded, then asserted.
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
    return { label: "enforced", format: "Rgba8Unorm", read: "textureLoad + unorm_byte", values: 256, status: mismatches.length ? "mismatch" : "exact", mismatchedChannels: mismatches.length, mismatches };
  } catch (error) {
    return { label: "enforced", format: "Rgba8Unorm", read: "textureLoad + unorm_byte", values: 256, status: "error", error: String(error), mismatchedChannels: 0, mismatches: [] };
  }
}

export async function runParityTests(engine, log) {
  const results = [];
  const test = async (name, fn) => {
    const start = performance.now();
    try {
      const detail = await fn();
      const row = { name, pass: true, ms: performance.now() - start, detail: detail?.detail ?? detail ?? "", ...(typeof detail === "object" ? detail : {}) };
      results.push(row);
      log(`PASS ${name} ${row.ms.toFixed(1)} ms${row.detail ? `; ${row.detail}` : ""}`);
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
  const compositeV3 = [];
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
    await test("atlas-read round-trips all 256 bytes", async () => {
      atlasRead = await measureAtlasRead();
      const gap = `${atlasRead.status} ${atlasRead.mismatchedChannels} channel mismatches${atlasRead.error ? ` ${atlasRead.error}` : ""}`;
      log(`atlas-read (enforced): ${gap}`);
      assert(atlasRead.status === "exact", `atlas-read ${gap}`);
      return gap;
    });
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
    for (const testCase of alphaMeasureCases()) {
      await test(`v3-${testCase.name}`, async () => {
        let renderPath = null;
        try {
          const suffix = testCase.w === 256 && testCase.h === 256 ? "" : `-${testCase.w}x${testCase.h}`;
          const fixture = await fetch(`../../tests/fixtures/render-v3-empty${suffix}.alto`);
          assert(fixture.ok, `v3 fixture status ${fixture.status}`);
          const doc = await engine.open(new Blob([await fixture.arrayBuffer()]));
          assert(doc.getSnapshot().meta.render_version === 3, "v3 fixture did not select integer compositing");
          // Linus 04:40 item 5: read the tile of the empty document first, so it is
          // cached when the layers arrive; the next read is through their damage.
          if (!testCase.level) await gpuTile(engine, testCase);
          for (const node of testCase.nodes) await addNode(doc, engine, node);
          // DQ3-1: a v3 tile at level >= 1 is the integer downsample of its children,
          // even when read cold, never a direct composite at that scale.
          const cpu = testCase.level ? renderPyramidCase(testCase) : renderCase(testCase, 3);
          const gpu = await gpuTile(engine, testCase);
          if (!testCase.level) {
            // The damaged read, a re-read of the cached tile and a cold read (caches
            // emptied) agree on every byte (and with the CPU below when enforced).
            const again = await gpuTile(engine, testCase);
            const cold = new Uint8Array(await testing(engine).request("readTileLevel", { level: 0, x: testCase.tx, y: testCase.ty, fresh: true }));
            assert(same(again, gpu), `v3-${testCase.name} re-read differs from the damaged read: ${diff(gpu, again)}`);
            assert(same(cold, gpu), `v3-${testCase.name} cold differs from the damaged read: ${diff(gpu, cold)}`);
          }
          renderPath = sceneGap(doc.getSnapshot());
          const enforced = isV3IntegerCase({ renderPath });
          const row = { ...measureRow(testCase, cpu, gpu), renderPath, name: `v3-${testCase.name}`, enforced, status: same(cpu, gpu) ? "exact" : "mismatch" };
          compositeV3.push(row);
          log(`${renderPath || "v3 composite"} (${enforced ? "enforced" : "measured-not-enforced"}): ${formatMeasure(row)}`);
          if (enforced) assert(same(cpu, gpu), formatMeasure(row));
          return { detail: formatMeasure(row), renderPath, enforced };
        } catch (error) {
          if (!compositeV3.some((row) => row.name === `v3-${testCase.name}`)) {
            compositeV3.push({ name: `v3-${testCase.name}`, renderPath, enforced: isV3IntegerCase({ renderPath }), status: "error", error: String(error) });
          }
          throw error;
        }
      });
    }
    for (const source of alphaMeasureCases()) {
      const testCase = { ...source, level: 1, tx: 0, ty: 0, name: `v3-${source.name}-level1` };
      await test(testCase.name, async () => {
        let renderPath = null;
        try {
          const suffix = source.w === 256 && source.h === 256 ? "" : `-${source.w}x${source.h}`;
          const fixture = await fetch(`../../tests/fixtures/render-v3-empty${suffix}.alto`);
          assert(fixture.ok, `v3 fixture status ${fixture.status}`);
          const doc = await engine.open(new Blob([await fixture.arrayBuffer()]));
          for (const node of source.nodes) await addNode(doc, engine, node);
          renderPath = sceneGap(doc.getSnapshot());
          const enforced = isV3IntegerCase({ renderPath });
          // Reset, populate children and read the parent in the same worker request.
          const gpu = new Uint8Array(await testing(engine).request("readTileLevel", { level: 1, x: 0, y: 0, pyramid: true }));
          // DQ3-1: a cold request (empty caches) must give the warm bytes.
          const cold = new Uint8Array(await testing(engine).request("readTileLevel", { level: 1, x: 0, y: 0, fresh: true }));
          const cpu = renderPyramidCase(testCase);
          const coldMatchesWarm = same(cold, gpu);
          const row = { ...measureRow(testCase,cpu,gpu), renderPath, name: testCase.name, level: 1, enforced, coldMatchesWarm, status: same(cpu,gpu) && coldMatchesWarm ? "exact" : "mismatch" };
          compositeV3.push(row);
          if (enforced) {
            assert(same(cpu,gpu),formatMeasure(row));
            assert(coldMatchesWarm, `cold level 1 differs from warm: ${diff(gpu, cold)}`);
          }
          return { detail: formatMeasure(row), renderPath, enforced };
        } catch (error) {
          if (!compositeV3.some((r)=>r.name===testCase.name)) compositeV3.push({name:testCase.name,level:1,renderPath,enforced:isV3IntegerCase({renderPath}),status:"error",error:String(error)});
          throw error;
        }
      });
    }
    // The 2^23 boundary in the browser (Linus 04:40 item 5, DQ3-4), each read
    // warm (the tile cached before the change, then damaged), re-read, and cold.
    // A drawn tile equals the CPU on every byte; a refused tile gives the same
    // GPU_UNSUPPORTED warm and cold. The canvas is at most 65536 px, so the
    // plain-path tiles past 2^23 px are off the canvas (no pixel, skipped); those
    // cases run natively (`renderer_cold_warm`, `renderer_cases`).
    {
      const vivid = { brightness: 0, contrast: 1.7, saturation: 1.6 };
      const open = async (name) => {
        const fixture = await fetch(`../../tests/fixtures/${name}`);
        assert(fixture.ok, `${name} status ${fixture.status}`);
        const doc = await engine.open(new Blob([await fixture.arrayBuffer()]));
        assert(doc.getSnapshot().meta.render_version === 3, `${name} is not v3`);
        return doc;
      };
      const read = async (x, y) => new Uint8Array(await unstable(engine).readTile(x, y));
      const readCold = async (x, y) =>
        new Uint8Array(await testing(engine).request("readTileLevel", { level: 0, x, y, fresh: true }));
      const refusal = async (fn) => {
        try {
          await fn();
        } catch (error) {
          return error;
        }
        return null;
      };
      // Warm (damaged), re-read and cold reads of tile (x, y) after `change`,
      // with the tile read before it. `want` is the CPU tile, or the refusal.
      const check = async (name, x, y, change, want) => {
        await read(x, y);
        await change();
        if (want.message) {
          for (const [kind, fn] of [["warm", () => read(x, y)], ["re-read", () => read(x, y)], ["cold", () => readCold(x, y)]]) {
            const e = await refusal(fn);
            assert(e, `${name} (${x}, ${y}) ${kind} was drawn; expected ${want.message}`);
            assert(e.code === "GPU_UNSUPPORTED" && e.message === want.message, `${name} (${x}, ${y}) ${kind}: ${e.code} ${e.message}`);
          }
          compositeV3.push({ name: `${name} (${x}, ${y})`, renderPath: "v3-unsupported", enforced: false, status: "refused", error: want.message });
          return `refused warm, re-read and cold: ${want.message}`;
        }
        const warm = await read(x, y);
        const again = await read(x, y);
        const cold = await readCold(x, y);
        assert(same(again, warm), `${name} (${x}, ${y}) re-read differs: ${diff(warm, again)}`);
        assert(same(cold, warm), `${name} (${x}, ${y}) cold differs from warm: ${diff(warm, cold)}`);
        const row = { ...measureRow({ name, nodes: [], level: 0, tx: x, ty: y }, want, warm), renderPath: "v3-m5-integer", enforced: true, status: same(want, warm) ? "exact" : "mismatch" };
        compositeV3.push(row);
        assert(same(want, warm), formatMeasure(row));
        return formatMeasure(row);
      };
      // RGBA 256x256 tiles of `image` by key, for `renderM5V3`.
      const tilesOf = (image) => {
        const out = [];
        for (let ty = 0; ty * 256 < image.h; ty++) for (let tx = 0; tx * 256 < image.w; tx++) {
          const bytes = new Uint8Array(256 * 256 * 4);
          for (let y = 0; y < 256 && ty * 256 + y < image.h; y++) {
            const from = ((ty * 256 + y) * image.w + tx * 256) * 4;
            const n = Math.min(256, image.w - tx * 256) * 4;
            bytes.set(image.pixels.subarray(from, from + n), y * 256 * 4);
          }
          out.push({ key: [tx, ty], bytes });
        }
        return out;
      };
      // A 300x100 document: an opaque backdrop and a color-adjust adjustment
      // layer moved by `shift` px with a linked white mask. The engine writes the
      // mask in the layer's pixels (`mask_for_node`): 255 where the pixel center
      // lands on the canvas, so its tiles run from texel -shift to 300 - shift.
      const W = 300, H = 100;
      const backdrop = () => {
        const image = colorAdjustGradient(W, H);
        for (let i = 3; i < image.pixels.length; i += 4) image.pixels[i] = 255;
        return image;
      };
      const maskScene = (shift) => async (doc, image) => {
        const asset = await imported(engine, image);
        let adjustment;
        await doc.transact("backdrop", (tx) => { tx.addRasterFromAsset(asset); });
        return async () => {
          await doc.transact("adjust", (tx) => {
            adjustment = tx.addAdjustment({ type: "color_adjust", ...vivid });
            tx.setTransform(adjustment, [1, 0, 0, 1, shift, 0]);
          });
          await doc.masks.addWhite(adjustment);
          assert(sceneGap(doc.getSnapshot()) === "v3-m5-integer", "masked adjustment is not v3 M5");
        };
      };
      const maskCpu = (image, shift, x, y) => {
        const at = (tx, ty) => (tx + 0.5 + shift >= 0 && tx + 0.5 + shift < W && ty + 0.5 < H ? 255 : 0);
        const out = renderM5V3([
          { tiles: tilesOf(image), transform: [1, 0, 0, 1, 0, 0] },
          { adjust: vivid, opacity: 1, mode: 0, mask: { transform: [1, 0, 0, 1, shift, 0], at } },
        ], 256, 256, [x * 256, y * 256], 1);
        for (let py = 0; py < 256; py++) for (let px = 0; px < 256; px++) {
          if (x * 256 + px >= W || y * 256 + py >= H) out.fill(0, (py * 256 + px) * 4, (py * 256 + px) * 4 + 4);
        }
        return out;
      };
      // Mask tiles end at texel 2^23 - 1 (shift -(2^23 - 300)); the pixels of tile
      // (1, 0) from 300 on are past 2^23 and dropped, and read nothing: drawn.
      await test("v3-mask-up-to-2^23-draws", async () => {
        const shift = -(8388608 - W);
        const doc = await open("render-v3-empty-300x100.alto");
        const image = backdrop();
        const change = await maskScene(shift)(doc, image);
        return check("v3-mask-up-to-2^23-draws", 1, 0, change, maskCpu(image, shift, 1, 0));
      });
      // Shifted 100 px further, the mask has a tile at 2^23 that dropped pixels
      // could read: refused, warm and cold.
      for (const x of [0, 1]) {
        await test(`v3-mask-tile-at-2^23-refused (${x}, 0)`, async () => {
          const doc = await open("render-v3-empty-300x100.alto");
          const change = await maskScene(-(8388608 - W) - 100)(doc, backdrop());
          return check("v3-mask-tile-at-2^23-refused", x, 0, change, { message: "GPU_UNSUPPORTED: v3 mask ROI has no integer tile range" });
        });
      }
      // M5 rasters (a color-adjust effect) placed at tile 32767 (the last tile
      // whose pixel centers fit i32) and at 32768 (they do not): drawn (off the
      // canvas, so transparent, as the CPU), and refused.
      for (const [key, want] of [
        [32767, () => new Uint8Array(256 * 256 * 4)],
        [32768, () => ({ message: "GPU_UNSUPPORTED: v3 raster ROI has no integer tile range" })],
      ]) {
        await test(`v3-raster-at-tile-${key}`, async () => {
          const doc = await open("render-v3-empty.alto");
          const asset = await imported(engine, colorAdjustGradient());
          const change = async () => {
            let id;
            await doc.transact("raster", (tx) => {
              id = tx.addRasterFromAsset(asset);
              tx.setTransform(id, [1, 0, 0, 1, key * 256, 0]);
            });
            await doc.effects.set(id, [{ type: "color_adjust", ...vivid }]);
          };
          return check(`v3-raster-at-tile-${key}`, key, 0, change, want());
        });
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
        if (gapName !== "v3-m5-integer") failures.push(`${name}: scene gap ${gapName}`);
        if (!same(cpu, gpu)) failures.push(`${name}: ${gap}`);
      }
      assert(failures.length === 0, failures.join("\n"));
      return `${colorAdjustGrid().length} settings, 0 pixels differ`;
    });
    // v3 M5 at opacity 0.5, integer-exact: a color-adjusted layer and an adjustment
    // layer, each at half opacity over opaque blue, plus their cold level 1.
    for (const [name, build] of [
      ["v3-m5-layer-opacity-half", (image) => [
        { image: solid(256, 256, [0, 0, 255, 255]) },
        { image, opacity: 0.5, effects: [{ brightness: 0, contrast: 1.7, saturation: 1.6 }] },
      ]],
      ["v3-m5-adjustment-opacity-half", (image) => [
        { image },
        { adjust: { brightness: 0, contrast: 1.7, saturation: 1.6 }, opacity: 0.5 },
      ]],
    ]) {
      await test(name, async () => {
        const fixture = await fetch("../../tests/fixtures/render-v3-empty.alto");
        assert(fixture.ok, `render-v3 fixture status ${fixture.status}`);
        const doc = await engine.open(new Blob([await fixture.arrayBuffer()]));
        assert(doc.getSnapshot().meta.render_version === 3, "v3 fixture did not open at version 3");
        const layers = build(colorAdjustGradient());
        for (const layer of layers) {
          if (layer.adjust) {
            await doc.transact("adjust", (tx) => {
              const id = tx.addAdjustment({ type: "color_adjust", ...layer.adjust });
              tx.setProps(id, { opacity: layer.opacity });
            });
            continue;
          }
          const asset = await imported(engine, layer.image);
          let id;
          await doc.transact("raster", (tx) => {
            id = tx.addRasterFromAsset(asset);
            tx.setProps(id, { opacity: layer.opacity ?? 1 });
          });
          if (layer.effects) await doc.effects.set(id, layer.effects.map((e) => ({ type: "color_adjust", ...e })));
        }
        const renderPath = sceneGap(doc.getSnapshot());
        assert(renderPath === "v3-m5-integer", `scene gap ${renderPath}`);
        const cpu = renderM5V3(layers);
        const gpu = await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
        const row = { ...measureRow({ name, nodes: [], level: 0, tx: 0, ty: 0 }, cpu, gpu), renderPath, enforced: true, status: same(cpu, gpu) ? "exact" : "mismatch" };
        compositeV3.push(row);
        log(`${name} (enforced): ${formatMeasure(row)}`);
        assert(same(cpu, gpu), formatMeasure(row));
        const warm = new Uint8Array(await testing(engine).request("readTileLevel", { level: 1, x: 0, y: 0, pyramid: true }));
        const cold = new Uint8Array(await testing(engine).request("readTileLevel", { level: 1, x: 0, y: 0, fresh: true }));
        assert(same(cold, warm), `${name} cold level 1 differs from warm: ${diff(warm, cold)}`);
        return formatMeasure(row);
      });
    }
    // v3 pixel masks (doc/design/2026-10-28-v3-integer-mask.md), integer-exact:
    // layer masks (mode 21), selection-limited color-adjust (mode 22) and masked
    // adjustment layers (mode 23), plus their cold level 1. The JS
    // reference rebuilds the mask bytes with the `selection_ops` rectangle formula.
    {
      const IDENT = [1, 0, 0, 1, 0, 0];
      const vivid = { brightness: 0, contrast: 1.7, saturation: 1.6 };
      // Edges at pixel fractions 0.25, 0.75, 0.5 and 0 with feather 64 (the
      // limit), so the four feathered edges hold every mask byte 0..=255.
      const feathered = { kind: "rectangle", x: 40.25, y: 40.75, width: 176.25, height: 175.25, feather: 64 };
      const featheredMask = { bytes: featheredRectangle([40.25, 40.75, 216.5, 216], 64) };
      const halfRect = { kind: "rectangle", x: 0, y: 0, width: 128, height: 256 };
      const halfMask = { bytes: Uint8Array.from({ length: 65536 }, (_, i) => (i % 256 < 128 ? 255 : 0)) };
      const whiteMask = { bytes: new Uint8Array(65536).fill(255) };
      const angle = 0.3;
      const c = Math.cos(angle), s = Math.sin(angle);
      const rotated = [c, s, -s, c, 128 - (c - s) * 128, 128 - (s + c) * 128];
      const opaque = () => {
        const image = colorAdjustGradient();
        for (let i = 3; i < image.pixels.length; i += 4) image.pixels[i] = 255;
        return image;
      };
      const raster = async (doc, image, props = {}, parent) => {
        const asset = await imported(engine, image);
        let id;
        await doc.transact("raster", (tx) => {
          id = tx.addRasterFromAsset(asset, parent ? { parent } : {});
          tx.setProps(id, props);
        });
        return id;
      };
      const masked = async (doc, id, spec) => {
        await doc.selection.apply(spec);
        await doc.masks.fromSelection(id);
      };
      const plain = (image, opacity) => renderCase({ w: 256, h: 256, level: 0, tx: 0, ty: 0,
        nodes: [{ type: "raster", image, mode: 0, opacity, visible: true, transform: IDENT }] }, 3);
      const maskCases = [
        // The v2 `mask` xfail scene on a v3 document.
        ["v3-mask", async (doc) => {
          const image = solid(256, 256, [255, 0, 0, 255]);
          await masked(doc, await raster(doc, image), halfRect);
          return { layers: [{ image, mask: halfMask }] };
        }],
        ...[1, 0.5, 0.35, 0].map((opacity) => [`v3-mask-coverage o=${opacity}`, async (doc) => {
          const image = colorAdjustGradient();
          await masked(doc, await raster(doc, image, { opacity }), feathered);
          const seen = new Set(featheredMask.bytes);
          assert(seen.size === 256, `feathered selection has ${seen.size} distinct bytes`);
          return { layers: [{ image, opacity, mask: featheredMask }] };
        }]),
        // A full mask equals the v3 tile compositor without a mask (0.5 is the 8192 tie).
        ...[1, 0.5, 0.35].map((opacity) => [`v3-mask-white o=${opacity}`, async (doc) => {
          const image = colorAdjustGradient();
          await doc.masks.addWhite(await raster(doc, image, { opacity }));
          return { layers: [{ image, opacity, mask: whiteMask }], also: plain(image, opacity) };
        }]),
        ["v3-mask-selection-color-adjust", async (doc) => {
          const image = colorAdjustGradient();
          const id = await raster(doc, image);
          await doc.selection.apply(feathered);
          await doc.effects.set(id, [{ type: "color_adjust", ...vivid }]);
          return { layers: [{ image, effects: [{ ...vivid, selection: featheredMask }] }] };
        }],
        ...[[0, 1], [1, 1], [2, 1], [3, 1], [2, 0.5]].map(([mode, opacity]) => [`v3-mask-adjustment-${BLENDS[mode]} o=${opacity}`, async (doc) => {
          const image = opaque();
          await raster(doc, image);
          let id;
          await doc.transact("adjust", (tx) => {
            id = tx.addAdjustment({ type: "color_adjust", ...vivid });
            tx.setProps(id, { opacity, blend: BLENDS[mode] });
          });
          await masked(doc, id, feathered);
          return { layers: [{ image }, { adjust: vivid, opacity, mode, mask: featheredMask }] };
        }]),
        // An unlinked mask turned 0.3 rad: the Q16 lookup (`SAMPLE_INTEGER`).
        ["v3-mask-rotated", async (doc) => {
          const image = colorAdjustGradient();
          const id = await raster(doc, image);
          await masked(doc, id, feathered);
          await doc.transact("mask transform", (tx) => tx.setProps(id, { mask_linked: false, mask_transform: rotated }));
          return { layers: [{ image, mask: { ...featheredMask, transform: rotated } }] };
        }],
        // A masked pass-through group of semi-transparent children over blue.
        ["v3-mask-group", async (doc) => {
          const blue = solid(256, 256, [0, 0, 255, 255]);
          const red = solid(256, 256, [255, 0, 0, 128]);
          const image = colorAdjustGradient();
          await raster(doc, blue);
          let group;
          await doc.transact("group", (tx) => {
            group = tx.addGroup({ isolated: false });
            tx.setProps(group, { opacity: 0.5 });
          });
          await raster(doc, red, {}, group);
          await raster(doc, image, { blend: "multiply" }, group);
          await masked(doc, group, feathered);
          return { layers: [{ image: blue }, { group: [{ image: red }, { image, mode: 1 }], opacity: 0.5, mask: featheredMask }] };
        }],
      ];
      for (const [name, build] of maskCases) {
        await test(name, async () => {
          const fixture = await fetch("../../tests/fixtures/render-v3-empty.alto");
          assert(fixture.ok, `render-v3 fixture status ${fixture.status}`);
          const doc = await engine.open(new Blob([await fixture.arrayBuffer()]));
          assert(doc.getSnapshot().meta.render_version === 3, "v3 fixture did not open at version 3");
          const { layers, also } = await build(doc);
          const renderPath = sceneGap(doc.getSnapshot());
          assert(renderPath === "v3-m5-integer", `scene gap ${renderPath}`);
          const cpu = renderM5V3(layers);
          if (also) assert(same(cpu, also), `M5 reference differs from the tile compositor: ${diff(also, cpu)}`);
          const gpu = await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
          const row = { ...measureRow({ name, nodes: [], level: 0, tx: 0, ty: 0 }, cpu, gpu), renderPath, enforced: true, status: same(cpu, gpu) ? "exact" : "mismatch" };
          compositeV3.push(row);
          log(`${name} (enforced): ${formatMeasure(row)}`);
          assert(same(cpu, gpu), formatMeasure(row));
          const warm = new Uint8Array(await testing(engine).request("readTileLevel", { level: 1, x: 0, y: 0, pyramid: true }));
          const cold = new Uint8Array(await testing(engine).request("readTileLevel", { level: 1, x: 0, y: 0, fresh: true }));
          assert(same(cold, warm), `${name} cold level 1 differs from warm: ${diff(warm, cold)}`);
          return formatMeasure(row);
        });
      }
      // An unmasked non-normal adjustment blend stays refused in v3 (#37, Linus
      // 21:46): GpuUnsupported from `v3_gap`, and no tile is drawn (DQ3-4).
      await test("v3-adjustment-screen-unmasked is refused", async () => {
        const fixture = await fetch("../../tests/fixtures/render-v3-empty.alto");
        assert(fixture.ok, `render-v3 fixture status ${fixture.status}`);
        const doc = await engine.open(new Blob([await fixture.arrayBuffer()]));
        await raster(doc, opaque());
        await doc.transact("adjust", (tx) => {
          const id = tx.addAdjustment({ type: "color_adjust", ...vivid });
          tx.setProps(id, { opacity: 0.5, blend: "screen" });
        });
        const renderPath = sceneGap(doc.getSnapshot());
        assert(renderPath === "v3-unsupported", `scene gap ${renderPath}`);
        let refusal = null;
        try {
          await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
        } catch (error) {
          refusal = error;
        }
        assert(refusal, "unmasked screen adjustment layer was drawn");
        assert(refusal.code === "GPU_UNSUPPORTED", `refused with ${refusal.code}: ${refusal.message}`);
        assert(
          String(refusal.message).includes("v3 adjustment has no integer definition yet"),
          `refused for another reason: ${refusal.message}`,
        );
        return renderPath;
      });
    }
    // DQ3-1 without blocking the Worker: a cold v3 overview is built through the
    // hierarchy in many steps over several ticks, and still equals the warm pyramid.
    await test("v3 cold pyramid is built incrementally across ticks", async () => {
      const fixture = await fetch("../../tests/fixtures/render-v3-empty-4096x256.alto");
      assert(fixture.ok, `v3 fixture status ${fixture.status}`);
      const doc = await engine.open(new Blob([await fixture.arrayBuffer()]));
      assert(doc.getSnapshot().meta.render_version === 3, "v3 fixture did not open at version 3");
      const image = { w: 4096, h: 256, pixels: new Uint8Array(4096 * 256 * 4) };
      for (let i = 0; i < image.pixels.length; i += 4) {
        const x = (i / 4) % 4096, y = Math.floor(i / 4 / 4096);
        image.pixels.set([x & 255, y, (x >> 4) & 255, 128 + (x & 127)], i);
      }
      const asset = await imported(engine, image);
      await doc.transact("raster", (tx) => tx.addRasterFromAsset(asset));
      // Level 4: one display tile over all 16 level-0 tiles, so 16 leaves and
      // 15 downsamples (8 + 4 + 2 + 1) once the caches are empty.
      const view = { center: [2048, 128], zoom: 1 / 16, rotation: 0 };
      engine.viewport.set(view);
      await testing(engine).request("setProfiling", { enabled: true });
      await testing(engine).request("resetRenderCache", { cpu: true });
      engine.viewport.set(view);
      let frame;
      try {
        frame = await unstable(engine).settle();
      } finally {
        await testing(engine).request("setProfiling", { enabled: false });
        await testing(engine).request("enablePrefetch");
      }
      const steps = frame.profile?.steps ?? 0;
      log(`v3 cold pyramid: ${steps} steps over ${frame.ticks} ticks, max step ${frame.profile?.maxStepMs?.toFixed?.(1)} ms`);
      assert(steps >= 31, `cold level 4 built in ${steps} steps; expected one per leaf or downsample`);
      assert(frame.ticks >= 2, `cold level 4 finished in ${frame.ticks} tick(s); the Worker was not handed back`);
      const shown = new Uint8Array(await testing(engine).request("readTileLevel", { level: 4, x: 0, y: 0 }));
      const cold = new Uint8Array(await testing(engine).request("readTileLevel", { level: 4, x: 0, y: 0, fresh: true }));
      const cpu = renderPyramidCase({ w: 4096, h: 256, level: 4, tx: 0, ty: 0,
        nodes: [{ type: "raster", image, mode: 0, opacity: 1, visible: true, transform: [1, 0, 0, 1, 0, 0] }] });
      assert(same(cpu, shown), `displayed level 4: ${diff(cpu, shown)}`);
      assert(same(cpu, cold), `cold readback level 4: ${diff(cpu, cold)}`);
      return `${steps} steps, ${frame.ticks} ticks`;
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
      compositeV3Label: "enforced",
      compositeV3: compositeV3.filter(isV3IntegerCase),
      v3Unsupported: compositeV3.filter((row) => !isV3IntegerCase(row)),
      integerExactCount: compositeV3.filter((row) => isV3IntegerCase(row) && row.status === "exact" && row.count === 0).length,
      atlasRead,
      results,
    });
  }
  const failed = results.filter((result) => result.pass === false);
  if (failed.length) throw Error(failed.map((result) => `${result.name}: ${result.error}`).join("\n"));
  log(`parity results ${results.length} failed 0`);
}
