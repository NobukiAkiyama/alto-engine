// M7: text, Japanese vertical layout, font fallback, OpenType features and selection,
// checked on GPU-rendered pixels through the SDK.
const assert = (v, m) => { if (!v) throw Error(m); };
const same = (a, b) => a.length === b.length && a.every((v, i) => v === b[i]);
const FONTS = ["../../tests/fonts/AltoTestJP.otf", "../../tests/fonts/AltoTestLatin.ttf"];
const frame = async (e) => { await e.request("validateFrame"); await e.unstable.settle(); };
/** Ink bounding box (alpha > 96) of a document region, read from level-0 display tiles. */
async function ink(e, x0, y0, x1, y1) {
  const tiles = new Map();
  let box = null, count = 0, rgb = [0, 0, 0];
  for (let y = Math.max(0, Math.floor(y0)); y < y1; y++) for (let x = Math.max(0, Math.floor(x0)); x < x1; x++) {
    const key = `${x >> 8},${y >> 8}`;
    if (!tiles.has(key)) tiles.set(key, await e.unstable.readTile(x >> 8, y >> 8));
    const t = tiles.get(key), i = (((y & 255) * 256) + (x & 255)) * 4;
    if (t[i + 3] > 96) {
      box = box ? [Math.min(box[0], x), Math.min(box[1], y), Math.max(box[2], x + 1), Math.max(box[3], y + 1)] : [x, y, x + 1, y + 1];
      count++;
      for (let k = 0; k < 3; k++) rgb[k] += t[i + k];
    }
  }
  return { box, count, rgb: rgb.map((v) => (count ? v / count : 0)) };
}
async function fonts(engine) {
  return Promise.all(FONTS.map(async (u) => engine.assets.importFont(await (await fetch(u)).blob())));
}
function vertical(text, fonts, extra = {}) {
  return { text, style: { fonts, size: 100, color: [0, 0, 0, 1], line_height: 1.5 }, writing_mode: "vertical_rl", frame: { kind: "point" }, ...extra };
}
export async function runM7Tests(engine, log) {
  const results = [], metrics = {};
  const test = async (name, fn) => { const s = performance.now(); await fn(); results.push({ name, pass: true, ms: performance.now() - s }); log("PASS " + name); };
  let doc, id, jp, latin;
  const setup = async (w = 512, h = 512) => {
    doc = await engine.createDocument({ width: w, height: h });
    [jp, latin] = await fonts(engine);
    await doc.transact("fonts", (tx) => { tx.addFont(jp); tx.addFont(latin); });
  };
  const addText = async (content, x = 200, y = 50) => {
    await doc.transact("text", (tx) => { id = tx.addText(content); tx.setTransform(id, [1, 0, 0, 1, x, y]); });
    await frame(engine);
    return id;
  };
  try {
    await test("font import / OS/2 fsType / vertical support / fallback order / placeholder", async () => {
      await setup();
      assert(jp.info.family === "Noto Sans JP" && jp.info.has_vertical_alternates && jp.warnings.length === 0, "font info");
      assert(latin.info.outline === "truetype" && !latin.info.has_vertical_alternates, "latin info");
      let bad = false;
      try { await engine.assets.importFont(new Blob([new Uint8Array([1, 2, 3])])); } catch { bad = true; }
      assert(bad, "invalid font accepted");
      await addText({ ...vertical("Aあ😀", [latin.record.id, jp.record.id]), writing_mode: "horizontal_tb" }, 20, 20);
      const info = await doc.text.info(id);
      assert(info.fonts_used[0] === latin.record.id && info.fonts_used[1] === jp.record.id, "fallback order");
      assert(info.missing.length === 1 && info.missing[0] === 2, "missing glyph report");
      // Text without fonts is still visible as placeholder boxes.
      await doc.transact("no fonts", (tx) => tx.setText(id, { ...doc.view.node(id).kind.content, text: "あ", style: { ...doc.view.node(id).kind.content.style, fonts: [] } }));
      await frame(engine);
      assert((await ink(engine, 20, 20, 140, 200)).count > 500, "placeholder box missing");
      let refused = false;
      try { await doc.transact("remove font", (tx) => { tx.ops.push({ type: "set_asset", id: jp.record.id, record: null }); tx.setText(id, vertical("縦", [jp.record.id])); }); } catch { refused = true; }
      assert(refused, "font removed while in use");
    });
    await test("vertical kuten / touten / brackets / long vowel sit where Japanese typesetting puts them", async () => {
      await setup(512, 900);
      // Column x 50..200 (centre 125), cells of 100 px from y = 50.
      await addText(vertical("あ。い、う「ー」", [jp.record.id]));
      const cell = (i) => ink(engine, 50, 50 + i * 100, 200, 150 + i * 100);
      const kuten = (await cell(1)).box, touten = (await cell(3)).box, open = (await cell(5)).box, bar = (await cell(6)).box;
      for (const [name, b] of [["。", kuten], ["、", touten]]) assert(b && b[0] > 125, `${name} not in the right half: ${b}`);
      assert(kuten[3] - 150 < 50, "。 must sit in the top half of its cell");
      assert(touten[3] - 350 < 50, "、 must sit in the top half of its cell");
      assert(open[1] - 550 > 40, "「 must sit at the foot of its cell: " + open);
      assert(bar[3] - bar[1] > 3 * (bar[2] - bar[0]), "ー must be vertical: " + bar);
      // Control: the same characters in horizontal text sit in the lower left.
      await doc.transact("horizontal", (tx) => tx.setText(id, { ...vertical("あ。", [jp.record.id]), writing_mode: "horizontal_tb" }));
      await frame(engine);
      const h = (await ink(engine, 300, 50, 400, 200)).box;
      assert(h && h[0] < 345 && h[1] > 100, "horizontal 。 control: " + h);
      metrics.punctuation = { kuten, touten, open, bar, horizontalKuten: h };
    });
    await test("tate-chu-yoko: automatic digits, explicit ranges compressed to one em, three digits rotate", async () => {
      await setup(768, 768);
      await addText(vertical("第12回", [latin.record.id, jp.record.id]), 300, 40);
      const tcy = await ink(engine, 150, 140, 300, 240);
      assert(tcy.box && tcy.box[2] - tcy.box[0] <= 100 && tcy.box[3] - tcy.box[1] < 100, "digits not in one cell: " + tcy.box);
      assert(Math.abs((tcy.box[0] + tcy.box[2]) / 2 - 225) < 6, "tcy not centred on the column");
      const info = await doc.text.info(id);
      assert(info.lines.length === 1 && info.glyphs === 4, "glyph count " + info.glyphs);
      await doc.text.setTateChuYoko(id, 0, 0, false);
      await doc.transact("explicit", (tx) => tx.setText(id, { ...vertical("記録2026年", [latin.record.id, jp.record.id]), tcy: [{ start: 2, end: 6 }] }));
      await frame(engine);
      const wide = await ink(engine, 150, 240, 300, 340);
      assert(wide.box && wide.box[2] - wide.box[0] <= 101, "explicit range not compressed: " + wide.box);
      await doc.transact("three", (tx) => tx.setText(id, vertical("第123回", [latin.record.id, jp.record.id])));
      await frame(engine);
      const rotated = await ink(engine, 150, 140, 300, 300);
      assert(rotated.box[3] - rotated.box[1] > 1.5 * (rotated.box[2] - rotated.box[0]), "123 should run down the column: " + rotated.box);
      metrics.tcy = { auto: tcy.box, explicit: wide.box, rotated: rotated.box };
    });
    await test("Latin runs rotate in vertical text and OpenType features change shaping", async () => {
      await setup(768, 768);
      await addText(vertical("縦ABC", [latin.record.id, jp.record.id]), 300, 40);
      const latinInk = await ink(engine, 150, 140, 300, 460);
      assert(latinInk.box[3] - latinInk.box[1] > 2 * (latinInk.box[2] - latinInk.box[0]), "Latin not rotated: " + latinInk.box);
      const width = async (content) => { await doc.text.set(id, content); const i = await doc.text.info(id); return i.bounds[2] - i.bounds[0]; };
      const base = { ...vertical("「あ、い」", [jp.record.id]), writing_mode: "horizontal_tb" };
      const plain = await width(base);
      const palt = await width({ ...base, style: { ...base.style, features: [{ tag: "palt", value: 1 }] } });
      assert(palt < plain - 50, `palt ${palt} vs ${plain}`);
      const fwid = await width({ ...base, text: "12", style: { ...base.style, features: [{ tag: "fwid", value: 1 }] } });
      assert(Math.abs(fwid - 200) < 0.01, "fwid " + fwid);
      const liga = { ...base, text: "fi", style: { ...base.style, fonts: [latin.record.id] } };
      await doc.text.set(id, liga);
      assert((await doc.text.info(id)).glyphs === 1, "fi ligature");
      await doc.text.set(id, { ...liga, style: { ...liga.style, features: [{ tag: "liga", value: 0 }] } });
      assert((await doc.text.info(id)).glyphs === 2, "liga=0");
      metrics.features = { plain, palt, fwid };
    });
    await test("selection: hit test / caret / rectangles / motion through layer transforms and surrogate pairs", async () => {
      await setup(1024, 768);
      await doc.transact("text", (tx) => { id = tx.addText({ text: "あいう\nえお𠮷", style: { fonts: [jp.record.id], size: 100, line_height: 1.5 }, writing_mode: "horizontal_tb", frame: { kind: "point" } }); tx.setTransform(id, [2, 0, 0, 2, 100, 50]); });
      assert(await doc.text.hitTest(id, 100 + 2 * 140, 50 + 2 * 60) === 1, "hit before mid");
      assert(await doc.text.hitTest(id, 100 + 2 * 160, 50 + 2 * 60) === 2, "hit after mid");
      assert(await doc.text.hitTest(id, 100 + 2 * 10, 50 + 2 * 200) === 4, "second line");
      const caret = await doc.text.caret(id, 5);
      assert(caret.line === 1 && caret.a[0] === 300 && caret.a[1] === 350, "caret " + JSON.stringify(caret));
      const rects = await doc.text.selectionRects(id, 1, 5);
      assert(rects.length === 2 && rects[0][0][0] === 300 && rects[0][1][0] === 700, "rects " + JSON.stringify(rects));
      assert(await doc.text.navigate(id, 6, "forward") === 8, "surrogate pair forward");
      assert(await doc.text.navigate(id, 8, "backward") === 6, "surrogate pair backward");
      assert(await doc.text.navigate(id, 1, "next_line") === 5, "next line");
      await doc.text.set(id, { ...doc.view.node(id).kind.content, writing_mode: "vertical_rl" });
      assert(await doc.text.hitTest(id, 100 - 2 * 75, 50 + 2 * 160) === 2, "vertical hit");
      assert(await doc.text.navigate(id, 1, "next_line") === 5, "vertical next column");
    });
    await test("editing: replace / span style / tate-chu-yoko / Undo-Redo pixels", async () => {
      await setup();
      await addText(vertical("縦書き10月", [latin.record.id, jp.record.id]));
      const before = await engine.unstable.readTile(0, 0);
      await doc.text.replace(id, 2, 3, "組み");
      assert(doc.view.node(id).kind.content.text === "縦書組み10月", "replace");
      await doc.text.style(id, 0, 1, { color: [1, 0, 0, 1] });
      await frame(engine);
      const red = await ink(engine, 50, 50, 200, 150);
      assert(red.rgb[0] > 150 && red.rgb[1] < 60, "span colour " + red.rgb);
      const spans = doc.view.node(id).kind.content.spans;
      assert(spans.length === 1 && spans[0].end === 1, "span");
      await doc.text.replace(id, 0, 0, "「");
      assert(doc.view.node(id).kind.content.spans[0].start === 1, "span shifted");
      const edited = await engine.unstable.readTile(0, 0);
      for (let i = 0; i < 3; i++) await doc.undo();
      assert(same(before, await engine.unstable.readTile(0, 0)), "Undo pixels");
      for (let i = 0; i < 3; i++) await doc.redo();
      assert(same(edited, await engine.unstable.readTile(0, 0)), "Redo pixels");
      let failed = false;
      try { await doc.text.set(id, { ...doc.view.node(id).kind.content, spans: [{ start: 3, end: 1, style: {} }] }); } catch { failed = true; }
      assert(failed, "invalid span accepted");
    });
    await test("small text: size-banded glyph atlas matches vector coverage, zoom switches back to vector, GPU loss rebuilds", async () => {
      await setup();
      const both = [latin.record.id, jp.record.id];
      const small = (text, size, extra = {}) => ({ text, style: { fonts: both, size, color: [0, 0, 0, 1], line_height: 1.5 }, writing_mode: "horizontal_tb", frame: { kind: "point" }, ...extra });
      const ids = {};
      const c = Math.cos(Math.PI / 6), s = Math.sin(Math.PI / 6);
      await doc.transact("small text", (tx) => {
        // One layer, two size runs: the 40 px span stays vector at level 0, the 12 px text uses the atlas.
        ids.horizontal = tx.addText(small("永あ漢字かな Abg12、。\n小さな文字 Text fig", 12, { spans: [{ start: 0, end: 1, style: { size: 40, color: [1, 0, 0, 1] } }] }));
        tx.setTransform(ids.horizontal, [1, 0, 0, 1, 10, 10]);
        ids.vertical = tx.addText(small("縦組み「小さい」10月", 10, { writing_mode: "vertical_rl" }));
        tx.setTransform(ids.vertical, [1, 0, 0, 1, 240, 90]);
        ids.rotated = tx.addText(small("回転した小さな文字 Rotate", 11));
        tx.setTransform(ids.rotated, [c, s, -s, c, 20, 120]);
      });
      await engine.unstable.setGlyphAtlas(true);
      await frame(engine);
      const atlasTile = await engine.unstable.readTile(0, 0);
      const stats = (await engine.unstable.vectorStats()).glyphAtlas;
      assert(stats.atlasGlyphs >= 40 && stats.entries >= 30 && stats.vectorRuns >= 1 && stats.fallbackGlyphs === 0, "atlas stats " + JSON.stringify(stats));
      await engine.unstable.setGlyphAtlas(false);
      await frame(engine);
      const vectorTile = await engine.unstable.readTile(0, 0);
      assert(!same(atlasTile, vectorTile), "atlas path not used at level 0");
      // Coverage of each layer's box: total ink, overlap (sum min / sum max), mean difference and ink bounds.
      // Coverage of each layer's box: total ink, overlap (sum min / sum max), mean difference,
      // ink bounds, and row/column ink profiles (where the glyphs land). The vector reference is
      // 4x MSAA (coverage in steps of 1/4) and the atlas exact-area, so thin strokes differ per pixel.
      const compare = async (id) => {
        const q = (await doc.text.info(id)).quad;
        const x0 = Math.max(0, Math.floor(Math.min(...q.map((p) => p[0]))) - 2), x1 = Math.min(256, Math.ceil(Math.max(...q.map((p) => p[0]))) + 2);
        const y0 = Math.max(0, Math.floor(Math.min(...q.map((p) => p[1]))) - 2), y1 = Math.min(256, Math.ceil(Math.max(...q.map((p) => p[1]))) + 2);
        let ia = 0, iv = 0, lo = 0, hi = 0, diff = 0, n = 0;
        const box = { a: [1e9, 1e9, -1, -1], v: [1e9, 1e9, -1, -1] };
        const rows = { a: new Array(y1 - y0).fill(0), v: new Array(y1 - y0).fill(0) }, cols = { a: new Array(x1 - x0).fill(0), v: new Array(x1 - x0).fill(0) };
        const grow = (b, x, y) => { b[0] = Math.min(b[0], x); b[1] = Math.min(b[1], y); b[2] = Math.max(b[2], x + 1); b[3] = Math.max(b[3], y + 1); };
        for (let y = y0; y < y1; y++) for (let x = x0; x < x1; x++) {
          const i = (y * 256 + x) * 4 + 3, a = atlasTile[i] / 255, v = vectorTile[i] / 255;
          ia += a; iv += v; lo += Math.min(a, v); hi += Math.max(a, v);
          rows.a[y - y0] += a; rows.v[y - y0] += v; cols.a[x - x0] += a; cols.v[x - x0] += v;
          if (a > 0.25) grow(box.a, x, y);
          if (v > 0.25) grow(box.v, x, y);
          if (a > 0 || v > 0) { diff += Math.abs(a - v); n++; }
        }
        const corr = (p, q) => {
          const m = (s) => s.reduce((t, v) => t + v, 0) / s.length, mp = m(p), mq = m(q);
          let sxy = 0, sxx = 0, syy = 0;
          p.forEach((v, i) => { sxy += (v - mp) * (q[i] - mq); sxx += (v - mp) ** 2; syy += (q[i] - mq) ** 2; });
          return sxy / Math.sqrt(sxx * syy);
        };
        const r = { ink: [ia, iv], ratio: ia / iv, overlap: lo / hi, meanDiff: diff / n, box, profiles: [corr(rows.a, rows.v), corr(cols.a, cols.v)] };
        assert(iv > 100, `${id}: no vector ink`);
        assert(Math.abs(r.ratio - 1) < 0.04, `ink ratio ${JSON.stringify(r)}`);
        assert(r.overlap > 0.7 && r.meanDiff < 0.15, `coverage ${JSON.stringify(r)}`);
        assert(r.profiles.every((c) => c > 0.97), `profiles ${JSON.stringify(r)}`);
        assert(box.a.every((v, k) => Math.abs(v - box.v[k]) <= 1), `ink bounds ${JSON.stringify(box)}`);
        return r;
      };
      const coverage = {};
      for (const k of Object.keys(ids)) coverage[k] = await compare(ids[k]);
      // Display levels measure the em themselves: at level -2 (12 px -> 48 output px) every run
      // is vector so both modes match exactly; at level 1 (6 px) the atlas is used again.
      const levels = { off: [await engine.unstable.readTileLevel(-2, 1, 0), await engine.unstable.readTileLevel(1, 0, 0)] };
      await engine.unstable.setGlyphAtlas(true);
      levels.on = [await engine.unstable.readTileLevel(-2, 1, 0), await engine.unstable.readTileLevel(1, 0, 0)];
      assert(same(levels.on[0], levels.off[0]), "level -2 must draw 48 px text as vectors");
      assert(!same(levels.on[1], levels.off[1]), "level 1 must draw 6 px text from the atlas");
      assert(same(atlasTile, await engine.unstable.readTile(0, 0)), "atlas output must be deterministic");
      // Device loss drops the atlas texture; entries are rasterized again from the outlines.
      await engine.unstable.destroyGpu();
      await new Promise((resolve, reject) => { const off = engine.on("gpuRestored", () => { off(); resolve(); }); setTimeout(() => reject(Error("GPU recovery timeout")), 15000); });
      assert(same(atlasTile, await engine.unstable.readTile(0, 0)), "atlas rebuilt after GPU loss");
      const rebuilt = (await engine.unstable.vectorStats()).glyphAtlas;
      assert(rebuilt.entries > 0 && rebuilt.rasterized > 0, "rebuilt stats " + JSON.stringify(rebuilt));
      await frame(engine);
      metrics.glyphAtlas = { stats, coverage, rebuilt };
    });
    await test("zoomed text keeps exact outlines (levels -4 .. 2)", async () => {
      await setup();
      await addText(vertical("永", [jp.record.id]), 200, 50);
      const rows = [];
      for (const level of [-4, -2, 0, 2]) {
        const scale = 2 ** level, cx = 125 / scale, cy = 100 / scale;
        const t = await engine.unstable.readTileLevel(level, Math.floor(cx / 256), Math.floor(cy / 256));
        let solid = 0, partial = 0;
        for (let i = 3; i < t.length; i += 4) { if (t[i] === 255) solid++; else if (t[i] > 0) partial++; }
        assert(solid > 0 && partial > 0, `level ${level}: solid ${solid} partial ${partial}`);
        rows.push({ level, solid, partial });
      }
      metrics.zoom = rows;
    });
    await test("ZIP / journal recovery / GPU loss keep fonts, text and pixels", async () => {
      const hash = (await doc.snapshotHash()), before = await engine.unstable.readTile(0, 0);
      doc = await engine.openDocument(new Blob([await engine.unstable.saveBytes()]));
      assert((await doc.snapshotHash()) === hash, "ZIP snapshot");
      assert(same(before, await engine.unstable.readTile(0, 0)), "ZIP pixels");
      await doc.text.replace(id, 0, 1, "東");
      const after = await engine.unstable.readTile(0, 0), saved = (await doc.snapshotHash());
      await doc.durable();
      await engine.unstable.crash();
      doc = engine.document;
      assert(saved === (await doc.snapshotHash()) && same(after, await engine.unstable.readTile(0, 0)), "journal");
      await engine.unstable.destroyGpu();
      await new Promise((resolve, reject) => { const off = engine.on("gpuRestored", () => { off(); resolve(); }); setTimeout(() => reject(Error("GPU recovery timeout")), 15000); });
      assert(same(after, await engine.unstable.readTile(0, 0)), "GPU rebuild");
      await frame(engine);
    });
  } catch (error) {
    results.push({ name: String(error.stack || error), pass: false });
    log("FAIL " + error);
    throw error;
  } finally {
    const report = { date: new Date().toISOString(), milestone: "M7", userAgent: navigator.userAgent, crossOriginIsolated, viewport: { width: innerWidth, height: innerHeight, dpr: devicePixelRatio }, results, metrics };
    document.querySelector("#test-results").dataset.report = JSON.stringify(report);
  }
  log("M7 COMPLETE");
  return results;
}
