import { MemoryObjectProvider } from "../../packages/alto-sdk/dist/index.js";
import { blobTarget } from "../../packages/alto-sdk/dist/browser.js";
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
