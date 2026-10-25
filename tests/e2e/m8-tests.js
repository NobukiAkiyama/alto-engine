// M8: version commits, semantic layer diffs, restore, history in .alto and the folder form.
import { MemoryObjectProvider, FileSystemDirectoryProvider } from "../../packages/alto-sdk/dist/index.js";
const assert = (v, m) => { if (!v) throw Error(m); };
const same = (a, b) => a.length === b.length && a.every((v, i) => v === b[i]);
const frame = async (e) => { await e.request("validateFrame"); await e.unstable.settle(); };
async function stroke(doc, layer, x, y, color) {
  const s = doc.tools.begin("brush", { layer, tip: { size: 60, hardness: 0.9, spacing: 0.1 }, color });
  for (let i = 0; i < 8; i++) s.input({ x: x + i * 12, y, pressure: 1 });
  await s.end();
}
export async function runM8Tests(engine, log) {
  const results = [], metrics = {};
  const test = async (name, fn) => { const s = performance.now(); await fn(); results.push({ name, pass: true, ms: performance.now() - s }); log("PASS " + name); };
  let doc, bg, ink, group, v1, v2, pixels1, pixels2;
  try {
    await test("commit / intent summary / log / status / empty commit refused", async () => {
      doc = await engine.createDocument({ width: 512, height: 512 });
      await doc.transact("レイヤー追加", (tx) => { bg = tx.addRaster({ name: "背景" }); ink = tx.addRaster({ name: "線画" }); });
      await stroke(doc, bg, 40, 60, [0.2, 0.4, 0.9, 1]);
      v1 = await doc.versions.commit("下地", { author: "tester" });
      pixels1 = await engine.unstable.readTile(0, 0);
      assert(v1.parents.length === 0 && v1.author === "tester", "first commit");
      assert(doc.view.versions.head === v1.hash && !doc.view.versions.changed, "status after commit");
      let refused = false;
      try { await doc.versions.commit("空"); } catch (e) { refused = e.code === "INVALID_ARGUMENT"; }
      assert(refused, "empty commit accepted");
      for (let i = 0; i < 3; i++) await stroke(doc, ink, 200, 200 + i * 30, [0, 0, 0, 1]);
      await doc.undo();
      await doc.transact("不透明度", (tx) => tx.setProps(bg, { opacity: 0.5 }));
      assert(doc.view.versions.changed && doc.view.versions.changes === 2, "working changes " + JSON.stringify(doc.view.versions));
      v2 = await doc.versions.commit("線画を描く");
      pixels2 = await engine.unstable.readTile(0, 0);
      const brush = v2.intents.find((i) => i.intent === "Brush stroke");
      assert(brush && brush.count === 2 && brush.target_names[0] === "線画", "intent summary " + JSON.stringify(v2.intents));
      const log = await doc.versions.log();
      assert(log.length === 2 && log[0].hash === v2.hash && log[0].parents[0] === v1.hash, "log");
      assert(doc.view.dirty, "a new commit is unsaved");
    });
    await test("semantic diff reads layer by layer (pixels, props, moves, text, assets)", async () => {
      const d = await doc.versions.diff(v1.hash, v2.hash);
      const px = d.items.find((i) => i.type === "pixels_changed");
      assert(px && px.id === ink && px.name === "線画" && px.tiles >= 1 && px.document_bounds.x0 === 0, "pixels " + JSON.stringify(d.items));
      assert(d.items.some((i) => i.type === "props_changed" && i.id === bg && i.fields.join() === "opacity"), "props");
      await doc.transact("group", (tx) => { group = tx.addGroup({ name: "人物", isolated: true }); tx.moveNode(ink, group, 0); });
      const w = await doc.versions.diff(v2.hash, "working");
      assert(w.items.some((i) => i.type === "added" && i.id === group), "added group");
      assert(w.items.some((i) => i.type === "moved" && i.id === ink && i.to[0] === group && w.names[i.to[0]] === "人物"), "move " + JSON.stringify(w.items));
      assert(!w.items.some((i) => i.type === "pixels_changed"), "moving is not a pixel change");
      metrics.diff = { v1v2: d.items, working: w.items };
    });
    await test("restore an earlier version as one undoable edit", async () => {
      await doc.versions.restore(v1.hash);
      await frame(engine);
      assert(same(pixels1, await engine.unstable.readTile(0, 0)), "restored pixels");
      assert((await doc.versions.diff(v1.hash, "working")).items.length === 0, "restored document");
      assert((await doc.versions.log()).length === 2, "history is not rewritten");
      await doc.undo();
      await doc.undo();
      assert((await doc.versions.diff(v2.hash, "working")).items.length === 0, "undo restore");
      assert(same(pixels2, await engine.unstable.readTile(0, 0)), "undo pixels");
    });
    await test(".alto keeps history; old versions restore after reopening", async () => {
      const bytes = await engine.unstable.saveBytes();
      doc = await engine.openDocument(new Blob([bytes]));
      const log = await doc.versions.log();
      assert(log.map((c) => c.hash).join() === [v2.hash, v1.hash].join(), "reopened log");
      await doc.versions.restore(v1.hash);
      assert(same(pixels1, await engine.unstable.readTile(0, 0)), "reopened restore pixels");
      await doc.undo();
      metrics.zipBytes = bytes.length;
    });
    await test("folder form in OPFS: incremental save, manifest last, reopen, conflict detection", async () => {
      const name = "alto-m8-" + Date.now();
      const root = await navigator.storage.getDirectory();
      const provider = new FileSystemDirectoryProvider(await root.getDirectoryHandle(name, { create: true }));
      try {
        const first = await doc.saveFolder(provider);
        assert(!doc.view.dirty, "saved state");
        const files = [...(await provider.list(""))];
        for (const f of ["manifest.json", "document.json", ".gitattributes"]) assert(f === ".gitattributes" ? !!(await provider.readRef(f)) : files.includes(f), "missing " + f);
        assert(files.filter((f) => f.startsWith("commits/")).length === 2, "commits " + files);
        assert(files.filter((f) => f.startsWith("snapshots/")).length >= 2, "snapshots");
        const manifest = JSON.parse(new TextDecoder().decode(await provider.readRef("manifest.json")));
        assert(manifest.head === v2.hash && manifest.format === 1, "manifest head");
        const docJson = new TextDecoder().decode(await provider.readRef("document.json"));
        assert(docJson.split("\n").length > 50 && docJson.includes('"name": "線画"'), "readable document.json");
        await doc.transact("名前", (tx) => tx.setProps(bg, { name: "背景（明）" }));
        const v3 = await doc.versions.commit("名前を変更");
        const second = await doc.saveFolder(provider);
        assert(second.skipped >= first.written - 2 && second.written <= 3, `incremental ${JSON.stringify(first)} -> ${JSON.stringify(second)}`);
        const hash = (await doc.snapshotHash());
        doc = await engine.openFolder(provider);
        assert((await doc.snapshotHash()) === hash, "folder snapshot");
        assert((await doc.versions.log()).map((c) => c.hash).join() === [v3.hash, v2.hash, v1.hash].join(), "folder log");
        await doc.versions.restore(v1.hash);
        assert(same(pixels1, await engine.unstable.readTile(0, 0)), "folder restore");
        await doc.undo();
        // Another writer changed the folder: the next save is rejected and nothing is overwritten.
        await provider.writeRef("manifest.json", new TextEncoder().encode('{"format":1}'));
        let conflict = false;
        try { await doc.saveFolder(provider); } catch (e) { conflict = e.code === "CONFLICT"; }
        assert(conflict, "stale manifest overwritten");
        assert(new TextDecoder().decode(await provider.readRef("manifest.json")) === '{"format":1}', "manifest changed by failed save");
        metrics.folder = { first, second, files: files.length };
      } finally {
        await root.removeEntry(name, { recursive: true });
      }
      const memory = new MemoryObjectProvider();
      await doc.saveFolder(memory);
      const reopened = await engine.openFolder(memory);
      assert((await reopened.versions.log()).length === 3, "memory provider");
      doc = reopened;
    });
    await test("commits survive a Worker crash (checkpoint) and journal recovery", async () => {
      await stroke(doc, bg, 300, 400, [1, 0, 0, 1]);
      const c = await doc.versions.commit("赤い線");
      const after = await engine.unstable.readTile(1, 1);
      // The commit's checkpoint is written after the reply.
      await doc.durable();
      await engine.unstable.crash();
      doc = engine.document;
      const log = await doc.versions.log();
      assert(log[0].hash === c.hash && log.length === 4, "commit lost in crash");
      assert(same(after, await engine.unstable.readTile(1, 1)), "pixels after recovery");
      await frame(engine);
    });
  } catch (error) {
    results.push({ name: String(error.stack || error), pass: false });
    log("FAIL " + error);
    throw error;
  } finally {
    const report = { date: new Date().toISOString(), milestone: "M8", userAgent: navigator.userAgent, crossOriginIsolated, viewport: { width: innerWidth, height: innerHeight, dpr: devicePixelRatio }, results, metrics };
    document.querySelector("#test-results").dataset.report = JSON.stringify(report);
  }
  log("M8 COMPLETE");
  return results;
}
