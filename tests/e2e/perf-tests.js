// Plan E: timings on the paths an embedder uses, with every sample kept.
// Run with `node scripts/verify-browser.mjs --suite=perf`. ALTO_PERF_FIXTURES=4k-1,fx
// narrows the fixtures; ALTO_PERF_STRESS=1 adds the near-limit stress document,
// which is reported apart from the fixed fixtures.
//
// Public SDK calls only, plus `unstable().settle()` (GPU-complete frame) and the
// `testing` hooks B1-B8 already use, so the same file measures a build before and
// after a change. Cold display and warm operation are separate rows, and so are
// raster and vector content. A p95 is given only for rows with at least 20 samples.
import { unstable } from "../../packages/alto-sdk/dist/unstable.js";
import { testing } from "../../packages/alto-sdk/dist/testing.js";

const assert = (ok, m) => {
  if (!ok) throw Error(m);
};
const raf = () => new Promise(requestAnimationFrame);
const P95_MIN = 20;
const FONTS = ["../../tests/fonts/AltoTestLatin.ttf", "../../tests/fonts/AltoTestJP.otf"];

function stats(samples) {
  const s = [...samples].sort((a, b) => a - b);
  const mean = s.reduce((a, b) => a + b, 0) / s.length;
  const sd = Math.sqrt(s.reduce((a, b) => a + (b - mean) ** 2, 0) / Math.max(1, s.length - 1));
  return {
    n: s.length,
    min: s[0],
    median: s.length % 2 ? s[(s.length - 1) / 2] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2,
    mean,
    sd,
    max: s.at(-1),
    p95: s.length >= P95_MIN ? s[Math.ceil(s.length * 0.95) - 1] : null,
  };
}

/** Engine accounting from a frame, JS heap from the page. Neither is physical VRAM or RSS. */
async function memory(engine, frame) {
  const out = {
    wasmBytes: frame.wasmBytes,
    decodePoolPeakBytes: frame.decodePeakBytes ?? null,
    gpuAccountedBytes: frame.gpuBytes,
    gpuAccounted: frame.gpu,
    readbackAccounted: frame.readback,
    mainJsHeapUsedBytes: performance.memory?.usedJSHeapSize ?? null,
    mainJsHeapTotalBytes: performance.memory?.totalJSHeapSize ?? null,
  };
  // Includes the Workers' JS heaps (crossOriginIsolated pages only). Chrome may delay
  // the answer until the next GC; a measurement that does not arrive is reported as null.
  if (crossOriginIsolated && performance.measureUserAgentSpecificMemory) {
    const measured = await Promise.race([
      performance.measureUserAgentSpecificMemory().catch((e) => ({ error: String(e) })),
      new Promise((r) => setTimeout(() => r(null), 30000)),
    ]);
    out.uaMemory = measured
      ? measured.error
        ? measured
        : {
            bytes: measured.bytes,
            breakdown: measured.breakdown
              .filter((b) => b.bytes)
              .map((b) => ({ bytes: b.bytes, types: b.types, scope: b.attribution.map((a) => a.scope) })),
          }
      : null;
  }
  return out;
}

async function rasterImage(engine, width, height, paint) {
  const c = new OffscreenCanvas(width, height);
  paint(c.getContext("2d"), width, height);
  return engine.assets.importImage(await c.convertToBlob());
}

// Fixed fixtures. Each builds its document deterministically from public calls and
// names the layer that warm edits and strokes target.
const FIXTURES = {
  "4k-1": {
    kind: "raster",
    async build(engine) {
      const doc = await engine.createDocument({ width: 3840, height: 2160 });
      const a = await rasterImage(engine, 3840, 2160, (ctx, w, h) => {
        ctx.fillStyle = "rgb(60,90,130)";
        ctx.fillRect(0, 0, w, h);
        for (let y = 0; y < h; y += 256)
          for (let x = 0; x < w; x += 256) {
            ctx.fillStyle = `rgb(${((x / 256) * 17) % 255},${((y / 256) * 29) % 255},180)`;
            ctx.fillRect(x + 7, y + 7, 220, 220);
          }
      });
      let target;
      await doc.transact("fixture", (tx) => (target = tx.addRasterFromAsset(a, { name: "Image" })));
      return { doc, target };
    },
  },
  "4k-20": {
    kind: "raster",
    // The B1-B8 document: 20 translucent full-canvas layers.
    async build(engine) {
      const doc = await engine.createDocument({ width: 3840, height: 2160 });
      let target;
      for (let i = 0; i < 20; i++) {
        const a = await rasterImage(engine, 3840, 2160, (ctx, w, h) => {
          ctx.fillStyle = `rgba(${40 + i * 7},${70 + i * 4},${110 + i * 3},.5)`;
          ctx.fillRect(0, 0, w, h);
          for (let y = 0; y < h; y += 256)
            for (let x = 0; x < w; x += 256) {
              ctx.fillStyle = `rgba(${((x / 256) * 17 + i * 11) % 255},${((y / 256) * 29 + i * 7) % 255},180,.45)`;
              ctx.fillRect(x + 7, y + 7, 220, 220);
            }
        });
        await doc.transact("fixture", (tx) => (target = tx.addRasterFromAsset(a, { name: "Layer " + i })));
      }
      return { doc, target };
    },
  },
  sparse: {
    kind: "raster",
    // A whole-document export at scale 1 exceeds the SDK's 512 MiB export operation limit.
    exportScale: 0.5,
    // 8000 x 8000 (64 M pixels, the largest whole-document PNG export), painted in
    // 24 small spots: most of the canvas has no tiles.
    async build(engine) {
      const doc = await engine.createDocument({ width: 8000, height: 8000 });
      let target;
      await doc.transact("fixture", (tx) => (target = tx.addRaster({ name: "Sparse" })));
      for (let i = 0; i < 24; i++) {
        const x = 300 + ((i * 2741) % 7300),
          y = 300 + ((i * 1777) % 7300);
        const s = doc.tools.begin("brush", { layer: target, tip: { size: 120 }, color: [((i * 37) % 255) / 255, 0.4, 0.7, 1] });
        for (let k = 0; k < 6; k++) s.input({ x: x + k * 30, y: y + Math.sin(k) * 40, pressure: 1 });
        await s.end();
      }
      return { doc, target, center: [4000, 4000] };
    },
  },
  fx: {
    kind: "raster",
    // Semi-transparency with effects: opacity, blend modes, a blurred layer, colour
    // adjustment, an isolated translucent group and a levels adjustment layer.
    async build(engine) {
      const doc = await engine.createDocument({ width: 3840, height: 2160 });
      const assets = [];
      for (let i = 0; i < 6; i++)
        assets.push(
          await rasterImage(engine, 3840, 2160, (ctx, w, h) => {
            const g = ctx.createLinearGradient(0, 0, w, h);
            g.addColorStop(0, `rgba(${30 + i * 35},${200 - i * 25},${90 + i * 20},.7)`);
            g.addColorStop(1, `rgba(${200 - i * 20},${40 + i * 30},${160},.35)`);
            ctx.fillStyle = g;
            ctx.fillRect(0, 0, w, h);
            for (let k = 0; k < 40; k++) {
              ctx.fillStyle = `rgba(${(k * 53 + i * 31) % 255},${(k * 97) % 255},${(k * 13 + 80) % 255},.6)`;
              ctx.beginPath();
              ctx.arc((k * 433 + i * 211) % w, (k * 271 + i * 157) % h, 60 + ((k * 7) % 120), 0, Math.PI * 2);
              ctx.fill();
            }
          }),
        );
      let target;
      await doc.transact("fixture", (tx) => {
        const blends = ["normal", "multiply", "screen", "overlay", "soft_light", "normal"];
        const group = tx.addGroup({ isolated: true, name: "Isolated" });
        for (let i = 0; i < 6; i++) {
          const id = tx.addRasterFromAsset(assets[i], { name: "FX " + i, parent: i >= 3 ? group : undefined });
          const effects =
            i === 1 ? [{ type: "gaussian_blur", radius: 8 }] : i === 4 ? [{ type: "color_adjust", brightness: 0.1, contrast: 1.2, saturation: 0.7 }] : [];
          tx.setProps(id, { opacity: 0.55 + i * 0.07, blend: blends[i], effects });
          if (i === 5) target = id;
        }
        tx.setProps(group, { opacity: 0.8 });
        tx.addAdjustment({ type: "levels", in_black: 0.05, in_white: 0.95, gamma: 1.1 }, { name: "Levels" });
      });
      return { doc, target };
    },
  },
  "text-vector": {
    kind: "vector",
    // On main before plan E's first fix a cold whole-canvas display took about 61 s
    // here (17 s after it), so the slow rows keep fewer samples (no p95 for them).
    reps: { cold: 3, commit: 5, open: 3, recover: 3 },
    // Mixed text and vector content: 8 vector layers of 125 paths and 24 text layers.
    async build(engine) {
      const doc = await engine.createDocument({ width: 3840, height: 2160 });
      const fonts = await Promise.all(FONTS.map(async (u) => engine.assets.importFont(await (await fetch(u)).blob())));
      await doc.transact("fonts", (tx) => fonts.forEach((f) => tx.addFont(f)));
      const shapes = await Promise.all(
        Array.from({ length: 5 }, (_, i) => engine.vectors.parseSvg(`M${100 + i * 3} 100Q600 20 800 600L100 800Z`)),
      );
      const groups = Array.from({ length: 8 }, (_, g) =>
        Array.from({ length: 125 }, (_, i) => ({
          data: shapes[i % shapes.length],
          fill: [0.2, 0.4, 0.8, 0.02],
          stroke: { color: [0.1, 0.5, 0.2, 0.5], width: 1.5, cap: "round", join: "round", miter_limit: 4 },
          fill_rule: "non_zero",
        })),
      );
      const layers = [];
      const only = this.only;
      await doc.transact("fixture", (tx) => {
        if (only !== "text") groups.forEach((paths, g) => {
          const id = tx.addVector(paths, { name: "Vector " + g });
          tx.setTransform(id, [1, 0, 0, 1, (g % 4) * 900 + 60, Math.floor(g / 4) * 1000 + 60]);
          layers.push(id);
        });
        if (only !== "vector") for (let t = 0; t < 24; t++) {
          const id = tx.addText({
            text: t % 2 ? "縦書きのテキスト 12" : "Alto engine text 0123",
            style: { fonts: [fonts[0].id, fonts[1].id], size: 48 + (t % 4) * 16, color: [0.1, 0.1, 0.1, 0.9], line_height: 1.4 },
            writing_mode: t % 2 ? "vertical_rl" : "horizontal_tb",
            frame: { kind: "point" },
          });
          tx.setTransform(id, [1, 0, 0, 1, 80 + (t % 6) * 620, 120 + Math.floor(t / 6) * 500]);
        }
      });
      if (only === "text") {
        let target;
        await doc.transact("target", (tx) => (target = tx.addVector([], { name: "Empty" })));
        return { doc, target, vector: { layers: [], groups: [] } };
      }
      return { doc, target: layers[0], vector: { layers, groups } };
    },
  },
};
// Halves of text-vector, for finding which content a cost belongs to (not part of the fixed set).
FIXTURES["diag-vector-only"] = { ...FIXTURES["text-vector"], only: "vector" };
FIXTURES["diag-text-only"] = { ...FIXTURES["text-vector"], only: "text" };
const STRESS = {
  // Near the limits: one fully painted 8000 x 8000 layer (the 64 M pixel export limit,
  // just under MAX_IMAGE_PIXELS for a layer), all tiles distinct.
  "stress-64mpx": {
    kind: "raster",
    exportScale: 0.5,
    async build(engine) {
      const doc = await engine.createDocument({ width: 8000, height: 8000 });
      const a = await rasterImage(engine, 8000, 8000, (ctx, w, h) => {
        for (let y = 0; y < h; y += 256)
          for (let x = 0; x < w; x += 256) {
            ctx.fillStyle = `rgb(${((x / 256) * 7) % 255},${((y / 256) * 11) % 255},${((x + y) / 256) % 255})`;
            ctx.fillRect(x, y, 256, 256);
            ctx.fillStyle = "rgba(255,255,255,.5)";
            ctx.fillRect(x + 16, y + 16, 64 + ((x / 256) % 8) * 16, 64);
          }
      });
      let target;
      await doc.transact("fixture", (tx) => (target = tx.addRasterFromAsset(a, { name: "Full" })));
      return { doc, target, center: [4000, 4000] };
    },
  },
};

/** One fixture through every measured path. All durations are milliseconds. */
async function measure(engine, name, fixture, log, reps, publish) {
  const settle = () => unstable(engine).settle();
  const ratio = unstable(engine).pixelRatio();
  if (new URLSearchParams(location.search).has("profile")) await testing(engine).request("setProfiling", { enabled: true });
  const built = performance.now();
  let { doc, target, center, vector } = await fixture.build(engine);
  const buildMs = performance.now() - built;
  await doc.durable();
  const { width, height } = doc.getSnapshot().meta;
  center ??= [width / 2, height / 2];
  const canvas = [engine.element.width, engine.element.height];
  // Zooms are physical: zoom 1 maps one document pixel to one device pixel.
  const fit = Math.min(canvas[0] / width, canvas[1] / height);
  const views = { "zoom 1": 1 / ratio, fit: fit / ratio };
  const out = { fixture: name, kind: fixture.kind, width, height, buildMs, views, reps, complete: false, rows: {}, memory: {} };
  // The report holds this fixture from here on, so a failure or a timeout later keeps the finished rows.
  publish(out);
  const row = (id, samples, extra = {}) => {
    out.rows[id] = { samples, ...stats(samples), ...extra };
    publish(out);
    const r = out.rows[id];
    log(`${name} | ${id}: n=${r.n} median=${r.median.toFixed(1)} min=${r.min.toFixed(1)} max=${r.max.toFixed(1)}${r.p95 !== null ? " p95=" + r.p95.toFixed(1) : ""}`);
  };
  const view = async (zoom) => {
    engine.viewport.set({ center, zoom, rotation: 0 });
    return settle();
  };
  out.memory.afterBuild = await memory(engine, await view(views.fit));

  // Cold first display: empty display caches, with and without the decoded CPU tiles.
  for (const [label, zoom] of Object.entries(views))
    for (const cpu of [false, true]) {
      const samples = [];
      let frame;
      for (let i = 0; i < reps.cold; i++) {
        await view(zoom);
        await testing(engine).request("resetRenderCache", { cpu });
        const t = performance.now();
        frame = await view(zoom);
        samples.push(performance.now() - t);
      }
      await testing(engine).request("enablePrefetch");
      // How the last cold frame was reached: Worker ticks, the longest tick, tiles drawn.
      const { ticks, maxWorkerTickMs, composed, profile } = frame;
      row(`cold ${cpu ? "CPU+GPU" : "GPU"} first display (${label})`, samples, { lastFrame: { ticks, maxWorkerTickMs, composed, profile } });
    }
  if (reps.coldOnly) {
    out.complete = true;
    return out;
  }

  // Warm continuous operation: panning at zoom 1.
  await view(views["zoom 1"]);
  await settle();
  {
    const samples = [];
    for (let i = 0; i < reps.pan; i++) {
      const t = performance.now();
      engine.viewport.set({ center: [center[0] + Math.sin(i * 0.07) * 100, center[1] + Math.cos(i * 0.07) * 60] });
      await settle();
      samples.push(performance.now() - t);
      await raf();
    }
    row("warm pan frame (zoom 1)", samples);
  }
  engine.viewport.set({ center, zoom: views["zoom 1"] });
  await settle();

  if (fixture.kind === "raster") {
    // Continuous brush input to GPU-complete present.
    const s = doc.tools.begin("brush", { layer: target, tip: { size: 100 }, color: [1, 0, 0, 1] });
    const samples = [];
    for (let i = 0; i < reps.brush; i++) {
      const t = performance.now();
      s.input({ x: center[0] - 120 + i * 8, y: center[1] + Math.sin(i * 0.2) * 40, pressure: 1 });
      await s.flush();
      await settle();
      samples.push(performance.now() - t);
      await raf();
    }
    const t = performance.now();
    await s.end();
    await settle();
    row("warm brush input-to-present", samples, { strokeEndMs: performance.now() - t });
    // Committing a small stroke: end() to redrawn, at both views.
    for (const [label, zoom] of Object.entries(views)) {
      await view(zoom);
      const commits = [];
      for (let i = 0; i < reps.commit; i++) {
        const s = doc.tools.begin("brush", { layer: target, tip: { size: 24 }, color: [0, 1, 0, 1] });
        s.input({ x: center[0] - 200 + i * 20, y: center[1] - 80, pressure: 1 });
        await s.flush();
        await settle();
        const t = performance.now();
        await s.end();
        await settle();
        commits.push(performance.now() - t);
      }
      row(`commit small stroke to redrawn (${label})`, commits);
    }
  } else if (vector.layers.length) {
    // Vector edit commit: recolor one path in every vector layer (not in the text-only diagnostic half).
    for (const [label, zoom] of Object.entries(views)) {
      await view(zoom);
      const commits = [];
      for (let i = 0; i < reps.commit; i++) {
        for (const g of vector.groups) g[124] = { ...g[124], fill: [i % 2, 0.3 + i * 0.001 + zoom * 0.01, 0.8, 0.25] };
        const t = performance.now();
        await doc.transact("recolor", (tx) => vector.layers.forEach((id, j) => tx.setVector(id, vector.groups[j])));
        await settle();
        commits.push(performance.now() - t);
      }
      row(`commit vector recolor to redrawn (${label})`, commits);
    }
  }
  // A property edit that every content kind has.
  {
    await view(views.fit);
    const commits = [];
    for (let i = 0; i < reps.commit; i++) {
      const t = performance.now();
      await doc.transact("opacity", (tx) => tx.setProps(target, { opacity: 0.6 + (i % 2) * 0.3 }));
      await settle();
      commits.push(performance.now() - t);
    }
    row("commit opacity edit to redrawn (fit)", commits);
  }
  out.memory.afterEdits = await memory(engine, await settle());

  // PNG export of the whole document (scale 1 unless the fixture says otherwise);
  // decoded once to check its size.
  {
    const scale = fixture.exportScale ?? 1;
    const samples = [];
    let bytes = 0;
    for (let i = 0; i < reps.export; i++) {
      const t = performance.now();
      const png = await doc.exportImage({ scale });
      samples.push(performance.now() - t);
      bytes = png.size;
      if (i === 0) {
        const bitmap = await createImageBitmap(png);
        assert(bitmap.width === Math.round(width * scale) && bitmap.height === Math.round(height * scale), `export ${bitmap.width}x${bitmap.height}`);
        bitmap.close();
      }
    }
    row(`PNG export (scale ${scale})`, samples, { pngBytes: bytes });
    out.memory.afterExport = await memory(engine, await settle());
  }

  // Save (the bytes `save` writes) and open with its first display.
  const hash = await doc.snapshotHash();
  {
    const saves = [];
    let bytes;
    for (let i = 0; i < reps.save; i++) {
      const t = performance.now();
      bytes = await testing(engine).saveBytes();
      saves.push(performance.now() - t);
    }
    row("save .alto bytes", saves, { archiveBytes: bytes.length });
    const opens = [],
      firstDraws = [];
    for (let i = 0; i < reps.open; i++) {
      const t = performance.now();
      doc = await engine.open(new Blob([bytes]));
      opens.push(performance.now() - t);
      engine.viewport.set({ center, zoom: views.fit, rotation: 0 });
      await settle();
      firstDraws.push(performance.now() - t);
      assert((await doc.snapshotHash()) === hash, "the reopened document differs");
    }
    row("open .alto", opens);
    row("open .alto + first display (fit)", firstDraws);
  }

  // Recovery: a Worker restart replays the OPFS journal, then draws. Each round
  // adds a few journal records after the checkpoint the open wrote.
  {
    const samples = [],
      records = [];
    for (let i = 0; i < reps.recover; i++) {
      for (let k = 0; k < 3; k++) await doc.transact("rename", (tx) => tx.setProps(target, { name: `r${i}.${k}` }));
      await doc.durable();
      const expected = await doc.snapshotHash();
      records.push((await testing(engine).recoveryStore()).records ?? null);
      const t = performance.now();
      await testing(engine).crash();
      await settle();
      samples.push(performance.now() - t);
      doc = engine.document;
      assert((await doc.snapshotHash()) === expected, "recovery lost durable edits");
    }
    row("recover (Worker restart + journal replay + first display)", samples, { journalRecords: records });
  }
  out.memory.final = await memory(engine, await settle());
  out.complete = true;
  return out;
}

export async function runPerfTests(engine, log, options = {}) {
  const reps = { cold: 10, pan: 120, brush: 30, commit: 20, export: 5, save: 5, open: 5, recover: 5, ...options.reps };
  for (const [key, count] of Object.entries(reps))
    assert(key === "coldOnly" || (Number.isInteger(count) && count >= 1), `invalid reps.${key}: ${count}`);
  const selected = options.fixtures?.length ? options.fixtures : Object.keys(FIXTURES).filter((n) => !n.startsWith("diag-"));
  const fixtures = [
    ...selected.map((n) => [n, FIXTURES[n], false]),
    ...(options.stress ? Object.entries(STRESS).map(([n, f]) => [n, f, true]) : []),
  ];
  const report = {
    date: new Date().toISOString(),
    userAgent: navigator.userAgent,
    crossOriginIsolated,
    viewport: {
      width: innerWidth,
      height: innerHeight,
      dpr: devicePixelRatio,
      pixelRatio: unstable(engine).pixelRatio(),
      canvasPixels: [engine.element.width, engine.element.height],
    },
    reps,
    p95MinSamples: P95_MIN,
    notes: [
      "Durations in ms, from the SDK call to unstable().settle() (GPU-complete frame).",
      "gpuAccounted* are the engine's allocation accounting, not physical VRAM; wasmBytes is the engine WASM memory; JS heap is the main thread, uaMemory includes Workers.",
    ],
    fixtures: [],
    results: [],
  };
  // Written after every row: verify-browser reads it even when the suite fails or times out.
  const flush = () => {
    document.querySelector("#test-results").dataset.report = JSON.stringify(report);
  };
  // A failing fixture is recorded and the next one still runs; the suite fails at the end.
  for (const [name, fixture, stress] of fixtures) {
    log("FIXTURE " + name + (stress ? " (stress)" : ""));
    const started = performance.now();
    let partial = null;
    try {
      assert(fixture, "unknown fixture " + name);
      const own = stress ? { cold: 3, pan: 30, brush: 10, commit: 5, export: 2, save: 2, open: 2, recover: 2 } : fixture.reps;
      // An explicit ALTO_PERF_REPS wins over a fixture's own counts.
      const publish = (out) => {
        if (!partial) {
          partial = out;
          out.stress = stress;
          report.fixtures.push(out);
        }
        flush();
      };
      await measure(engine, name, fixture, log, { ...reps, ...own, ...options.reps }, publish);
      report.results.push({ name, pass: true, ms: performance.now() - started });
      flush();
    } catch (e) {
      if (partial) partial.error = String(e?.stack || e);
      report.results.push({ name, pass: false, error: String(e?.stack || e) });
      log("FAIL " + name + " " + e);
      try {
        flush();
      } catch (flushError) {
        log("FAIL report " + flushError);
      }
    }
  }
  const failed = report.results.filter((r) => !r.pass).map((r) => r.name);
  if (failed.length) throw Error("perf fixtures failed: " + failed.join(", "));
  log("PERF COMPLETE");
  return report;
}
