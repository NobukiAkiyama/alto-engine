// Plan F / QA-4: a long run of seeded edits, recorded once a minute.
// Run with `node scripts/verify-browser.mjs --suite=soak`. ALTO_SOAK_MINUTES (default 240)
// sets the length and ALTO_SOAK_SEED (default 1) the edit sequence, so a run can be repeated.
//
// The edits mix brush strokes, Undo/Redo, transforms, effect changes, text at many sizes
// (glyph atlas pressure), pans and zooms over the whole canvas (many input tiles), version
// commits and saves. Every minute it records the engine's WASM memory and GPU accounting
// (allocation estimates, not physical VRAM), the main thread JS heap, the recovery journal
// size (the active generation, apart from the objects and packs the versions keep), the history depth, and B1/B3-style p95s (frame and input-to-present, GPU-complete).
// At the end it saves, reopens and compares the document, tiles, export and versions.
//
// The suite fails on an error, a GPU loss, an engine failure or a mismatch after reopening.
// Whether memory and p95s stay flat is judged from the report (`verdict`), which keeps
// every sample so the judgement can be redone.
import { unstable } from "../../packages/alto-sdk/dist/unstable.js";
import { OpfsFolderProvider } from "../../packages/alto-sdk/dist/index.js";

const assert = (ok, m) => {
  if (!ok) throw Error(m);
};
const raf = () => new Promise(requestAnimationFrame);
const FONTS = ["../../tests/fonts/AltoTestLatin.ttf", "../../tests/fonts/AltoTestJP.otf"];
const TEXTS = ["Alto engine text 0123", "縦書きのテキスト 12", "Soak 4567 テキスト", "engine 89 縦書き"];
const MIB = 1048576;

/** mulberry32: a small seeded generator, so a run can be repeated exactly. */
function generator(seed) {
  let a = seed >>> 0;
  const next = () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  return {
    next,
    int: (lo, hi) => lo + Math.floor(next() * (hi - lo + 1)),
    pick: (list) => list[Math.floor(next() * list.length)],
  };
}

const p95 = (samples) => {
  const s = [...samples].sort((a, b) => a - b);
  return s[Math.ceil(s.length * 0.95) - 1];
};
const median = (values) => {
  const s = [...values].sort((a, b) => a - b);
  return s.length % 2 ? s[(s.length - 1) / 2] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2;
};
async function sha256(blob) {
  const digest = await crypto.subtle.digest("SHA-256", await blob.arrayBuffer());
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}

async function build(engine) {
  const W = 3840;
  const H = 2160;
  const doc = await engine.createDocument({ width: W, height: H });
  const c = new OffscreenCanvas(W, H);
  const ctx = c.getContext("2d");
  for (let y = 0; y < H; y += 256)
    for (let x = 0; x < W; x += 256) {
      ctx.fillStyle = `rgb(${(x / 256) * 13 % 255},${(y / 256) * 23 % 255},150)`;
      ctx.fillRect(x, y, 256, 256);
    }
  const image = await engine.assets.importImage(await c.convertToBlob());
  const fonts = await Promise.all(FONTS.map(async (u) => engine.assets.importFont(await (await fetch(u)).blob())));
  const shape = await engine.vectors.parseSvg("M100 100Q600 20 800 600L100 800Z");
  const ids = { raster: [], text: [], vector: [], adjust: null, group: null };
  await doc.transact("soak fixture", (tx) => {
    fonts.forEach((f) => tx.addFont(f));
    ids.raster.push(tx.addRasterFromAsset(image, { name: "Base" }));
    for (let i = 0; i < 3; i++) ids.raster.push(tx.addRaster({ name: "Paint " + i }));
    ids.group = tx.addGroup({ name: "Effects", isolated: true });
    ids.raster.push(tx.addRaster({ name: "Paint in group", parent: ids.group }));
    ids.adjust = tx.addAdjustment({ type: "gaussian_blur", radius: 2 }, { name: "Blur", parent: ids.group });
    for (let t = 0; t < 6; t++) {
      const id = tx.addText({
        text: TEXTS[t % TEXTS.length],
        style: { fonts: [fonts[0].id, fonts[1].id], size: 32 + t * 12, color: [0.1, 0.1, 0.1, 0.9], line_height: 1.4 },
        writing_mode: t % 2 ? "vertical_rl" : "horizontal_tb",
        frame: { kind: "point" },
      });
      tx.setTransform(id, [1, 0, 0, 1, 200 + t * 560, 300 + (t % 3) * 500]);
      ids.text.push(id);
    }
    const v = tx.addVector(
      [{ data: shape, fill: [0.2, 0.4, 0.8, 0.3], stroke: { color: [0.1, 0.5, 0.2, 0.8], width: 3, cap: "round", join: "round", miter_limit: 4 }, fill_rule: "non_zero" }],
      { name: "Vector" },
    );
    ids.vector.push(v);
  });
  ids.fonts = fonts.map((f) => f.id);
  return { doc, ids, W, H };
}

/** What a saved `.alto` holds: objects split by which snapshot references them. */
export async function archiveBreakdown(blob) {
  const buf = new Uint8Array(await blob.arrayBuffer());
  const dv = new DataView(buf.buffer);
  let eocd = -1;
  for (let i = buf.length - 22; i >= Math.max(0, buf.length - 65557); i--) {
    if (dv.getUint32(i, true) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) throw new Error("archiveBreakdown: no end of central directory");
  const count = dv.getUint16(eocd + 10, true);
  let at = dv.getUint32(eocd + 16, true);
  const text = new TextDecoder();
  const entries = [];
  for (let n = 0; n < count; n++) {
    if (dv.getUint32(at, true) !== 0x02014b50) throw new Error("archiveBreakdown: central directory");
    if (dv.getUint16(at + 10, true) !== 0) throw new Error("archiveBreakdown: expected stored entries");
    const size = dv.getUint32(at + 20, true);
    const nameLen = dv.getUint16(at + 28, true);
    const extraLen = dv.getUint16(at + 30, true);
    const commentLen = dv.getUint16(at + 32, true);
    const local = dv.getUint32(at + 42, true);
    const name = text.decode(buf.subarray(at + 46, at + 46 + nameLen));
    const start = local + 30 + dv.getUint16(local + 26, true) + dv.getUint16(local + 28, true);
    entries.push({ name, size, data: () => text.decode(buf.subarray(start, start + size)) });
    at += 46 + nameLen + extraLen + commentLen;
  }
  const hashes = (json) => new Set(json.match(/(?<![0-9a-f])[0-9a-f]{64}(?![0-9a-f])/g) ?? []);
  const manifest = JSON.parse(entries.find((e) => e.name === "manifest.json").data());
  const current = String(manifest.snapshot).match(/[0-9a-f]{64}/)[0];
  let live = new Set();
  const inVersions = new Set();
  const out = { objects: 0, objectBytes: 0, snapshots: 0, snapshotBytes: 0, commits: 0, commitBytes: 0 };
  for (const e of entries) {
    if (e.name.startsWith("snapshots/")) {
      out.snapshots++;
      out.snapshotBytes += e.size;
      const refs = hashes(e.data());
      if (e.name.includes(current)) live = refs;
      else for (const h of refs) inVersions.add(h);
    } else if (e.name.startsWith("commits/")) {
      out.commits++;
      out.commitBytes += e.size;
    }
  }
  if (live.size === 0) throw new Error("archiveBreakdown: the current snapshot references nothing");
  const split = { live: [0, 0], versionsOnly: [0, 0], unreferenced: [0, 0] };
  for (const e of entries) {
    if (!e.name.startsWith("objects/")) continue;
    out.objects++;
    out.objectBytes += e.size;
    const h = e.name.split("/").pop();
    const k = live.has(h) ? "live" : inVersions.has(h) ? "versionsOnly" : "unreferenced";
    split[k][0]++;
    split[k][1] += e.size;
  }
  out.split = split;
  return out;
}

/** Runs the soak and returns the report. `publish` receives it after every sample. */
export async function runSoak(engine, log, options = {}) {
  const minutes = options.minutes ?? 240;
  const seed = options.seed ?? 1;
  const sampleMs = options.sampleMs ?? 60000;
  // A gap between edits, about three a second: heavy use, not a script's full speed.
  const gapMs = options.gapMs ?? 250;
  const rng = generator(seed);
  const settle = () => unstable(engine).settle();
  const ratio = unstable(engine).pixelRatio();
  const report = {
    suite: "soak",
    minutes,
    seed,
    gapMs,
    startedAt: new Date().toISOString(),
    notes:
      "wasmBytes is the engine WASM memory; gpuAccountedBytes is the engine's allocation accounting, not physical VRAM; mainJsHeap is the main thread only. journalBytes is the OPFS recovery journal of this engine's namespace.",
    samples: [],
    saves: [],
    events: { gpu: [], status: [], errors: [] },
    ops: {},
    results: [],
  };
  const flush = () => {
    document.querySelector("#test-results").dataset.report = JSON.stringify(report);
  };
  const offGpu = unstable(engine).onGpu((g) => {
    report.events.gpu.push({ t: performance.now(), event: g });
    log("GPU " + g);
  });
  const offStatus = engine.on("status", ({ state, error }) => report.events.status.push({ t: performance.now(), state, error: error?.code ?? null }));
  const offError = engine.on("error", (e) => report.events.errors.push({ t: performance.now(), code: e?.code ?? String(e) }));
  // The namespace's OPFS folder: the two journal files apart from the objects and packs
  // the versions keep (those grow with every version and are not the journal).
  const folder = "ns-" + Array.from(new TextEncoder().encode(engine.namespace), (b) => b.toString(16).padStart(2, "0")).join("");
  // Packs are deleted mid-session, so a file listed here can be gone when it is read.
  const gone = (e) => {
    if (e?.name === "NotFoundError") return 0;
    throw e;
  };
  const sizeOf = async (dir) => {
    let bytes = 0;
    for await (const [, handle] of dir.entries())
      bytes += handle.kind === "file" ? await handle.getFile().then((f) => f.size, gone) : await sizeOf(handle);
    return bytes;
  };
  const storage = async () => {
    const dir = await (await (await navigator.storage.getDirectory()).getDirectoryHandle("alto-engine")).getDirectoryHandle(folder);
    // The journal is two files: the active generation (what recovery replays) and the
    // previous one, kept until the next checkpoint overwrites it.
    const out = { journal: 0, activeJournal: 0, objects: 0, packs: 0, other: 0 };
    let newest = -1;
    for await (const [name, handle] of dir.entries()) {
      const file = handle.kind === "file" ? await handle.getFile() : null;
      const bytes = file ? file.size : await sizeOf(handle);
      if (/^journal-[ab]\.bin$/.test(name)) {
        out.journal += bytes;
        const head = new DataView(await file.slice(0, 12).arrayBuffer());
        const generation = head.byteLength === 12 ? head.getUint32(8, true) : -1;
        if (generation > newest) {
          newest = generation;
          out.activeJournal = bytes;
        }
      }
      else if (name === "objects" || name === "packs") out[name] += bytes;
      else out.other += bytes;
    }
    return out;
  };
  const journalBytes = async () => (await storage()).activeJournal;

  // ALTO_SOAK_THIN: each save writes a thin `.alto` and keeps the versions in an OPFS folder
  // of its own (outside "alto-engine", so the journal accounting above is not touched).
  // Emptied at the start so a run can be repeated. Both sizes are recorded, apart.
  const THIN_DIR = "alto-soak-thin";
  let store = null;
  let storeHandle = null;
  if (options.thin) {
    const root = await navigator.storage.getDirectory();
    await root.removeEntry(THIN_DIR, { recursive: true }).catch((e) => {
      if (e?.name !== "NotFoundError") throw e;
    });
    storeHandle = await root.getDirectoryHandle(THIN_DIR, { create: true });
    store = await OpfsFolderProvider.open(storeHandle);
    report.thin = { directory: THIN_DIR };
  }
  const storeBytes = () => sizeOf(storeHandle);

  const { doc, ids, W, H } = await build(engine);
  await doc.durable();
  const fit = Math.min(engine.element.width / W, engine.element.height / H) / ratio;
  const zoom1 = 1 / ratio;
  const count = (name) => (report.ops[name] = (report.ops[name] ?? 0) + 1);

  // One seeded edit. Each waits for its GPU-complete frame, as a user's next action would.
  const step = async () => {
    const r = rng.next();
    if (r < 0.34) {
      const layer = rng.pick(ids.raster);
      const stroke = doc.tools.begin("brush", {
        layer,
        color: [rng.next(), rng.next(), rng.next(), 0.4 + rng.next() * 0.6],
        tip: { size: rng.int(4, 160) },
      });
      let x = rng.int(0, W);
      let y = rng.int(0, H);
      for (let i = rng.int(6, 30); i > 0; i--) {
        x = Math.max(0, Math.min(W, x + rng.int(-120, 120)));
        y = Math.max(0, Math.min(H, y + rng.int(-120, 120)));
        stroke.input({ x, y, pressure: 0.3 + rng.next() * 0.7 });
      }
      await stroke.end();
      count("brush");
    } else if (r < 0.48) {
      for (let i = rng.int(1, 3); i > 0 && doc.getSnapshot().history.undo; i--) await doc.undo();
      count("undo");
    } else if (r < 0.56) {
      for (let i = rng.int(1, 2); i > 0 && doc.getSnapshot().history.redo; i--) await doc.redo();
      count("redo");
    } else if (r < 0.66) {
      // Text and vector layers only: moving a painted raster layer around and painting it
      // again widens its bounds until the image size limit refuses the stroke.
      const id = rng.pick([...ids.text, ...ids.vector]);
      await doc.transact("move", (tx) =>
        tx.setTransform(id, { x: rng.int(-400, W - 400), y: rng.int(-200, H - 200), scaleX: 0.5 + rng.next(), scaleY: 0.5 + rng.next(), rotation: (rng.next() - 0.5) * 0.6 }),
      );
      count("transform");
    } else if (r < 0.74) {
      const kind = rng.int(0, 2);
      await doc.transact("effect", (tx) => {
        if (kind === 0) tx.setAdjustment(ids.adjust, { type: "gaussian_blur", radius: rng.int(0, 24) });
        else if (kind === 1) tx.setAdjustment(ids.adjust, { type: "color_adjust", brightness: rng.next() - 0.5, contrast: 0.5 + rng.next(), saturation: 0.5 + rng.next() });
        else tx.setProps(ids.group, { opacity: 0.2 + rng.next() * 0.8 });
      });
      count("effect");
    } else if (r < 0.84) {
      const id = rng.pick(ids.text);
      const fonts = ids.fonts;
      await doc.transact("text", (tx) =>
        tx.setText(id, {
          text: rng.pick(TEXTS) + " " + rng.int(0, 9999),
          style: { fonts, size: rng.int(6, 240), color: [rng.next(), rng.next(), rng.next(), 1], line_height: 1.2 + rng.next() * 0.6 },
          writing_mode: rng.next() < 0.5 ? "vertical_rl" : "horizontal_tb",
          frame: { kind: "point" },
        }),
      );
      count("text");
    } else {
      const z = rng.next() < 0.3 ? fit : zoom1 * (0.25 + rng.next() * 3);
      engine.viewport.set({ center: [rng.int(0, W), rng.int(0, H)], zoom: z, rotation: rng.next() < 0.2 ? (rng.next() - 0.5) * 0.5 : 0 });
      count("view");
    }
    await settle();
  };
  // Saves and version commits come on a clock, as a person's would, not per edit.
  const save = async () => {
    await doc.durable();
    const before = await journalBytes();
    const started = performance.now();
    let bytes = 0;
    let blob = null;
    const wasmBefore = options.inspect ? (await settle()).wasmBytes : null;
    const target = {
      write: async (stream) => {
        blob = await new Response(stream).blob();
        bytes = blob.size;
      },
    };
    if (store) await doc.save(target, { history: { folder: store } });
    else await doc.save(target);
    await doc.durable();
    const after = await journalBytes();
    const entry = { t: performance.now(), ms: performance.now() - started, archiveBytes: bytes, journalBefore: before, journalAfter: after };
    if (store) {
      entry.storeBytes = await storeBytes();
      log(`SOAK thin save alto=${(bytes / 1024).toFixed(0)}KiB store=${(entry.storeBytes / 1024).toFixed(0)}KiB versions=${report.ops.commit ?? 0}`);
    }
    if (options.inspect) {
      entry.wasmBefore = wasmBefore;
      entry.wasmAfter = (await settle()).wasmBytes;
      try {
        entry.contents = await archiveBreakdown(blob);
      } catch (e) {
        entry.contents = { error: String(e) };
      }
      entry.versions = report.ops.commit ?? 0;
      log(`SOAK save ${(bytes / MIB).toFixed(1)}MiB wasm ${(wasmBefore / MIB).toFixed(0)}->${(entry.wasmAfter / MIB).toFixed(0)}MiB ${JSON.stringify(entry.contents.split ?? entry.contents)}`);
    }
    blob = null;
    report.saves.push(entry);
    count("save");
  };
  const commit = async () => {
    await doc.versions.commit("soak " + (report.ops.commit ?? 0), { author: "soak", allowEmpty: true });
    count("commit");
  };

  // B1/B3-style p95s, as the benchmark measures them, on the current document.
  const probe = async () => {
    engine.viewport.set({ center: [W / 2, H / 2], zoom: zoom1, rotation: 0 });
    await settle();
    const frames = [];
    for (let i = 0; i < 120; i++) {
      const start = performance.now();
      engine.viewport.set({ center: [W / 2 + Math.sin(i * 0.07) * 100, H / 2 + Math.cos(i * 0.07) * 60] });
      await settle();
      frames.push(performance.now() - start);
      await raf();
    }
    const stroke = doc.tools.begin("brush", { layer: ids.raster[1], tip: { size: 100 }, color: [1, 0, 0, 1] });
    const inputs = [];
    for (let i = 0; i < 30; i++) {
      const start = performance.now();
      stroke.input({ x: W / 2 - 120 + i * 8, y: H / 2 + Math.sin(i * 0.2) * 40, pressure: 1 });
      await stroke.flush();
      await settle();
      inputs.push(performance.now() - start);
      await raf();
    }
    await stroke.end();
    count("probe-stroke");
    return { b1: p95(frames), b3: p95(inputs) };
  };

  const sample = async (elapsedMs) => {
    const { b1, b3 } = await probe();
    const frame = await settle();
    const snap = doc.getSnapshot();
    const s = {
      minute: Math.round(elapsedMs / 60000),
      elapsedMs,
      wasmBytes: frame.wasmBytes,
      gpuAccountedBytes: frame.gpuBytes,
      gpuAccounted: frame.gpu,
      mainJsHeapUsedBytes: performance.memory?.usedJSHeapSize ?? null,
      storage: await storage(),
      journalBytes: 0,
      history: snap.history,
      versions: (await doc.versions.log()).length ?? null,
      archive: report.saves.at(-1)?.archiveBytes ?? null,
      ...(store ? { store: await storeBytes() } : {}),
      b1P95: b1,
      b3P95: b3,
      ops: { ...report.ops },
    };
    s.journalBytes = s.storage.activeJournal;
    s.tiles = (await Promise.all(ids.raster.map(async (id) => Object.keys((await doc.nodeTiles(id)).tiles ?? {}).length))).reduce((a, b) => a + b, 0);
    report.samples.push(s);
    flush();
    log(
      `SOAK m${s.minute} wasm=${(s.wasmBytes / MIB).toFixed(1)}MiB gpu=${(s.gpuAccountedBytes / MIB).toFixed(1)}MiB journal=${(s.journalBytes / 1024).toFixed(0)}KiB undo=${s.history.undo} b1=${b1.toFixed(1)} b3=${b3.toFixed(1)} ops=${Object.values(report.ops).reduce((a, b) => a + b, 0)}` +
        (store ? ` alto=${((s.archive ?? 0) / 1024).toFixed(0)}KiB store=${(s.store / 1024).toFixed(0)}KiB` : ""),
    );
  };

  let failure = null;
  const started = performance.now();
  try {
    await sample(0);
    let nextSample = sampleMs;
    let nextSave = options.saveMs ?? 120000;
    let nextCommit = options.commitMs ?? 600000;
    // A person stops now and then; the engine checkpoints the journal after 2 s idle.
    let nextPause = rng.int(30000, 90000);
    while (performance.now() - started < minutes * 60000) {
      await step();
      await new Promise((r) => setTimeout(r, gapMs));
      if (performance.now() - started >= nextPause) {
        await new Promise((r) => setTimeout(r, 3000));
        count("pause");
        nextPause = performance.now() - started + rng.int(30000, 90000);
      }
      const now = performance.now() - started;
      if (now >= nextSave) {
        await save();
        nextSave += options.saveMs ?? 120000;
      }
      if (now >= nextCommit) {
        await commit();
        nextCommit += options.commitMs ?? 600000;
      }
      const lost = report.events.gpu.some((e) => e.event === "lost");
      const failed = report.events.status.some((e) => e.state === "failed" || e.state === "storage-failed");
      assert(!lost, "the GPU was lost during the soak");
      assert(!failed, "the engine failed during the soak");
      if (performance.now() - started >= nextSample) {
        await sample(performance.now() - started);
        nextSample += sampleMs;
      }
    }
    if (report.samples.at(-1).elapsedMs < performance.now() - started - 1000) await sample(performance.now() - started);
    report.results.push({ name: "run", pass: true, ms: performance.now() - started });

    // End: save, reopen and compare the document, every node's tiles, the export and the versions.
    await doc.durable();
    const before = {
      hash: await doc.snapshotHash(),
      tiles: Object.fromEntries(await Promise.all(ids.raster.map(async (id) => [id, JSON.stringify(await doc.nodeTiles(id))]))),
      png: await sha256(await doc.exportImage()),
      versions: JSON.stringify(await doc.versions.log({ mode: "dag", allBranches: true })),
    };
    let archive;
    const finalTarget = { write: async (stream) => (archive = await new Response(stream).blob()) };
    if (store) await doc.save(finalTarget, { history: { folder: store } });
    else await doc.save(finalTarget);
    const reopened = await (store ? engine.open(archive, { history: store }) : engine.open(archive));
    await settle();
    const after = {
      hash: await reopened.snapshotHash(),
      tiles: Object.fromEntries(await Promise.all(ids.raster.map(async (id) => [id, JSON.stringify(await reopened.nodeTiles(id))]))),
      png: await sha256(await reopened.exportImage()),
      versions: JSON.stringify(await reopened.versions.log({ mode: "dag", allBranches: true })),
    };
    const tilesSame = ids.raster.every((id) => before.tiles[id] === after.tiles[id]);
    report.reopen = {
      archiveBytes: archive.size,
      ...(store ? { storeBytes: await storeBytes() } : {}),
      hash: [before.hash, after.hash],
      hashSame: before.hash === after.hash,
      tilesSame,
      pngSame: before.png === after.png,
      versionsSame: before.versions === after.versions,
      versionCount: JSON.parse(after.versions).length ?? null,
    };
    const ok = report.reopen.hashSame && tilesSame && report.reopen.pngSame && report.reopen.versionsSame;
    report.results.push({ name: "reopen", pass: ok, detail: report.reopen });
    assert(ok, "the reopened document differs: " + JSON.stringify(report.reopen));
  } catch (e) {
    failure = e;
    report.results.push({ name: "soak", pass: false, error: String(e?.stack || e) });
    log("FAIL soak " + e);
  } finally {
    offGpu();
    offStatus();
    offError();
    report.verdict = verdict(report);
    report.finishedAt = new Date().toISOString();
    flush();
  }
  if (failure) throw failure;
  log("SOAK COMPLETE " + JSON.stringify(report.verdict));
  return report;
}

/**
 * QA-4's checks from the samples. The history is full from the first sample where
 * `history.undo` reached its cap (100); memory is compared from 15 minutes after that,
 * so a cache that fills once is not counted as growth. Memory grows when the last
 * quarter's maximum exceeds the first post-warm-up quarter's by more than 5% (and
 * 1 MiB). B1/B3 compare the median of the first five samples' p95 with the last five.
 */
export function verdict(report) {
  const s = report.samples;
  if (s.length < 8) return { judged: false, reason: "fewer than 8 samples" };
  const full = s.findIndex((x) => x.history.undo >= 100);
  const from = full < 0 ? -1 : s.findIndex((x) => x.elapsedMs >= s[full].elapsedMs + 15 * 60000);
  const out = { judged: true, historyFullAtMinute: full < 0 ? null : s[full].minute };
  if (from >= 0 && s.length - from >= 8) {
    const tail = s.slice(from);
    const q = Math.floor(tail.length / 4);
    const grow = (key) => {
      const first = Math.max(...tail.slice(0, q).map((x) => x[key]));
      const last = Math.max(...tail.slice(-q).map((x) => x[key]));
      return { firstQuarterMaxMiB: first / MIB, lastQuarterMaxMiB: last / MIB, grows: last > first * 1.05 + MIB };
    };
    out.wasm = grow("wasmBytes");
    out.gpuAccounted = grow("gpuAccountedBytes");
  } else out.memory = "not judged: too few samples after the history was full";
  const first = median(s.slice(0, 5).flatMap((x) => [x.b1P95]));
  const last = median(s.slice(-5).map((x) => x.b1P95));
  const first3 = median(s.slice(0, 5).map((x) => x.b3P95));
  const last3 = median(s.slice(-5).map((x) => x.b3P95));
  out.b1 = { initial: first, final: last, worse20: last > first * 1.2 };
  out.b3 = { initial: first3, final: last3, worse20: last3 > first3 * 1.2 };
  out.journal = {
    saves: report.saves.length,
    // A save checkpoints: the journal is then one checkpoint, not every edit since the last one.
    shrankOnEverySave: report.saves.every((x) => x.journalAfter <= x.journalBefore),
    maxBytes: Math.max(...s.map((x) => x.journalBytes)),
  };
  out.gpuLost = report.events.gpu.filter((e) => e.event === "lost").length;
  out.restarts = report.events.status.filter((e) => e.state === "restarting").length;
  out.errors = report.events.errors.length;
  return out;
}
