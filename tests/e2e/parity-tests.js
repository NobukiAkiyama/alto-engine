// GPU tiles from the production compositor against the f32 CPU reference.
// A mismatch names the first pixel and the number of pixels that differ.
// Paths composite.wgsl does not draw are explicit xfails: they still read a tile,
// and a bit-exact match fails the xfail so it can be removed.
import { unstable } from "../../packages/alto-sdk/dist/unstable.js";
import { testing } from "../../packages/alto-sdk/dist/testing.js";
import { alphaMeasureCases, assertDistinctAlphaLayers, colorAdjustGradient, colorAdjustGrid, featheredRectangle, decodeTextBake, fnv64, parityCases, renderBlurV3, renderCase, renderTextV3, renderVectorV3, renderPyramidCase, renderColorAdjustV3, renderM5V3, isV3IntegerCase, sceneGap, solid } from "./cpu-composite.js";

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


// Enforced: all 256 bytes in an Rgba8Unorm texture (every channel) or an
// R8Unorm texture (the v3 glyph atlas and scratch, the r channel), read with
// textureLoad + unorm_byte (the q14.wgsl text). Recorded, then asserted.
async function measureAtlasRead(r8 = false) {
  const format = r8 ? "R8Unorm" : "Rgba8Unorm";
  const channels = r8 ? 1 : 4;
  try {
    const adapter = await navigator.gpu.requestAdapter();
    const device = await adapter.requestDevice();
    const texel = (i) => [i, 255 - i, i ^ 0x55, (i + 128) & 255];
    const data = new Uint8Array(256 * channels);
    for (let i = 0; i < 256; i++) data.set(texel(i).slice(0, channels), i * channels);
    const texture = device.createTexture({ size: [256, 1], format: format.toLowerCase(), usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST });
    device.queue.writeTexture({ texture }, data, { bytesPerRow: 256 * channels }, [256, 1]);
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
      for (let ch = 0; ch < channels; ch++) {
        if (got[i * 4 + ch] !== want[ch]) mismatches.push({ texel: i, channel: ch, byte: want[ch], read: got[i * 4 + ch] });
      }
    }
    return { label: "enforced", format, read: "textureLoad + unorm_byte", values: 256, status: mismatches.length ? "mismatch" : "exact", mismatchedChannels: mismatches.length, mismatches };
  } catch (error) {
    return { label: "enforced", format, read: "textureLoad + unorm_byte", values: 256, status: "error", error: String(error), mismatchedChannels: 0, mismatches: [] };
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
  const textMeasure = [];
  const compositeV3 = [];
  let atlasRead = null;
  // Every enforced v3 tile is locked. The composite catalog uses the FNV-1a column
  // Rust's CPU reference writes to cpu-reference.txt; every other tile is locked in
  // v3-browser.txt as "<fnv> <name>". Regenerate that file with
  // ALTO_WRITE_V3_BROWSER=1 node scripts/verify-browser.mjs --suite=parity.
  const writeParity = globalThis.__altoWriteV3Browser === true;
  const rustV3 = new Map((await (await fetch("../../tests/parity/cpu-reference.txt")).text()).split("\n")
    .filter((line) => line.startsWith("v3-"))
    .map((line) => { const [name, , fnv] = line.split(" "); return [name, fnv]; }));
  const browserResponse = await fetch("../../tests/parity/v3-browser.txt");
  const browserV3 = new Map();
  for (const line of browserResponse.ok ? (await browserResponse.text()).split("\n") : []) {
    if (!line || line.startsWith("#")) continue;
    const name = line.slice(17);
    if (browserV3.has(name)) throw Error(`v3-browser.txt locks ${name} twice`);
    browserV3.set(name, line.slice(0, 16));
  }
  // The tiles checked against each file, so an unused lock fails too.
  const v3Browser = new Map();
  const v3Rust = new Set();
  const assertV3Locked = (name, cpu, gpu, locks = browserV3) => {
    const hash = fnv64(gpu);
    if (locks === browserV3) {
      assert(!v3Browser.has(name), `${name} is locked twice`);
      v3Browser.set(name, hash);
    } else {
      v3Rust.add(name);
    }
    if (writeParity && locks === browserV3) return;
    const locked = locks.get(name);
    assert(locked, `${name} has no locked v3 hash`);
    assert(fnv64(cpu) === locked && hash === locked, `${name} differs from its locked v3 hash ${locked}: reference ${fnv64(cpu)}, GPU ${hash}`);
  };
  try {
    for (const testCase of parityCases()) {
      await test(testCase.name, async () => {
        const doc = await engine.createDocument({ width: testCase.w, height: testCase.h, renderVersion: 2 });
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
      // The Rgba8Unorm read at the top level (as before), the R8Unorm read under r8Unorm.
      atlasRead = { ...(await measureAtlasRead()), r8Unorm: await measureAtlasRead(true) };
      const gap = (r) => `${r.format} ${r.status} ${r.mismatchedChannels} channel mismatches${r.error ? ` ${r.error}` : ""}`;
      const detail = `${gap(atlasRead)}; ${gap(atlasRead.r8Unorm)}`;
      log(`atlas-read (enforced): ${detail}`);
      assert(atlasRead.status === "exact" && atlasRead.r8Unorm.status === "exact", `atlas-read ${detail}`);
      return detail;
    });
    await test("alpha-measure layers are distinct", async () => {
      assertDistinctAlphaLayers();
    });
    for (const testCase of alphaMeasureCases()) {
      const start = performance.now();
      try {
        const doc = await engine.createDocument({ width: testCase.w, height: testCase.h, renderVersion: 2 });
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
          if (enforced) assertV3Locked(`v3-${testCase.name}`, cpu, gpu, rustV3);
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
            assertV3Locked(testCase.name, cpu, gpu, rustV3);
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
    // GPU_UNSUPPORTED warm and cold. The plain-path tiles at 2^23 px use the
    // empty v3 fixture 8388864 + 256 px wide, opened through `engine.open`
    // (`openPackage`, no canvas limit; `createDocument` caps new documents at
    // 65536 px), with one small image imported and moved there.
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
      const check = async (name, x, y, change, want, renderPath = "v3-m5-integer") => {
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
        const row = { ...measureRow({ name, nodes: [], level: 0, tx: x, ty: y }, want, warm), renderPath, enforced: true, status: same(want, warm) ? "exact" : "mismatch" };
        compositeV3.push(row);
        assert(same(want, warm), formatMeasure(row));
        assertV3Locked(`${name} (${x}, ${y})`, want, warm);
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
      // The plain path at 2^23 px (Linus 08:10, Codex P2-1): a 256x256 raster
      // (no effect or mask, so the integer tile compositor) moved to tile 32767
      // draws and equals the CPU; moved to tile 32768, whose pixel centers do not
      // fit i32, it is refused there; left at tile 32767, tile 32768 gets nothing
      // from it and is transparent, as the CPU.
      const WIDE = 8388864 + 256;
      for (const [name, shift, key, refused] of [
        ["v3-plain-tile-32767-draws", 8388352, 32767, false],
        ["v3-plain-tile-32768-refused", 8388608, 32768, true],
        ["v3-plain-tile-32768-no-contribution", 8388352, 32768, false],
      ]) {
        await test(name, async () => {
          const doc = await open("render-v3-empty-8389120x256.alto");
          const meta = doc.getSnapshot().meta;
          assert(meta.width === WIDE && meta.height === 256, `wide fixture is ${meta.width}x${meta.height}`);
          const image = colorAdjustGradient();
          const asset = await imported(engine, image);
          const transform = [1, 0, 0, 1, shift, 0];
          const change = async () => {
            await doc.transact("raster", (tx) => {
              const id = tx.addRasterFromAsset(asset);
              tx.setTransform(id, transform);
            });
            assert(sceneGap(doc.getSnapshot()) === null, `${name} is not on the plain path`);
          };
          if (refused) return check(name, key, 0, change, { message: "GPU_UNSUPPORTED: v3 draw ROI has no integer tile range" });
          const cpu = renderCase({ w: WIDE, h: 256, level: 0, tx: key, ty: 0,
            nodes: [{ type: "raster", image, mode: 0, opacity: 1, visible: true, transform }] }, 3);
          if (key === 32767) assert(cpu.some((v, i) => i % 4 === 3 && v !== 0), `${name}: the CPU tile is transparent`);
          else assert(cpu.every((v) => v === 0), `${name}: the CPU tile is not transparent`);
          return check(name, key, 0, change, cpu, null);
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
      const doc = await engine.createDocument({ width: 256, height: 256, renderVersion: 2 });
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
      const doc = await engine.createDocument({ width: 256, height: 256, renderVersion: 2 });
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
        // This case opens an explicit v3 fixture for integer color adjustment.
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
        else {
          try { assertV3Locked(name, cpu, gpu); } catch (error) { failures.push(String(error.message ?? error)); }
        }
      }
      assert(failures.length === 0, failures.join("\n"));
      return `${colorAdjustGrid().length} settings, 0 pixels differ`;
    });
    for (const [name, effect, color, expected, tolerance] of [
      ["invert", { type: "invert" }, [255, 0, 0, 255], [0, 255, 255, 255], 0],
      ["curves", { type: "curves", points: [{ x: 0, y: 1 }, { x: 1, y: 0 }] }, [255, 0, 0, 255], [0, 255, 255, 255], 0],
      ["levels", { type: "levels", in_black: 0, in_white: 1, gamma: 2, out_black: 0, out_white: 1 }, [128, 128, 128, 255], [181, 181, 181, 255], 2],
      ["levels-gamma-10", { type: "levels", in_black: 0, in_white: 1, gamma: 10, out_black: 0, out_white: 1 }, [128, 128, 128, 255], [238, 238, 238, 255], 2],
      ["hue-saturation", { type: "hue_saturation", hue: 90, saturation: 1, lightness: 0 }, [255, 0, 0, 255], [108, 0, 255, 255], 2],
    ]) {
      for (const adjustment of [false, true]) {
        await test(`v3-${name}-${adjustment ? "adjustment" : "effect"}`, async () => {
          const doc = await engine.createDocument({ width: 256, height: 256 });
          assert(doc.getSnapshot().meta.render_version === 3, "new document did not default to v3");
          const asset = await imported(engine, solid(256, 256, color));
          let layer;
          await doc.transact("raster", (tx) => { layer = tx.addRasterFromAsset(asset); });
          if (adjustment) await doc.transact("adjust", (tx) => tx.addAdjustment(effect));
          else await doc.effects.set(layer, [effect]);
          assert(sceneGap(doc.getSnapshot()) === "v3-m5-integer", `scene gap ${sceneGap(doc.getSnapshot())}`);
          const image = await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
          const pixel = image.slice((100 * 256 + 100) * 4, (100 * 256 + 100) * 4 + 4);
          assert(pixel.every((value, i) => Math.abs(value - expected[i]) <= tolerance), `${name} ${adjustment}: ${pixel} expected ${expected}`);
          // No JS reference: the GPU tile itself is locked.
          assertV3Locked(`v3-${name}-${adjustment ? "adjustment" : "effect"}`, image, image);
        });
      }
    }
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
        assertV3Locked(name, cpu, gpu);
        const warm = new Uint8Array(await testing(engine).request("readTileLevel", { level: 1, x: 0, y: 0, pyramid: true }));
        const cold = new Uint8Array(await testing(engine).request("readTileLevel", { level: 1, x: 0, y: 0, fresh: true }));
        assert(same(cold, warm), `${name} cold level 1 differs from warm: ${diff(warm, cold)}`);
        return formatMeasure(row);
      });
    }
    // v3 Gaussian blur (doc/design/2026-10-28-v3-integer-blur.md), integer-exact at
    // level 0 (which never coarsens) against `renderBlurV3`, an independent JS
    // reference that blurs the layer as an infinite plane and cuts the tile. Each
    // tile is also re-read and read cold, and level 1 cold equals level 1 warm.
    {
      const rect = (w, h, [x0, y0, x1, y1]) => {
        const image = solid(w, h, [0, 0, 0, 0]);
        for (let y = y0; y < y1; y++) for (let x = x0; x < x1; x++) image.pixels.set([255, 0, 0, 255], (y * w + x) * 4);
        return image;
      };
      const cases = [
        { name: "blur-v3-square", image: square, radii: [8] },
        { name: "blur-v3-gradient", image: () => colorAdjustGradient(), radii: [12] },
        // R = 0 passes through; 1/512 is the tie R = 1; 64 is 129 taps.
        ...[0.001, 1 / 512, 1, 63.99, 64].map((radius) => ({ name: `blur-v3-radii r=${radius}`, image: square, radii: [radius] })),
        { name: "blur-v3-stack", image: square, radii: [6, 9] },
        // A rectangle across the tile seams of a 512x512 document.
        ...[[0, 0], [1, 0], [0, 1], [1, 1]].map((tile) => ({
          name: `blur-v3-seam (${tile[0]}, ${tile[1]})`,
          fixture: "render-v3-empty-512x512.alto",
          canvas: [512, 512],
          tile,
          image: () => rect(512, 512, [220, 220, 290, 290]),
          radii: [12],
        })),
      ];
      for (const testCase of cases) {
        await test(testCase.name, async () => {
          const fixtureName = testCase.fixture ?? "render-v3-empty.alto";
          const fixture = await fetch(`../../tests/fixtures/${fixtureName}`);
          assert(fixture.ok, `${fixtureName} status ${fixture.status}`);
          const doc = await engine.open(new Blob([await fixture.arrayBuffer()]));
          assert(doc.getSnapshot().meta.render_version === 3, `${fixtureName} is not v3`);
          const image = testCase.image();
          const asset = await imported(engine, image);
          let layer;
          await doc.transact("raster", (tx) => { layer = tx.addRasterFromAsset(asset); });
          await doc.effects.set(layer, testCase.radii.map((radius) => ({ type: "gaussian_blur", radius })));
          const renderPath = sceneGap(doc.getSnapshot());
          assert(renderPath === "v3-m5-integer", `scene gap ${renderPath}`);
          const [tx, ty] = testCase.tile ?? [0, 0];
          const cpu = renderBlurV3(image, testCase.radii, [tx, ty], testCase.canvas ?? [256, 256]);
          const gpu = await gpuTile(engine, { level: 0, tx, ty });
          const again = await gpuTile(engine, { level: 0, tx, ty });
          const cold = new Uint8Array(await testing(engine).request("readTileLevel", { level: 0, x: tx, y: ty, fresh: true }));
          assert(same(again, gpu), `${testCase.name} re-read differs: ${diff(gpu, again)}`);
          assert(same(cold, gpu), `${testCase.name} cold differs: ${diff(gpu, cold)}`);
          const row = { ...measureRow({ name: testCase.name, nodes: [], level: 0, tx, ty }, cpu, gpu), renderPath, enforced: true, status: same(cpu, gpu) ? "exact" : "mismatch" };
          compositeV3.push(row);
          log(`${testCase.name} (enforced): ${formatMeasure(row)}`);
          assert(same(cpu, gpu), formatMeasure(row));
          assertV3Locked(testCase.name, cpu, gpu);
          if (!testCase.tile) {
            const warm = new Uint8Array(await testing(engine).request("readTileLevel", { level: 1, x: 0, y: 0, pyramid: true }));
            const coldLevel1 = new Uint8Array(await testing(engine).request("readTileLevel", { level: 1, x: 0, y: 0, fresh: true }));
            assert(same(coldLevel1, warm), `${testCase.name} cold level 1 differs from warm: ${diff(warm, coldLevel1)}`);
          }
          return formatMeasure(row);
        });
      }
    }
    // v3 vector fill (doc/design/2026-10-28-v3-integer-vector-fill.md), integer-exact
    // against `renderVectorV3`, an independent JS rasterizer. Colours are exact in
    // f32 so the document's stored values are the JS values.
    {
      const rect = (x0, y0, x1, y1, reverse = false) => {
        const pts = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
        if (reverse) pts.reverse();
        return [{ op: "move", x: pts[0][0], y: pts[0][1] }, ...pts.slice(1).map(([x, y]) => ({ op: "line", x, y })), { op: "close" }];
      };
      const circle = (cx, cy, r) => {
        const k = 0.5522847498 * r;
        return [
          { op: "move", x: cx + r, y: cy },
          { op: "cubic", c1x: cx + r, c1y: cy + k, c2x: cx + k, c2y: cy + r, x: cx, y: cy + r },
          { op: "cubic", c1x: cx - k, c1y: cy + r, c2x: cx - r, c2y: cy + k, x: cx - r, y: cy },
          { op: "cubic", c1x: cx - r, c1y: cy - k, c2x: cx - k, c2y: cy - r, x: cx, y: cy - r },
          { op: "cubic", c1x: cx + k, c1y: cy - r, c2x: cx + r, c2y: cy - k, x: cx + r, y: cy },
          { op: "close" },
        ];
      };
      const star = (cx, cy, r) => {
        const pts = [0, 1, 2, 3, 4].map((i) => {
          const a = ((-90 + 144 * i) * Math.PI) / 180;
          return [cx + r * Math.cos(a), cy + r * Math.sin(a)];
        });
        return [{ op: "move", x: pts[0][0], y: pts[0][1] }, ...pts.slice(1).map(([x, y]) => ({ op: "line", x, y })), { op: "close" }];
      };
      const fill = (commands, color, rule = "non_zero") => ({ data: { commands }, fill: color, stroke: null, fill_rule: rule });
      const red = [1, 0, 0, 1], halfRed = [1, 0, 0, 0.5], blue = [0, 0, 1, 0.75], green = [0, 1, 0, 0.5];
      const angle = 0.3;
      const rotation = [Math.cos(angle), Math.sin(angle), -Math.sin(angle), Math.cos(angle),
        128 - (Math.cos(angle) - Math.sin(angle)) * 128, 128 - (Math.sin(angle) + Math.cos(angle)) * 128];
      const opaqueGradient = () => {
        const image = colorAdjustGradient();
        for (let i = 3; i < image.pixels.length; i += 4) image.pixels[i] = 255;
        return image;
      };
      const vectorCases = [
        { name: "vector-v3-square", paths: [fill(rect(32, 32, 200, 200), red)] },
        { name: "vector-v3-fractional", paths: [fill(rect(32.5, 32.5, 200.5, 200.5), red)] },
        { name: "vector-v3-circle", paths: [fill(circle(128, 128, 80), blue)] },
        { name: "vector-v3-fill-rules star non-zero", paths: [fill(star(128, 128, 100), red)] },
        { name: "vector-v3-fill-rules star even-odd", paths: [fill(star(128, 128, 100), red, "even_odd")] },
        ...["non_zero", "even_odd"].flatMap((rule) => [false, true].map((reverse) => ({
          name: `vector-v3-fill-rules hole ${rule}${reverse ? " reversed" : ""}`,
          paths: [fill([...rect(32, 32, 224, 224, reverse), ...rect(96.25, 96.75, 160.5, 160, !reverse)], blue, rule)],
        }))),
        ...["non_zero", "even_odd"].map((rule) => ({
          name: `vector-v3-shared-edge ${rule}`,
          paths: [fill([...rect(32.5, 32.5, 200.5, 200.5), ...rect(96.5, 32.5, 200.5, 200.5)], red, rule)],
        })),
        { name: "vector-v3-overlap", paths: [fill(rect(15.5, 17.5, 178.5, 190.5), halfRed), fill(rect(70.25, 35.75, 221.5, 205.25), green), fill(circle(150, 150, 70), [0, 0, 1, 0.5])] },
        { name: "vector-v3-rotated", transform: rotation, paths: [fill(circle(128, 128, 80), blue), fill(star(128, 128, 60), halfRed, "even_odd")] },
        ...[[0, 0], [1, 0], [0, 1], [1, 1]].map((tile) => ({
          name: `vector-v3-seam (${tile[0]}, ${tile[1]})`, fixture: "render-v3-empty-512x512.alto", canvas: [512, 512], tile,
          paths: [fill(circle(256, 256, 80), blue)],
        })),
        // A cubic far past NMAX skips its path (VECTOR_CURVE_RANGE); the square draws.
        { name: "vector-v3-curve-range", paths: [
          fill([{ op: "move", x: 0, y: 0 }, { op: "cubic", c1x: 600000, c1y: 0, c2x: -600000, c2y: 600000, x: 1, y: 1 }, { op: "close" }], blue),
          fill(rect(32.5, 32.5, 100.5, 100.5), red),
        ] },
        { name: "vector-v3-degenerate", paths: [
          fill(rect(40, 40, 40, 200), red),
          fill([{ op: "move", x: 30, y: 60 }, { op: "line", x: 220, y: 60 }, { op: "close" }], red),
          fill([{ op: "move", x: 100, y: 100 }], red),
          fill([{ op: "move", x: 120, y: 120 }, { op: "line", x: 200, y: 130 }, { op: "line", x: 150, y: 210 }], blue),
        ] },
        // A vector mask and a clip path on a raster: coverages multiplied once.
        { name: "vector-v3-clip", image: opaqueGradient,
          vectorMask: { path: { commands: circle(120, 128, 90) }, fill_rule: "non_zero", feather: 0, enabled: true },
          clipPath: { path: { commands: rect(64.5, 20.25, 230.75, 200.5) }, fill_rule: "even_odd", enabled: true } },
      ];
      for (const testCase of vectorCases) {
        await test(testCase.name, async () => {
          const fixtureName = testCase.fixture ?? "render-v3-empty.alto";
          const fixture = await fetch(`../../tests/fixtures/${fixtureName}`);
          assert(fixture.ok, `${fixtureName} status ${fixture.status}`);
          const doc = await engine.open(new Blob([await fixture.arrayBuffer()]));
          assert(doc.getSnapshot().meta.render_version === 3, `${fixtureName} is not v3`);
          const image = testCase.image?.();
          let id;
          if (image) {
            const asset = await imported(engine, image);
            await doc.transact("raster", (tx) => { id = tx.addRasterFromAsset(asset); });
          } else {
            await doc.transact("vector", (tx) => {
              id = tx.addVector(testCase.paths);
              if (testCase.transform) tx.setTransform(id, testCase.transform);
            });
          }
          if (testCase.vectorMask) await doc.vectors.setMask(id, testCase.vectorMask);
          if (testCase.clipPath) await doc.vectors.setClipPath(id, testCase.clipPath);
          const renderPath = sceneGap(doc.getSnapshot());
          assert(renderPath === "v3-m5-integer", `scene gap ${renderPath}`);
          const [tx, ty] = testCase.tile ?? [0, 0];
          const cpu = renderVectorV3({ ...testCase, image }, [tx, ty], testCase.canvas ?? [256, 256]);
          const gpu = await gpuTile(engine, { level: 0, tx, ty });
          const cold = new Uint8Array(await testing(engine).request("readTileLevel", { level: 0, x: tx, y: ty, fresh: true }));
          assert(same(cold, gpu), `${testCase.name} cold differs: ${diff(gpu, cold)}`);
          const row = { ...measureRow({ name: testCase.name, nodes: [], level: 0, tx, ty }, cpu, gpu), renderPath, enforced: true, status: same(cpu, gpu) ? "exact" : "mismatch" };
          compositeV3.push(row);
          log(`${testCase.name} (enforced): ${formatMeasure(row)}`);
          assert(same(cpu, gpu), formatMeasure(row));
          assertV3Locked(testCase.name, cpu, gpu);
          assert(testCase.name.includes("degenerate") || gpu.some((v, i) => i % 4 === 3 && v > 0), `${testCase.name} drew nothing`);
          return formatMeasure(row);
        });
      }
    }
    // v3 text (doc/design/2026-10-28-v3-integer-text.md), integer-exact against
    // `renderTextV3`, an independent JS reference that reads only the node's
    // bake (fetched from wasm), and the bakes themselves against the native
    // fixture tests/parity/text-bake.txt (the same text bakes the same bytes).
    {
      const fontBlob = async (name) => (await fetch(`../../tests/fonts/${name}`)).blob();
      const hexBytes = (hex) => Uint8Array.from(hex.match(/../g) ?? [], (b) => parseInt(b, 16));
      const hashResponse = await fetch("../../tests/parity/cpu-reference.txt");
      assert(hashResponse.ok, `cpu-reference status ${hashResponse.status}`);
      const textHashes = new Map((await hashResponse.text()).split("\n")
        .filter((line) => line.startsWith("text-v3-"))
        .map((line) => { const [name, , fnv] = line.split(" "); return [name, fnv]; }));
      // `content(latin, jp)` from the document's font ids. The fonts are imported
      // for each document: opening another one drops pending import pins.
      const addText = async (doc, content, transform = [1, 0, 0, 1, 0, 0]) => {
        const latinFont = await engine.assets.importFont(await fontBlob("AltoTestLatin.ttf"));
        const jpFont = await engine.assets.importFont(await fontBlob("AltoTestJP.otf"));
        let id;
        await doc.transact("text", (tx) => {
          const latin = tx.addFont(latinFont), jp = tx.addFont(jpFont);
          id = tx.addText(content(latin, jp));
          tx.setTransform(id, transform);
        });
        return id;
      };
      const open = async (name) => {
        const fixture = await fetch(`../../tests/fixtures/${name}`);
        assert(fixture.ok, `${name} status ${fixture.status}`);
        const doc = await engine.open(new Blob([await fixture.arrayBuffer()]));
        assert(doc.getSnapshot().meta.render_version === 3, `${name} is not v3`);
        return doc;
      };
      const style = (fonts, size, color) => ({ fonts, size, ...(color ? { color } : {}) });
      const textCases = [
        { name: "text-v3-latin-atlas", at: [20.25, 60.625], content: (l) => ({ text: "Alto 0.25 px/em", style: style([l], 13.5, [0.1, 0.2, 0.9, 1]) }) },
        { name: "text-v3-latin-direct", at: [12.5, 20.375], content: (l) => ({ text: "Ag&", style: style([l], 61, [0.9, 0.1, 0.1, 0.5]) }) },
        { name: "text-v3-japanese", at: [10, 40.5], content: (l, j) => ({ text: "吾輩は猫", style: style([l, j], 30, [0, 0, 0, 0.75]) }) },
        { name: "text-v3-vertical", at: [-40.25, 8], content: (l, j) => ({ text: "吾輩は猫である。\n第12回", style: style([l, j], 40), writing_mode: "vertical_rl", frame: { kind: "area", width: 300, height: 400 } }) },
        { name: "text-v3-rotated", transform: [Math.cos(0.3), Math.sin(0.3), -Math.sin(0.3), Math.cos(0.3), 40, 30], content: (l) => ({ text: "Rotated 21", style: style([l], 26, [0.2, 0.6, 0.3, 1]) }) },
        { name: "text-v3-placeholder", at: [30, 50], content: () => ({ text: "A B", style: style([], 24) }) },
        ...[[0, 0], [1, 0], [0, 1], [1, 1]].map((tile) => ({
          name: `text-v3-seam (${tile[0]}, ${tile[1]})`, fixture: "render-v3-empty-512x512.alto", canvas: [512, 512], tile,
          hashName: `text-v3-seam-${tile[0]}-${tile[1]}`,
          at: [180.5, 200.25], content: (l, j) => ({ text: "Seam 継ぎ目\nWWW", style: style([l, j], 48, [0.4, 0.1, 0.6, 0.9]) }),
        })),
      ];
      for (const testCase of textCases) {
        await test(testCase.name, async () => {
          const doc = await open(testCase.fixture ?? "render-v3-empty.alto");
          const transform = testCase.transform ?? [1, 0, 0, 1, ...testCase.at];
          const id = await addText(doc, testCase.content, transform);
          const renderPath = sceneGap(doc.getSnapshot());
          assert(renderPath === "v3-m5-integer", `scene gap ${renderPath}`);
          const bake = await testing(engine).request("textBake", { node: id });
          assert(bake.hash && bake.hex, `${testCase.name} has no bake`);
          const [tx, ty] = testCase.tile ?? [0, 0];
          const cpu = renderTextV3(hexBytes(bake.hex), transform, [tx, ty], testCase.canvas ?? [256, 256]);
          const gpu = await gpuTile(engine, { level: 0, tx, ty });
          const locked = textHashes.get(testCase.hashName ?? testCase.name);
          assert(locked, `${testCase.name} has no locked hash`);
          assert(fnv64(cpu) === locked, `${testCase.name} JS reference hash differs from Rust: ${fnv64(cpu)} vs ${locked}`);
          assert(fnv64(gpu) === locked, `${testCase.name} GPU hash differs from Rust: ${fnv64(gpu)} vs ${locked}`);
          const cold = new Uint8Array(await testing(engine).request("readTileLevel", { level: 0, x: tx, y: ty, fresh: true }));
          assert(same(cold, gpu), `${testCase.name} cold differs: ${diff(gpu, cold)}`);
          const row = { ...measureRow({ name: testCase.name, nodes: [], level: 0, tx, ty }, cpu, gpu), renderPath, enforced: true, status: same(cpu, gpu) ? "exact" : "mismatch" };
          compositeV3.push(row);
          log(`${testCase.name} (enforced): ${formatMeasure(row)}`);
          assert(same(cpu, gpu), formatMeasure(row));
          assert(gpu.some((v, i) => i % 4 === 3 && v > 0), `${testCase.name} drew nothing`);
          return formatMeasure(row);
        });
      }
      // Diagnostic v2 output against the fixed v3 reference. These rows show
      // the atlas and small-glyph differences without making old pixels a gate.
      for (const [name, size, atlas] of [
        ["text-v2-Ag-72", 72, true],
        ["text-v2-Ag-72-no-atlas", 72, false],
        ["text-v2-Ag-12", 12, true],
      ]) {
        await test(name, async () => {
          const transform = [1, 0, 0, 1, 24, 80];
          const content = (l) => ({ text: "Ag", style: style([l], size) });
          const v3 = await open("render-v3-empty.alto");
          const id = await addText(v3, content, transform);
          const bake = await testing(engine).request("textBake", { node: id });
          assert(bake.hex, `${name} has no v3 reference bake`);
          const cpu = renderTextV3(hexBytes(bake.hex), transform, [0, 0], [256, 256]);
          const v2 = await engine.createDocument({ width: 256, height: 256, renderVersion: 2 });
          await addText(v2, content, transform);
          await unstable(engine).setGlyphAtlas(atlas);
          let gpu;
          try { gpu = await gpuTile(engine, { level: 0, tx: 0, ty: 0 }); }
          finally { await unstable(engine).setGlyphAtlas(true); }
          const row = { ...measureRow({ name, nodes: [], level: 0, tx: 0, ty: 0 }, cpu, gpu), measure: true, enforced: false, baseline: "v3-text-reference" };
          textMeasure.push(row);
          log(`MEASURED-NOT-ENFORCED ${name}: ${formatMeasure(row)}`);
          return { detail: formatMeasure(row), measure: true, enforced: false, count: row.count, maxAbs: row.maxAbs };
        });
      }
      // The browser (wasm) bakes the native fixture's texts to the same bytes.
      await test("text-v3-bake matches the native fixture", async () => {
        const fixture = await (await fetch("../../tests/parity/text-bake.txt")).text();
        const want = new Map(fixture.split("\n").filter((line) => line && !line.startsWith("#")).map((line) => line.split(" ")));
        const texts = {
          "text-v3-latin": (l) => ({ text: "Alto 0.25 px/em", style: style([l], 37.5) }),
          "text-v3-vertical": (l, j) => ({ text: "吾輩は猫である。\n第12回", style: style([l, j], 40), writing_mode: "vertical_rl", frame: { kind: "area", width: 300, height: 400 } }),
          "text-v3-placeholder": () => ({ text: "A B", style: style([], 24) }),
        };
        assert(want.size === Object.keys(texts).length, `fixture has ${want.size} rows`);
        const failures = [];
        for (const [name, content] of Object.entries(texts)) {
          const doc = await open("render-v3-empty.alto");
          const id = await addText(doc, content);
          const bake = await testing(engine).request("textBake", { node: id });
          if (bake.hash !== want.get(name)) failures.push(`${name}: wasm ${bake.hash}, native ${want.get(name)}`);
          // The bytes returned are a valid bake with glyphs (not only a hash).
          const decoded = bake.hex && decodeTextBake(hexBytes(bake.hex));
          if (!decoded?.glyphs.length) failures.push(`${name}: the returned bytes are not a bake with glyphs`);
        }
        assert(failures.length === 0, failures.join("; "));
        return `${want.size} bakes equal the native fixture`;
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
        ["v3-adjustment-screen-unmasked", async (doc) => {
          const image = opaque();
          await raster(doc, image);
          await doc.transact("adjust", (tx) => {
            const id = tx.addAdjustment({ type: "color_adjust", ...vivid });
            tx.setProps(id, { opacity: 0.5, blend: "screen" });
          });
          return { layers: [{ image }, { adjust: vivid, opacity: 0.5, mode: 2 }] };
        }],
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
          assertV3Locked(name, cpu, gpu);
          const warm = new Uint8Array(await testing(engine).request("readTileLevel", { level: 1, x: 0, y: 0, pyramid: true }));
          const cold = new Uint8Array(await testing(engine).request("readTileLevel", { level: 1, x: 0, y: 0, fresh: true }));
          assert(same(cold, warm), `${name} cold level 1 differs from warm: ${diff(warm, cold)}`);
          return formatMeasure(row);
        });
      }
    }
    // DQ3-1 without blocking the Worker: a cold v3 overview is built through the
    // hierarchy in bounded steps, yielding if needed, and equals the warm pyramid.
    await test("v3 cold pyramid builds in bounded steps", async () => {
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
      log(`v3 cold pyramid: ${steps} steps over ${frame.diagnosticTicks} diagnostic ticks (${frame.ticks} wait ticks), max Worker tick ${frame.maxWorkerTickMs?.toFixed?.(1)} ms, max step ${frame.profile?.maxStepMs?.toFixed?.(1)} ms`);
      assert(steps >= 31, `cold level 4 built in ${steps} steps; expected one per leaf or downsample`);
      // Every synchronous Worker tick since resetRenderCache is measured,
      // including automatic drain ticks. Allow the 8 ms renderer budget,
      // one final step past its deadline, and 8 ms for scene/Worker overhead.
      const maxStepMs = frame.profile?.maxStepMs;
      const maxTickMs = frame.maxWorkerTickMs;
      assert(frame.diagnosticTicks >= frame.ticks && frame.ticks >= 1 && Number.isFinite(maxTickMs) && Number.isFinite(maxStepMs),
        `missing cold pyramid tick timing: ${frame.diagnosticTicks} diagnostic ticks, ${frame.ticks} wait ticks, max ${maxTickMs} ms`);
      assert(maxTickMs <= 16 + maxStepMs,
        `cold level 4 blocked the Worker for ${maxTickMs} ms in one tick; max step ${maxStepMs} ms`);
      const shown = new Uint8Array(await testing(engine).request("readTileLevel", { level: 4, x: 0, y: 0 }));
      const cold = new Uint8Array(await testing(engine).request("readTileLevel", { level: 4, x: 0, y: 0, fresh: true }));
      const cpu = renderPyramidCase({ w: 4096, h: 256, level: 4, tx: 0, ty: 0,
        nodes: [{ type: "raster", image, mode: 0, opacity: 1, visible: true, transform: [1, 0, 0, 1, 0, 0] }] });
      assert(same(cpu, shown), `displayed level 4: ${diff(cpu, shown)}`);
      assert(same(cpu, cold), `cold readback level 4: ${diff(cpu, cold)}`);
      return `${steps} steps, ${frame.diagnosticTicks} diagnostic ticks`;
    });
    await xfail("vector-fill", "vector: 4x MSAA mesh, not composite.wgsl", async () => {
      const doc = await engine.createDocument({ width: 256, height: 256, renderVersion: 2 });
      const data = await engine.vectors.parseSvg("M32 32 H200 V200 H32 Z");
      await doc.transact("vector", (tx) => tx.addVector([{ data, fill: [1, 0, 0, 1], stroke: null, fill_rule: "non_zero" }]));
      const gpu = await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
      assert(gpu.length === 262144 && gpu.some((v, i) => i % 4 === 3 && v > 0), "vector produced no ink");
      assert(sceneGap(doc.getSnapshot()) === "vector", sceneGap(doc.getSnapshot()));
      const empty = new Uint8Array(262144);
      assert(!same(empty, gpu), "vector matched an empty composite bit-exactly; remove the xfail");
    });
    await xfail("text", "text: glyph atlas or vector outlines, no separate CPU pixel reference", async () => {
      const doc = await engine.createDocument({ width: 256, height: 256, renderVersion: 2 });
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
    await test("new documents default to render version 3 and save format 4", async () => {
      const doc = await engine.createDocument({ width: 256, height: 256 });
      const fresh = doc.getSnapshot().meta;
      assert(fresh.render_version === 3 && fresh.format_version === 4, `new document ${fresh.render_version}/${fresh.format_version}`);
      const saved = await testing(engine).saveBytes();
      const reopened = await engine.open(new Blob([saved]));
      const meta = reopened.getSnapshot().meta;
      assert(meta.render_version === 3 && meta.format_version === 4, `saved document ${meta.render_version}/${meta.format_version}`);
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
      assert(upgraded.render_version === 3 && upgraded.format_version === 4, `upgrade meta ${upgraded.render_version} format ${upgraded.format_version}`);
      const version3 = await gpuTile(engine, { level: 0, tx: 0, ty: 0 });
      const cpu = renderCase(parityCases().find((item) => item.name === "scale-bilinear"), 3);
      const gap = diff(cpu, version3);
      log(`render-upgrade: ${gap}`);
      assert(same(cpu, version3), gap);
      assert(!same(version1, version3), "upgrade left the version 1 pixels");
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
    await test("v3 locked hashes are exactly the enforced v3 tiles", async () => {
      // A Rust v3- line that no enforced composite tile checked is unused, for
      // example when a case leaves the integer path.
      const unchecked = [...rustV3.keys()].filter((name) => !v3Rust.has(name));
      assert(unchecked.length === 0, `cpu-reference.txt v3 lines no tile checked: ${unchecked.join(", ")}`);
      if (writeParity) return `${v3Browser.size} hashes recorded for writing`;
      const missing = [...v3Browser.keys()].filter((name) => !browserV3.has(name));
      const stale = [...browserV3.keys()].filter((name) => !v3Browser.has(name));
      assert(missing.length === 0 && stale.length === 0, `unlocked: ${missing.join(", ")}; stale: ${stale.join(", ")}`);
      return `${v3Browser.size} browser and ${rustV3.size} Rust v3 hashes`;
    });
  } finally {
    document.querySelector("#test-results").dataset.report = JSON.stringify({
      date: new Date().toISOString(),
      milestone: "gpu-cpu-parity",
      userAgent: navigator.userAgent,
      alphaMeasureLabel: "measured-not-enforced",
      alphaMeasure,
      textMeasure,
      compositeV3Label: "enforced",
      compositeV3: compositeV3.filter(isV3IntegerCase),
      v3Unsupported: compositeV3.filter((row) => !isV3IntegerCase(row)),
      integerExactCount: compositeV3.filter((row) => isV3IntegerCase(row) && row.status === "exact" && row.count === 0).length,
      atlasRead,
      v3Browser: Object.fromEntries(v3Browser),
      results,
    });
  }
  const failed = results.filter((result) => result.pass === false);
  if (failed.length) throw Error(failed.map((result) => `${result.name}: ${result.error}`).join("\n"));
  log(`parity results ${results.length} failed 0`);
}
