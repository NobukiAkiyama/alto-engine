import { FileSystemDirectoryProvider, MemoryObjectProvider, OpfsFolderProvider } from "../../packages/alto-sdk/dist/index.js";
import { blobTarget, opfsFolder } from "../../packages/alto-sdk/dist/browser.js";
import { testing } from "../../packages/alto-sdk/dist/testing.js";
import { unstable } from "../../packages/alto-sdk/dist/unstable.js";
const assert = (v, m) => { if (!v) throw Error(m); };
const stable = (v) => Array.isArray(v) ? v.map(stable) : v && typeof v === "object" ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, stable(v[k])])) : v;
const eq = (a, b) => JSON.stringify(stable(a)) === JSON.stringify(stable(b));
async function reject(promise, code) { try { await promise; } catch (e) { assert(e.code === code, `${code} expected: ${e.code}: ${e.message}`); return; } throw Error(`expected ${code}`); }
export async function runImageVersionTests(engine, log) {
  const results = [];
  const test = async (name, fn) => { const started = performance.now(); await fn(); results.push({ name, pass: true, ms: performance.now() - started }); log("PASS " + name); };
  let d, layer;
  try {
    await test("CPU image edits / explicit transparent bounds / Undo / rendered pixels", async () => {
      d = await engine.createDocument({ width: 300, height: 100 });
      await d.transact("layer.add", (tx) => { layer = tx.addRaster({ bounds: { x: 254, y: 0, width: 3, height: 2 } }); });
      await d.image.fill(layer, [254, 0], [1, 0, 0, 1]);
      assert(eq((await d.image.pickColor(layer, [256, 1])).bytes, [255, 0, 0, 255]), "fill across tile boundary");
      await d.image.rotate(layer, 1); assert(eq(d.getSnapshot().nodes.get(layer).bounds, { x: 254, y: 0, width: 2, height: 3 }), "rotation bounds");
      await d.undo(); assert(d.getSnapshot().nodes.get(layer).bounds.width === 3, "Undo bounds");
      await d.image.crop(layer, { x: 255, y: 0, width: 2, height: 1 });
      await reject(d.image.pickColor(layer, [254, 0]), "NOT_FOUND");
      const before = await d.snapshotHash(); await reject(d.image.resize(layer, 65536, 65536), "INVALID_ARGUMENT"); assert(await d.snapshotHash() === before, "failed resize mutated document");
      await d.image.flip(layer, "horizontal"); await d.image.resize(layer, 4, 2, { filter: "nearest" });
      assert(eq((await d.image.pickColor(layer, [258, 1])).bytes, [255, 0, 0, 255]), "nearest resize");
      const pixel = await d.image.pickComposite([255, 0]); assert(eq(pixel.bytes, [255, 0, 0, 255]), "composite sample " + pixel.bytes);
      const saved = blobTarget(); await d.save(saved); const hash = await d.snapshotHash(); d = await engine.open(saved.blob); assert(await d.snapshotHash() === hash, "image save/reopen");
    });
    await test("selection-limited gradient / masks / SVG all-or-nothing", async () => {
      d = await engine.createDocument({ width: 8, height: 8 }); await d.transact("layer.add", (tx) => { layer = tx.addRaster(); });
      await d.selection.apply({ kind: "rectangle", x: 0, y: 0, width: 4, height: 8 });
      await d.image.gradient(layer, { kind: "linear", start: [0, 0], end: [8, 0], stops: [{ offset: 0, color: [1, 0, 0, 0] }, { offset: 1, color: [0, 1, 0, 1] }] });
      assert((await d.image.pickColor(layer, [2, 0])).bytes[1] === 255, "premultiplied stop interpolation");
      assert(eq((await d.image.pickColor(layer, [6, 0])).bytes, [0, 0, 0, 0]), "selection leaked");
      await d.masks.fromSelection(layer); const maskBefore = d.getSnapshot().nodes.get(layer).props.mask_transform; await d.image.rotate(layer, 1);
      assert(!eq(d.getSnapshot().nodes.get(layer).props.mask_transform, maskBefore), "linked mask did not move"); await d.undo();
      const hash = await d.snapshotHash(); await reject(d.image.importSvg('<svg width="8" height="8"><rect width="2" height="2"/><script/></svg>'), "UNSUPPORTED"); assert(await d.snapshotHash() === hash, "partial SVG import");
      const svg = await d.image.importSvg('<svg width="8" height="8" viewBox="0 0 8 8"><g opacity="0.5"><circle cx="4" cy="4" r="2" fill="#f00"/></g></svg>'); assert(d.getSnapshot().nodes.has(svg.node), "SVG missing"); await d.undo(); assert(await d.snapshotHash() === hash, "SVG Undo");
      d = await engine.createDocument({ width: 8, height: 8 });
      await d.image.importSvg('<svg width="8" height="8"><g opacity="0.5" fill="#f00"><rect width="6" height="6"/><rect x="1" y="1" width="6" height="6"/></g></svg>');
      assert(eq((await d.image.pickComposite([2, 2])).bytes, [255, 0, 0, 128]), "group opacity was applied per child");
    });
    await test("canvas edits move every layer / selection / Undo / save / brush after rotation", async () => {
      d = await engine.createDocument({ width: 300, height: 100 });
      await d.transact("layer.add", (tx) => { layer = tx.addRaster({ bounds: { x: 0, y: 0, width: 300, height: 100 } }); });
      await d.image.fill(layer, [0, 0], [0, 0, 1, 1]);
      await d.transact("layer.add", (tx) => { tx.addRaster({ bounds: { x: 0, y: 0, width: 4, height: 4 } }); });
      const top = d.getSnapshot().nodes.get(d.getSnapshot().root).children.at(-1);
      await d.image.fill(top, [0, 0], [1, 0, 0, 1]); const before = await d.snapshotHash(), source = d.getSnapshot().nodes.get(layer).tiles;
      await d.selection.apply({ kind: "rectangle", x: 280, y: 0, width: 10, height: 10 });
      const withSelection = await d.snapshotHash();
      assert(eq(await d.canvas.rotate(1), { width: 100, height: 300, no_op: false }), "rotate result");
      assert(d.getSnapshot().meta.width === 100 && d.getSnapshot().meta.height === 300, "canvas size");
      assert(eq((await d.image.pickComposite([98, 1])).bytes, [255, 0, 0, 255]), "corner pixels followed the rotation");
      assert(eq((await d.image.pickComposite([98, 5])).bytes, [0, 0, 255, 255]), "lower layer followed the rotation");
      assert(eq(d.getSnapshot().nodes.get(layer).tiles, source), "canvas rotation rewrote pixels");
      await d.image.fill(layer, [150, 50], [0, 1, 0, 1], { selection: "ignore", tolerance: 255 });
      assert(eq((await d.image.pickComposite([50, 150])).bytes, [0, 255, 0, 255]), "layer edit after rotation");
      await d.selection.clear(); const brush = d.tools.begin("brush", { layer, tip: { size: 8, hardness: 1 }, color: [1, 1, 0, 1] });
      brush.input({ x: 60, y: 200, pressure: 1 }); brush.input({ x: 62, y: 200, pressure: 1 }); await brush.end();
      assert(eq((await d.image.pickComposite([61, 200])).bytes, [255, 255, 0, 255]), "brush lands at the document point after rotation");
      await d.undo(); await d.undo(); await d.undo(); await d.undo();
      assert(await d.snapshotHash() === withSelection, "canvas rotation Undo");
      await d.selection.clear();
      await d.canvas.crop({ x: 2, y: 2, width: 10, height: 10 }); assert(eq((await d.image.pickComposite([1, 1])).bytes, [255, 0, 0, 255]), "crop moved content");
      assert(eq((await d.image.pickComposite([2, 2])).bytes, [0, 0, 255, 255]), "crop origin");
      await reject(d.image.pickComposite([10, 0]), "NOT_FOUND");
      await d.undo();
      await d.canvas.flip("horizontal"); assert(eq((await d.image.pickComposite([299, 0])).bytes, [255, 0, 0, 255]), "flip"); await d.undo();
      await d.canvas.resize(600, 200); assert(eq((await d.image.pickComposite([1, 1])).bytes, [255, 0, 0, 255]), "resize keeps the corner"); assert(eq((await d.image.pickComposite([100, 100])).bytes, [0, 0, 255, 255]), "resize keeps the layer");
      const hash = await d.snapshotHash(), saved = blobTarget(); await d.save(saved); await d.durable(); await testing(engine).crash(); d = engine.document;
      assert(await d.snapshotHash() === hash, "canvas edit recovery"); assert(d.getSnapshot().meta.width === 600, "recovered canvas");
      d = await engine.open(saved.blob); assert(await d.snapshotHash() === hash, "canvas edit reopen");
      await reject(d.canvas.rotate(4), "INVALID_ARGUMENT"); await reject(d.canvas.crop({ x: 0, y: 0, width: 0, height: 1 }), "INVALID_ARGUMENT"); assert(await d.snapshotHash() === hash, "failed canvas edit changed the document");
      void before;
    });
    await test("clone stamp / smudge / histogram / merge down / rasterize through the SDK", async () => {
      d = await engine.createDocument({ width: 600, height: 32 });
      let lower, upper;
      await d.transact("layer.add", (tx) => { lower = tx.addRaster({ bounds: { x: 0, y: 0, width: 600, height: 32 } }); upper = tx.addRaster({ bounds: { x: 0, y: 0, width: 8, height: 8 } }); });
      await d.image.fill(upper, [0, 0], [0, 1, 0, 1]);
      await d.selection.apply({ kind: "rectangle", x: 0, y: 0, width: 4, height: 4 }); await d.image.fill(lower, [0, 0], [1, 0, 0, 1]); await d.selection.clear();
      const start = await d.snapshotHash();
      const stamp = await d.image.cloneStamp(lower, [1, 1], { points: [[300, 10]], radius: 3 }); assert(stamp.changed_tiles === 1, "stamp tile");
      assert(eq((await d.image.pickColor(lower, [300, 10])).bytes, [255, 0, 0, 255]), "stamp pixel"); assert(eq((await d.image.pickComposite([300, 10])).bytes, [255, 0, 0, 255]), "stamp rendered");
      await d.undo(); assert(await d.snapshotHash() === start, "stamp Undo");
      await d.image.smudge(lower, { points: [[2, 2], [20, 2]], radius: 2 }, { strength: 1 }); assert((await d.image.pickColor(lower, [15, 2])).bytes[3] === 255, "smudge carried"); await d.undo();
      await reject(d.image.smudge(lower, { points: [], radius: 2 }), "INVALID_ARGUMENT"); assert(await d.snapshotHash() === start, "failed smudge");
      const histogram = await d.image.histogram(lower); assert(histogram.pixels === 19200 && histogram.r[255] === 16 && histogram.a[0] === 19184, "histogram");
      const composite = await d.image.pickComposite([1, 1]);
      await reject(d.image.mergeDown(lower), "INVALID_ARGUMENT");
      await d.transact("layer.props", (tx) => tx.setProps(upper, { blend: "multiply" })); await reject(d.image.mergeDown(upper), "UNSUPPORTED"); await d.undo();
      const merged = await d.image.mergeDown(upper); assert(merged.node === lower && !d.getSnapshot().nodes.has(upper), "merge result");
      assert(eq((await d.image.pickComposite([1, 1])).bytes, composite.bytes), "merge changed the picture"); assert(eq((await d.image.pickColor(lower, [6, 6])).bytes, [0, 255, 0, 255]), "merged pixels");
      await d.undo(); assert(await d.snapshotHash() === start && d.getSnapshot().nodes.has(upper), "merge Undo");
      await d.transact("layer.props", (tx) => tx.setProps(upper, { transform: [1, 0, 0, 1, 300, 1] }));
      const baked = await d.image.rasterize(upper); assert(eq(baked.bounds, { x: 300, y: 1, width: 8, height: 8 }), "rasterize bounds"); assert(eq((await d.image.pickComposite([301, 2])).bytes, [0, 255, 0, 255]), "rasterize picture");
      const hash = await d.snapshotHash(), saved = blobTarget(); await d.save(saved); d = await engine.open(saved.blob); assert(await d.snapshotHash() === hash, "layer edit reopen");
    });
    await test("Catmull-Rom and area filters / vector layers and groups become pixels with the same picture", async () => {
      d = await engine.createDocument({ width: 64, height: 64 }); await d.transact("layer.add", (tx) => { layer = tx.addRaster({ bounds: { x: 0, y: 0, width: 4, height: 1 } }); });
      await d.image.fill(layer, [0, 0], [1, 1, 1, 1]); await d.image.resize(layer, 2, 1, { filter: "area" }); assert(eq((await d.image.pickColor(layer, [1, 0])).bytes, [255, 255, 255, 255]), "area");
      await d.image.resize(layer, 5, 1, { filter: "catmull_rom" }); assert(eq((await d.image.pickColor(layer, [4, 0])).bytes, [255, 255, 255, 255]), "catmull-rom");
      await reject(d.image.resize(layer, 2, 1, { filter: "lanczos" }), "UNSUPPORTED");
      d = await engine.createDocument({ width: 300, height: 64 });
      const svg = await d.image.importSvg('<svg width="300" height="64"><rect x="250" y="10" width="20" height="20" fill="#f00"/><circle cx="260" cy="40" r="12" fill="#00f" stroke="#0f0" stroke-width="4"/></svg>');
      let group = null, vector = svg.node; while (d.getSnapshot().nodes.get(vector).type !== "vector") { group = vector; vector = d.getSnapshot().nodes.get(vector).children[0]; }
      const samples = [[255, 15], [260, 40], [260, 28], [272, 40], [248, 40], [120, 30]];
      const before = await Promise.all(samples.map((p) => d.image.pickComposite(p))), hash = await d.snapshotHash();
      const baked = await d.image.rasterize(vector); assert(d.getSnapshot().nodes.get(vector).type === "raster" && baked.bounds.width > 0, "kind");
      const after = await Promise.all(samples.map((p) => d.image.pickComposite(p)));
      for (const [i, p] of samples.entries()) assert(before[i].bytes.every((v, c) => Math.abs(v - after[i].bytes[c]) <= 1), `vector picture changed at ${p}: ${before[i].bytes} ${after[i].bytes}`);
      assert(eq((await d.image.pickComposite([255, 15])).bytes, [255, 0, 0, 255]), "opaque interior is exact");
      await d.undo(); assert(await d.snapshotHash() === hash, "vector bake Undo");
      await d.transact("layer.props", (tx) => tx.setProps(vector, { transform: [2, 0, 0, 2, 0, 0] })); const scaled = await d.snapshotHash();
      await reject(d.image.rasterize(vector), "UNSUPPORTED"); assert(await d.snapshotHash() === scaled, "refused bake changed the document"); await d.undo();
      if (group) { const merged = await d.image.rasterize(group); assert(merged.node === group && d.getSnapshot().nodes.get(group).type === "raster" && !d.getSnapshot().nodes.has(vector), "group bake");
        const flat = await Promise.all(samples.map((p) => d.image.pickComposite(p))); for (const [i, p] of samples.entries()) assert(before[i].bytes.every((v, c) => Math.abs(v - flat[i].bytes[c]) <= 1), `group picture changed at ${p}`); }
      const saved = blobTarget(), done = await d.snapshotHash(); await d.save(saved); d = await engine.open(saved.blob); assert(await d.snapshotHash() === done, "bake reopen");
    });
    await test("branches / dirty refusal / conflict resolution / two-parent roundtrip", async () => {
      d = await engine.createDocument({ width: 8, height: 8 }); await d.transact("layer.add", (tx) => { layer = tx.addRaster(); });
      const base = await d.versions.commit("base"); await d.versions.createBranch("案A", { checkout: true });
      await d.transact("layer.props", (tx) => tx.setProps(layer, { name: "theirs" })); await reject(d.versions.checkout("main"), "CONFLICT");
      const theirs = await d.versions.commit("theirs"); await d.versions.checkout("main");
      await d.transact("layer.props", (tx) => tx.setProps(layer, { name: "ours" })); const ours = await d.versions.commit("ours");
      await reject(d.versions.deleteBranch("案A"), "BRANCH_NOT_MERGED");
      const hash = await d.snapshotHash(), plan = await d.versions.prepareMerge("案A"); assert(plan.base === base.hash && plan.conflicts.length === 1, "merge plan");
      await reject(d.versions.finishMerge(plan.token, []), "INVALID_ARGUMENT"); assert(await d.snapshotHash() === hash, "partial merge");
      const merged = await d.versions.finishMerge(plan.token, plan.conflicts.map((c) => ({ conflict_id: c.id, choice: "theirs" })));
      const commits = await d.versions.log(); assert(eq(commits[0].parents, [ours.hash, theirs.hash]), "two parents"); assert((await d.versions.log({ mode: "dag" })).some((c) => c.hash === theirs.hash), "DAG omitted second parent"); assert(d.getSnapshot().nodes.get(layer).name === "theirs", "resolution");
      await d.versions.deleteBranch("案A");
      // Calls issued without awaiting still follow the public mutation order.
      const preparing = d.versions.prepareMerge("main"); const cancelling = d.versions.cancelMerge();
      const cancelledPlan = await preparing; await cancelling;
      await reject(d.versions.finishMerge(cancelledPlan.token, []), "STALE_MERGE");
      const finalPlan = await d.versions.prepareMerge("main");
      const finishing = d.versions.finishMerge(finalPlan.token, []); const afterFinish = d.versions.cancelMerge();
      assert((await finishing).kind === "no_op", "cancel overtook finishMerge"); await afterFinish;
      const saved = blobTarget(); await d.save(saved); d = await engine.open(saved.blob); assert((await d.versions.log())[0].hash === merged.head, "merge reopen");
    });
    await test("folder GC / explicit execution / protected roots / clean subsequent save", async () => {
      await d.versions.createBranch("discard", { checkout: true }); await d.image.fill(layer, [0, 0], [0, 0, 1, 1]); const discard = await d.versions.commit("discard");
      await d.versions.checkout("main"); await d.versions.deleteBranch("discard", { force: true });
      const memory = new MemoryObjectProvider(); await d.saveFolder(memory); const preview = await d.versions.gc(); assert(preview.dry_run && preview.by_kind.commits.removed >= 1, "GC preview");
      assert(await memory.getObject(`commits/${discard.hash}.json`), "dry-run removed a commit");
      await reject(d.versions.gc({ dryRun: false, expectedBaseline: "stale" }), "CONFLICT");
      const result = await d.versions.gc({ dryRun: false, expectedBaseline: preview.baseline }); assert(!result.dry_run && !d.getSnapshot().dirty, "GC did not publish saved reference state");
      assert(!await memory.getObject(`commits/${discard.hash}.json`), "GC did not delete"); await d.saveFolder(memory); assert(!await memory.getObject(`commits/${discard.hash}.json`), "save resurrected collected history");
      const snapshot = await d.snapshotHash(); d = await engine.openFolder(memory); assert(await d.snapshotHash() === snapshot, "GC lost working root");
      const saved = blobTarget(); await d.save(saved); const compact = await d.versions.compactArchive(saved.blob); d = await engine.open(new Blob([compact.bytes])); assert(await d.snapshotHash() === snapshot, "compacted archive");
      const mirrorStore = new MemoryObjectProvider();
      const mirrorFailure = new Proxy(mirrorStore, { get(target, key) {
        if (key === "exclusive") return (run) => target.exclusive((view) => run({ ...view, writeRef: async (name, data, expected) => { if (name === "document.json") throw Error("injected mirror failure"); return view.writeRef(name, data, expected); } }));
        const value = target[key]; return typeof value === "function" ? value.bind(target) : value;
      } });
      const savedWithWarning = await d.saveFolder(mirrorFailure); assert(savedWithWarning.warnings.length === 1 && !d.getSnapshot().dirty, "mirror failure invalidated the saved original");
      d = await engine.openFolder(mirrorFailure); assert(await d.snapshotHash() === snapshot, "mirror failure lost canonical content");
      const disabledStore = new MemoryObjectProvider();
      const disabledGc = new Proxy(disabledStore, { get(target, key) { if (key === "gcCapabilities") return { exclusive: false, atomicManifest: false, deleteObjects: false }; const value = target[key]; return typeof value === "function" ? value.bind(target) : value; } });
      await d.saveFolder(disabledGc); const manifestBefore = await disabledGc.readRef("manifest.json");
      await reject(d.versions.gc({ dryRun: false }), "UNSUPPORTED");
      assert(eq([...manifestBefore], [...await disabledGc.readRef("manifest.json")]), "disabled GC changed storage");
      const brokenStore = new MemoryObjectProvider();
      const brokenGc = new Proxy(brokenStore, { get(target, key) { if (key === "exclusive") return (run) => target.exclusive((view) => run({ ...view, deleteObjects: undefined })); const value = target[key]; return typeof value === "function" ? value.bind(target) : value; } });
      await d.saveFolder(brokenGc); const beforeBrokenGc = await brokenGc.readRef("manifest.json");
      await reject(d.versions.gc({ dryRun: false }), "UNSUPPORTED");
      assert(eq([...beforeBrokenGc], [...await brokenGc.readRef("manifest.json")]), "missing delete method changed storage");
    });
    await test("OPFS folder: writers exclude each other / stale views / lock in another context", async () => {
      const name = "gc-lock-" + crypto.randomUUID(), root = await navigator.storage.getDirectory();
      const handle = await root.getDirectoryHandle(name, { create: true }), a = await opfsFolder(name), b = await OpfsFolderProvider.open(handle, { waitMs: 200 });
      assert(a.gcCapabilities?.exclusive && a.kind === "opfs-dir", "OPFS capability");
      // A picked folder needs a user gesture here, so the browser's answer for one is stubbed.
      const resolve = FileSystemDirectoryHandle.prototype.resolve; FileSystemDirectoryHandle.prototype.resolve = async () => null;
      try { await reject(OpfsFolderProvider.open(handle), "UNSUPPORTED"); } finally { FileSystemDirectoryHandle.prototype.resolve = resolve; }
      const legacy = new FileSystemDirectoryProvider(handle); assert(!legacy.gcCapabilities, "plain provider must not claim GC");
      let release, stale; const gate = new Promise((r) => { release = r; }); let entered;
      const held = new Promise((r) => { entered = r; });
      const holding = a.exclusive(async (view) => { stale = view; entered(); await gate; await view.writeRef("manifest.json", new TextEncoder().encode("first")); });
      await held;
      // Another SDK writer, and another browsing context, cannot get in while it is held.
      await reject(b.writeRef("manifest.json", new TextEncoder().encode("second")), "BUSY");
      let legacyDone = false; const legacyWrite = legacy.writeRef("other.json", new TextEncoder().encode("x")).then(() => { legacyDone = true; });
      await new Promise((r) => setTimeout(r, 150)); assert(!legacyDone, "a plain provider on the same OPFS directory ignored the lock");
      const frame = document.createElement("iframe"); document.body.append(frame);
      const lockName = (await navigator.locks.query()).held.map((l) => l.name).find((n) => n.endsWith(name)); assert(lockName, "lock not held");
      assert(await frame.contentWindow.navigator.locks.request(lockName, { ifAvailable: true }, (lock) => lock === null), "another context got the lock"); frame.remove();
      release(); await holding; await legacyWrite;
      await reject(stale.writeRef("manifest.json", new TextEncoder().encode("late")), "CONFLICT");
      assert(new TextDecoder().decode(await a.readRef("manifest.json")) === "first", "stale view wrote");
      await reject(a.writeRef("manifest.json", new TextEncoder().encode("cas"), "wrong"), "CONFLICT");
      assert(new TextDecoder().decode(await a.readRef("manifest.json")) === "first", "compare-and-replace wrote");
      await a.putObjects([{ path: "commits/x.json", data: new Uint8Array([1]) }]);
      await reject(a.putObjects([{ path: "commits/x.json", data: new Uint8Array([2]) }]), "CORRUPT_DATA");
      await (await (await handle.getDirectoryHandle("commits")).getFileHandle("x.json.crswap", { create: true }));
      assert(eq(await a.list("commits/"), ["commits/x.json"]), "a leftover swap file was listed");
      await root.removeEntry(name, { recursive: true });
    });
    await test("OPFS folder GC: deletes only garbage / interrupted run recovers / Undo / reopen", async () => {
      const name = "gc-real-" + crypto.randomUUID(), folder = await opfsFolder(name), root = await navigator.storage.getDirectory();
      d = await engine.createDocument({ width: 600, height: 8 }); await d.transact("layer.add", (tx) => { layer = tx.addRaster({ bounds: { x: 0, y: 0, width: 600, height: 8 } }); });
      await d.image.fill(layer, [0, 0], [1, 0, 0, 1]); const base = await d.versions.commit("base");
      await d.versions.createBranch("discard", { checkout: true }); await d.image.fill(layer, [0, 0], [0, 0, 1, 1]); const discard = await d.versions.commit("discard");
      await d.versions.createBranch("discard2", { checkout: true }); await d.image.fill(layer, [0, 0], [0, 1, 0, 1]); const discard2 = await d.versions.commit("discard2");
      await d.versions.checkout("main"); await d.versions.deleteBranch("discard", { force: true }); await d.versions.deleteBranch("discard2", { force: true });
      await d.transact("layer.props", (tx) => tx.setProps(layer, { name: "before undo" }));
      // The run stops partway through its deletions, as a closed tab would.
      let deletions = 0, failing = true;
      const interrupted = new Proxy(folder, { get(target, key) {
        if (key === "exclusive") return (run) => target.exclusive((view) => run({ ...view, deleteObjects: async (paths) => { if (failing && ++deletions > 1) throw Error("injected stop"); return view.deleteObjects(paths); } }));
        const value = target[key]; return typeof value === "function" ? value.bind(target) : value;
      } });
      await d.saveFolder(interrupted);
      await reject(d.versions.gc({ dryRun: false, expectedBaseline: "stale" }), "CONFLICT");
      assert(await folder.getObject(`commits/${discard.hash}.json`), "stale baseline deleted");
      const preview = await d.versions.gc(); assert(preview.by_kind.commits.removed === 2, "two unreachable commits " + JSON.stringify(preview.by_kind));
      const again = await d.versions.gc(); const snapshot = await d.snapshotHash();
      await reject(d.versions.gc({ dryRun: false, expectedBaseline: again.baseline }), "STORAGE");
      failing = false;
      const redone = await d.versions.gc(); assert(redone.by_kind.commits.removed + redone.by_kind.snapshots.removed + redone.by_kind.objects.removed > 0, "garbage left to collect");
      const finished = await d.versions.gc({ dryRun: false, expectedBaseline: redone.baseline }); assert(!finished.dry_run && !d.getSnapshot().dirty, "GC finished clean");
      assert(!await folder.getObject(`commits/${discard.hash}.json`) && !await folder.getObject(`commits/${discard2.hash}.json`), "garbage kept"); assert(await folder.getObject(`commits/${base.hash}.json`), "history lost");
      await d.undo(); assert(d.getSnapshot().nodes.get(layer).name !== undefined, "Undo after GC"); await d.redo(); assert(await d.snapshotHash() === snapshot, "Redo after GC");
      await d.saveFolder(interrupted); assert(!await folder.getObject(`commits/${discard.hash}.json`), "save resurrected garbage");
      const hash = await d.snapshotHash(); d = await engine.openFolder(folder); assert(await d.snapshotHash() === hash, "reopen after GC");
      assert((await d.versions.log())[0].hash === base.hash, "head after GC"); assert(eq((await d.image.pickColor(layer, [0, 0])).bytes.slice(3), [255]), "pixels after GC");
      await d.versions.commit("after GC"); await root.removeEntry(name, { recursive: true });
    });
    await test("on-demand history: reads only what is needed / checkout / restore / Undo / saves", async () => {
      d = await engine.createDocument({ width: 512, height: 512 }); await d.transact("layer.add", (tx) => { layer = tx.addRaster({ bounds: { x: 0, y: 0, width: 512, height: 512 } }); });
      const versions = [];
      for (let i = 0; i < 6; i++) { await d.image.fill(layer, [0, 0], [i / 8, 0.5, 1 - i / 8, 1], { tolerance: 255 }); versions.push((await d.versions.commit("v" + i)).hash); }
      const store = new MemoryObjectProvider(); let bytesRead = 0, objectReads = 0, unavailable = false;
      const folder = new Proxy(store, { get(target, key) {
        const count = (view) => ({ ...view, getObject: async (path) => { if (unavailable && path.startsWith("objects/")) return null; const data = await view.getObject(path); if (path.startsWith("objects/") && data) { bytesRead += data.length; objectReads++; } return data; } });
        if (key === "exclusive") return (run) => target.exclusive((view) => run(count(view)));
        const value = target[key]; return typeof value === "function" ? value.bind(target) : value;
      } });
      await d.saveFolder(folder); const workingHash = await d.snapshotHash();
      await engine.openFolder(folder); const allBytes = bytesRead, allReads = objectReads; bytesRead = objectReads = 0;
      d = await engine.openFolder(folder, { history: "on-demand" }); const lazyBytes = bytesRead, lazyReads = objectReads;
      assert(await d.snapshotHash() === workingHash, "working document");
      assert(lazyReads > 0 && lazyReads * 4 <= allReads, `read ${lazyReads} of ${allReads} objects`); log(`on-demand open: ${lazyReads}/${allReads} objects, ${lazyBytes}/${allBytes} bytes`);
      assert(await d.versions.externalObjects() > 0, "history left in the folder");
      bytesRead = 0; assert((await d.versions.log()).length === 6, "log"); await d.versions.diff(versions[0], versions[5]); assert(bytesRead === 0, "log and diff read pixels");
      const color = async () => (await d.image.pickColor(layer, [0, 0])).bytes.join();
      const current = await color();
      // A restore reads that version, one Undo step away.
      await d.versions.restore(versions[0]); assert(bytesRead > 0, "restore read nothing"); const old = await color(); assert(old !== current, "restored pixels");
      await d.undo(); assert(await color() === current, "Undo after release"); await d.redo(); assert(await color() === old, "Redo"); await d.undo();
      // Switching reads the branch's version; the one left behind goes back to the folder.
      await d.versions.createBranch("old", { from: versions[1], checkout: true }); assert(d.versions.status().refs.current_branch === "old", "branch checkout");
      await d.versions.checkout("main"); assert(await color() === current, "checkout back");
      // Merging reads both sides.
      await d.versions.createBranch("side", { checkout: true }); await d.transact("layer.props", (tx) => tx.setProps(layer, { name: "side" })); await d.versions.commit("side");
      await d.versions.checkout("main"); await d.transact("layer.props", (tx) => tx.setProps(layer, { opacity: 0.5 })); await d.versions.commit("main");
      const plan = await d.versions.prepareMerge("side"); const merged = await d.versions.finishMerge(plan.token, []); assert(merged.kind === "committed", "merge");
      // Saving to the same folder reads no history and leaves it there.
      bytesRead = 0; await d.saveFolder(folder); assert(bytesRead === 0, "same-folder save read objects"); assert(await d.versions.externalObjects() > 0, "still external");
      // A missing object is reported and changes nothing, and a normal save refuses to drop history.
      unavailable = true; const before = await d.snapshotHash(); const status = d.versions.status().head;
      await reject(d.versions.detach(versions[2]), "STORAGE"); const saveTarget = blobTarget(); await reject(d.save(saveTarget), "STORAGE");
      assert(await d.snapshotHash() === before && d.versions.status().head === status, "failed read changed the document");
      const working = blobTarget(); await d.save(working, { history: "omit" });  unavailable = false;
      // A folder can be attached only if it holds this document's history.
      await reject(d.versions.attachFolder(new MemoryObjectProvider()), "CONFLICT"); await d.versions.attachFolder(folder); assert(d.folder === folder, "attached");
      // A full save reads everything, and that archive stands alone.
      const full = blobTarget(); await d.save(full); const hash = await d.snapshotHash(); const count = (await d.versions.log({ mode: "dag" })).length;
      // Worker restart: the same handle reads on.
      await d.durable(); await testing(engine).crash(); d = engine.document; assert(await d.snapshotHash() === hash, "recovered working document");
      await d.versions.detach(versions[3]); assert((await color()) !== current, "read after a Worker restart"); await d.versions.checkout("main");
      // Without its folder: a reload recovers the document, and attachFolder brings history back.
      d = await engine.open(full.blob); assert(await d.snapshotHash() === hash && (await d.versions.log({ mode: "dag" })).length === count, "full archive");
      d = await engine.open(working.blob); assert((await d.versions.log()).length === 0, "working-only archive has no history");
    });
    await test("OPFS on-demand: other documents cannot collect what an open document needs", async () => {
      const name = "gc-hold-" + crypto.randomUUID(), root = await navigator.storage.getDirectory(), folder = await opfsFolder(name);
      d = await engine.createDocument({ width: 256, height: 256 }); await d.transact("layer.add", (tx) => { layer = tx.addRaster({ bounds: { x: 0, y: 0, width: 256, height: 256 } }); });
      await d.image.fill(layer, [0, 0], [1, 0, 0, 1]); const first = await d.versions.commit("first");
      await d.versions.createBranch("drop", { checkout: true }); await d.image.fill(layer, [0, 0], [0, 1, 0, 1], { tolerance: 255 }); const dropped = await d.versions.commit("drop");
      await d.versions.checkout("main"); await d.versions.deleteBranch("drop", { force: true });
      await d.image.fill(layer, [0, 0], [0, 0, 1, 1], { tolerance: 255 }); await d.versions.commit("second"); await d.saveFolder(folder);
      d = await engine.openFolder(folder, { history: "on-demand" }); assert(await d.versions.externalObjects() > 0, "external");
      // A second session on the folder is refused while another holds it.
      const other = await folder.holdObjects(); const preview = await d.versions.gc();
      await reject(d.versions.gc({ dryRun: false, expectedBaseline: preview.baseline }), "BUSY"); assert(await folder.getObject(`commits/${dropped.hash}.json`), "GC ran under a hold"); await other();
      // Alone, its own hold steps aside: garbage goes, and its history stays readable.
      const done = await d.versions.gc({ dryRun: false, expectedBaseline: preview.baseline }); assert(!done.dry_run, "GC ran");
      assert(!await folder.getObject(`commits/${dropped.hash}.json`), "garbage kept");
      await d.versions.detach(first.hash); assert(eq((await d.image.pickColor(layer, [0, 0])).bytes, [255, 0, 0, 255]), "history after GC");
      await d.versions.checkout("main"); await root.removeEntry(name, { recursive: true });
    });
    await test("history and images survive Worker restart and GPU device loss", async () => {
      await d.versions.createBranch("recovery", { checkout: true }); await d.image.fill(layer, [0, 0], [1, 0, 1, 1]); const commit = await d.versions.commit("recover");
      await d.durable(); await testing(engine).crash(); d = engine.document;
      assert(d.versions.status().refs.current_branch === "recovery" && d.versions.status().head === commit.hash, "history recovery");
      assert(eq((await d.image.pickColor(layer, [0, 0])).bytes, [255, 0, 255, 255]), "CPU pixels lost");
      const restored = new Promise((resolve, reject) => { const timeout = setTimeout(() => { off(); reject(Error("GPU recovery timeout")); }, 30000); const off = unstable(engine).onGpu((state) => { if (state === "restored") { clearTimeout(timeout); off(); resolve(); } }); });
      await testing(engine).destroyGpu(); await restored;
      assert(d.versions.status().head === commit.hash, "GPU recovery lost head"); assert(eq((await d.image.pickColor(layer, [0, 0])).bytes, [255, 0, 255, 255]), "GPU recovery lost pixels");
    });
    await test("Playground image and branch controls apply through the visible UI", async () => {
      d = await engine.createDocument({ width: 8, height: 8 });
      await d.transact("layer.add", (tx) => { layer = tx.addRaster({ bounds: { x: 0, y: 0, width: 3, height: 2 } }); });
      const waitFor = async (predicate) => { const end = performance.now() + 10000; while (!predicate()) { if (performance.now() > end) throw Error("UI update timeout: " + document.querySelector("#error").textContent); await new Promise((r) => setTimeout(r, 20)); } };
      await waitFor(() => !document.querySelector("#image-rotate").disabled);
      document.querySelector("#image-rotate").click();
      await waitFor(() => d.getSnapshot().nodes.get(layer).bounds.width === 2);
      assert(d.getSnapshot().nodes.get(layer).bounds.height === 3, "UI rotation");
      document.querySelector("#canvas-rotate").click();
      await waitFor(() => d.getSnapshot().meta.width === 8 && d.getSnapshot().history.undo);
      document.querySelector("#canvas-width").value = 16; document.querySelector("#canvas-height").value = 16; document.querySelector("#canvas-resize").click();
      await waitFor(() => d.getSnapshot().meta.width === 16);
      await d.undo(); await d.undo();
      await d.versions.commit("UI base");
      document.querySelector("#branch-name").value = "画面の別案";
      document.querySelector("#branch-create").click();
      await waitFor(() => d.versions.status().refs.current_branch === "画面の別案");
      await waitFor(() => document.querySelector("#branch-current").textContent.includes("画面の別案"));
      document.querySelector("#branch-target").value = "main";
      document.querySelector("#branch-checkout").click();
      await waitFor(() => d.versions.status().refs.current_branch === "main");
      assert(!d.getSnapshot().history.undo && !d.getSnapshot().history.redo, "UI checkout history");
    });
  } catch (e) { results.push({ name: String(e.stack || e), pass: false }); log("FAIL " + e); throw e; }
  finally { document.querySelector("#test-results").dataset.report = JSON.stringify({ date: new Date().toISOString(), milestone: "ImageVersions", results }); }
  log("IMAGE VERSIONS COMPLETE"); return results;
}
