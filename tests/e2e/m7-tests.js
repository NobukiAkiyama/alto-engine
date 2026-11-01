// M7: text, Japanese vertical layout, font fallback, OpenType features and selection,
// checked on GPU-rendered pixels through the SDK.
import { unstable } from "../../packages/alto-sdk/dist/unstable.js";
import { testing } from "../../packages/alto-sdk/dist/testing.js";
const assert = (v, m) => { if (!v) throw Error(m); };
const same = (a, b) => a.length === b.length && a.every((v, i) => v === b[i]);
const FONTS = ["../../tests/fonts/AltoTestJP.otf", "../../tests/fonts/AltoTestLatin.ttf"];
const frame = async (e) => { await testing(e).request("validateFrame"); await unstable(e).settle(); };
/** Ink bounding box (alpha > 96) of a document region, read from level-0 display tiles. */
async function ink(e, x0, y0, x1, y1) {
  const tiles = new Map();
  let box = null, count = 0, rgb = [0, 0, 0];
  for (let y = Math.max(0, Math.floor(y0)); y < y1; y++) for (let x = Math.max(0, Math.floor(x0)); x < x1; x++) {
    const key = `${x >> 8},${y >> 8}`;
    if (!tiles.has(key)) tiles.set(key, await unstable(e).readTile(x >> 8, y >> 8));
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
  const setup = async (w = 512, h = 512, renderVersion) => {
    doc = await engine.createDocument({ width: w, height: h, renderVersion });
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
      await addText({ ...vertical("Aあ😀", [latin.id, jp.id]), writing_mode: "horizontal_tb" }, 20, 20);
      const info = await doc.text.info(id);
      assert(info.fonts_used[0] === latin.id && info.fonts_used[1] === jp.id, "fallback order");
      assert(info.missing.length === 1 && info.missing[0] === 2, "missing glyph report");
      // Text without fonts is still visible as placeholder boxes.
      await doc.transact("no fonts", (tx) => tx.setText(id, { ...doc.getSnapshot().nodes.get(id).content, text: "あ", style: { ...doc.getSnapshot().nodes.get(id).content.style, fonts: [] } }));
      await frame(engine);
      assert((await ink(engine, 20, 20, 140, 200)).count > 500, "placeholder box missing");
      let refused = false;
      try { await doc.transact("remove font", (tx) => { tx.ops.push({ type: "set_asset", id: jp.id, record: null }); tx.setText(id, vertical("縦", [jp.id])); }); } catch { refused = true; }
      assert(refused, "font removed while in use");
    });
    await test("vertical kuten / touten / brackets / long vowel sit where Japanese typesetting puts them", async () => {
      await setup(512, 900);
      // Column x 50..200 (centre 125), cells of 100 px from y = 50.
      await addText(vertical("あ。い、う「ー」", [jp.id]));
      const cell = (i) => ink(engine, 50, 50 + i * 100, 200, 150 + i * 100);
      const kuten = (await cell(1)).box, touten = (await cell(3)).box, open = (await cell(5)).box, bar = (await cell(6)).box;
      for (const [name, b] of [["。", kuten], ["、", touten]]) assert(b && b[0] > 125, `${name} not in the right half: ${b}`);
      assert(kuten[3] - 150 < 50, "。 must sit in the top half of its cell");
      assert(touten[3] - 350 < 50, "、 must sit in the top half of its cell");
      assert(open[1] - 550 > 40, "「 must sit at the foot of its cell: " + open);
      assert(bar[3] - bar[1] > 3 * (bar[2] - bar[0]), "ー must be vertical: " + bar);
      // Control: the same characters in horizontal text sit in the lower left.
      await doc.transact("horizontal", (tx) => tx.setText(id, { ...vertical("あ。", [jp.id]), writing_mode: "horizontal_tb" }));
      await frame(engine);
      const h = (await ink(engine, 300, 50, 400, 200)).box;
      assert(h && h[0] < 345 && h[1] > 100, "horizontal 。 control: " + h);
      metrics.punctuation = { kuten, touten, open, bar, horizontalKuten: h };
    });
    await test("tate-chu-yoko: automatic digits, explicit ranges compressed to one em, three digits rotate", async () => {
      await setup(768, 768);
      await addText(vertical("第12回", [latin.id, jp.id]), 300, 40);
      const tcy = await ink(engine, 150, 140, 300, 240);
      assert(tcy.box && tcy.box[2] - tcy.box[0] <= 100 && tcy.box[3] - tcy.box[1] < 100, "digits not in one cell: " + tcy.box);
      assert(Math.abs((tcy.box[0] + tcy.box[2]) / 2 - 225) < 6, "tcy not centred on the column");
      const info = await doc.text.info(id);
      assert(info.lines.length === 1 && info.glyphs === 4, "glyph count " + info.glyphs);
      await doc.text.setTateChuYoko(id, 0, 0, false);
      await doc.transact("explicit", (tx) => tx.setText(id, { ...vertical("記録2026年", [latin.id, jp.id]), tcy: [{ start: 2, end: 6 }] }));
      await frame(engine);
      const wide = await ink(engine, 150, 240, 300, 340);
      assert(wide.box && wide.box[2] - wide.box[0] <= 101, "explicit range not compressed: " + wide.box);
      await doc.transact("three", (tx) => tx.setText(id, vertical("第123回", [latin.id, jp.id])));
      await frame(engine);
      const rotated = await ink(engine, 150, 140, 300, 300);
      assert(rotated.box[3] - rotated.box[1] > 1.5 * (rotated.box[2] - rotated.box[0]), "123 should run down the column: " + rotated.box);
      metrics.tcy = { auto: tcy.box, explicit: wide.box, rotated: rotated.box };
    });
    await test("Latin runs rotate in vertical text and OpenType features change shaping", async () => {
      await setup(768, 768);
      await addText(vertical("縦ABC", [latin.id, jp.id]), 300, 40);
      const latinInk = await ink(engine, 150, 140, 300, 460);
      assert(latinInk.box[3] - latinInk.box[1] > 2 * (latinInk.box[2] - latinInk.box[0]), "Latin not rotated: " + latinInk.box);
      const width = async (content) => { await doc.text.set(id, content); const i = await doc.text.info(id); return i.bounds[2] - i.bounds[0]; };
      const base = { ...vertical("「あ、い」", [jp.id]), writing_mode: "horizontal_tb" };
      const plain = await width(base);
      const palt = await width({ ...base, style: { ...base.style, features: [{ tag: "palt", value: 1 }] } });
      assert(palt < plain - 50, `palt ${palt} vs ${plain}`);
      const fwid = await width({ ...base, text: "12", style: { ...base.style, features: [{ tag: "fwid", value: 1 }] } });
      assert(Math.abs(fwid - 200) < 0.01, "fwid " + fwid);
      const liga = { ...base, text: "fi", style: { ...base.style, fonts: [latin.id] } };
      await doc.text.set(id, liga);
      assert((await doc.text.info(id)).glyphs === 1, "fi ligature");
      await doc.text.set(id, { ...liga, style: { ...liga.style, features: [{ tag: "liga", value: 0 }] } });
      assert((await doc.text.info(id)).glyphs === 2, "liga=0");
      metrics.features = { plain, palt, fwid };
    });
    await test("selection: hit test / caret / rectangles / motion through layer transforms and surrogate pairs", async () => {
      await setup(1024, 768);
      await doc.transact("text", (tx) => { id = tx.addText({ text: "あいう\nえお𠮷", style: { fonts: [jp.id], size: 100, line_height: 1.5 }, writing_mode: "horizontal_tb", frame: { kind: "point" } }); tx.setTransform(id, [2, 0, 0, 2, 100, 50]); });
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
      await doc.text.set(id, { ...doc.getSnapshot().nodes.get(id).content, writing_mode: "vertical_rl" });
      assert(await doc.text.hitTest(id, 100 - 2 * 75, 50 + 2 * 160) === 2, "vertical hit");
      assert(await doc.text.navigate(id, 1, "next_line") === 5, "vertical next column");
    });
    await test("editing: replace / span style / tate-chu-yoko / Undo-Redo pixels", async () => {
      await setup();
      await addText(vertical("縦書き10月", [latin.id, jp.id]));
      const before = await unstable(engine).readTile(0, 0);
      await doc.text.replace(id, 2, 3, "組み");
      assert(doc.getSnapshot().nodes.get(id).content.text === "縦書組み10月", "replace");
      await doc.text.style(id, 0, 1, { color: [1, 0, 0, 1] });
      await frame(engine);
      const red = await ink(engine, 50, 50, 200, 150);
      assert(red.rgb[0] > 150 && red.rgb[1] < 60, "span colour " + red.rgb);
      const spans = doc.getSnapshot().nodes.get(id).content.spans;
      assert(spans.length === 1 && spans[0].end === 1, "span");
      await doc.text.replace(id, 0, 0, "「");
      assert(doc.getSnapshot().nodes.get(id).content.spans[0].start === 1, "span shifted");
      const edited = await unstable(engine).readTile(0, 0);
      for (let i = 0; i < 3; i++) await doc.undo();
      assert(same(before, await unstable(engine).readTile(0, 0)), "Undo pixels");
      for (let i = 0; i < 3; i++) await doc.redo();
      assert(same(edited, await unstable(engine).readTile(0, 0)), "Redo pixels");
      let failed = false;
      try { await doc.text.set(id, { ...doc.getSnapshot().nodes.get(id).content, spans: [{ start: 3, end: 1, style: {} }] }); } catch { failed = true; }
      assert(failed, "invalid span accepted");
    });
    await test("zoomed text keeps exact outlines (levels -4 .. 2)", async () => {
      await setup();
      await addText(vertical("永", [jp.id]), 200, 50);
      const rows = [];
      for (const level of [-4, -2, 0, 2]) {
        const scale = 2 ** level, cx = 125 / scale, cy = 100 / scale;
        const t = await unstable(engine).readTileLevel(level, Math.floor(cx / 256), Math.floor(cy / 256));
        let solid = 0, partial = 0;
        for (let i = 3; i < t.length; i += 4) { if (t[i] === 255) solid++; else if (t[i] > 0) partial++; }
        assert(solid > 0 && partial > 0, `level ${level}: solid ${solid} partial ${partial}`);
        rows.push({ level, solid, partial });
      }
      metrics.zoom = rows;
    });
    await test("ZIP / journal recovery / GPU loss keep fonts, text and pixels", async () => {
      const hash = (await doc.snapshotHash()), before = await unstable(engine).readTile(0, 0);
      doc = await engine.open(new Blob([await testing(engine).saveBytes()]));
      assert((await doc.snapshotHash()) === hash, "ZIP snapshot");
      assert(same(before, await unstable(engine).readTile(0, 0)), "ZIP pixels");
      await doc.text.replace(id, 0, 1, "東");
      const after = await unstable(engine).readTile(0, 0), saved = (await doc.snapshotHash());
      await doc.durable();
      await testing(engine).crash();
      doc = engine.document;
      assert(saved === (await doc.snapshotHash()) && same(after, await unstable(engine).readTile(0, 0)), "journal");
      await testing(engine).destroyGpu();
      await new Promise((resolve, reject) => { const off = unstable(engine).onGpu((g) => { if (g !== "restored") return; off(); resolve(); }); setTimeout(() => reject(Error("GPU recovery timeout")), 15000); });
      assert(same(after, await unstable(engine).readTile(0, 0)), "GPU rebuild");
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
