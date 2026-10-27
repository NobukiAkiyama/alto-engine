// Embedding: engines a host creates in its own containers, with their own storage
// namespaces, through the public API. Each test starts and disposes its own engines.
import { createEngine, deleteRecovery, isSupported, listRecoveryNamespaces, MemoryObjectProvider } from "../../packages/alto-sdk/dist/index.js";
import { blobTarget } from "../../packages/alto-sdk/dist/browser.js";
import { unstable } from "../../packages/alto-sdk/dist/unstable.js";
import { testing } from "../../packages/alto-sdk/dist/testing.js";
const assert = (v, m) => { if (!v) throw Error(m); };
const near = (a, b, t = 1e-6) => Math.abs(a - b) <= t;
/** Runs `action` and waits for an engine event that `match` accepts. */
async function event(engine, name, action, match) {
  let off;
  const seen = new Promise((resolve, reject) => {
    const t = setTimeout(() => (off(), reject(Error("timeout " + name))), 15000);
    off = engine.on(name, (v) => {
      if (!match(v)) return;
      clearTimeout(t);
      off();
      resolve();
    });
  });
  await action();
  await seen;
}
const run = Date.now().toString(36);
let serial = 0;
const name = (label) => `test-embed/${run}/${label}-${serial++}`;

function container(width = 320, height = 240) {
  const el = document.createElement("div");
  el.style.cssText = `position:fixed;left:0;top:0;width:${width}px;height:${height}px;z-index:-1`;
  document.body.append(el);
  return el;
}
const started = [];
async function engineIn(el, options = {}) {
  const engine = await createEngine({ container: el, gpu: options.gpu, storage: { namespace: name("engine"), ...options.storage } });
  started.push({ engine, el });
  return engine;
}
async function cleanUp() {
  for (const { engine, el } of started.splice(0)) {
    await engine.dispose();
    el.remove();
  }
  for (const { namespace } of await listRecoveryNamespaces())
    if (namespace.startsWith(`test-embed/${run}/`)) await deleteRecovery(namespace).catch(() => {});
}
/** The next frame whose surface matches `test`. */
function frameWhere(engine, test, timeout = 5000) {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => {
      off();
      reject(Error("no matching frame"));
    }, timeout);
    const off = unstable(engine).onFrame((f) => {
      if (!test(f)) return;
      clearTimeout(t);
      off();
      resolve(f);
    });
  });
}
async function rejectsWith(promise, code, reason) {
  try {
    await promise;
  } catch (e) {
    return e.code === code && (reason === undefined || e.details?.reason === reason);
  }
  return false;
}
async function quadrants(engine, doc) {
  const c = new OffscreenCanvas(512, 256), ctx = c.getContext("2d");
  for (const [x, y, color] of [[0, 0, "#ff0000"], [256, 0, "#00ff00"], [0, 128, "#0000ff"], [256, 128, "rgba(255,255,0,0.5)"]]) {
    ctx.fillStyle = color;
    ctx.fillRect(x, y, 256, 128);
  }
  const image = await engine.assets.importImage(await c.convertToBlob());
  await doc.transact("image.import", (tx) => tx.addRasterFromAsset(image));
}

export async function runEmbedTests(_playground, log) {
  const results = [];
  const test = async (label, fn) => {
    const s = performance.now();
    try {
      await fn();
    } finally {
      await cleanUp();
    }
    results.push({ name: label, pass: true, ms: performance.now() - s });
    log("PASS " + label);
  };
  try {
    await test("64/128/256 MiB budgets reserve at most half and preserve brush pixels", async () => {
      for (const [budget, fixed] of [[64,32],[128,64],[256,128]]) {
        const engine=await engineIn(container(128,128),{gpu:{memoryBudgetMB:budget}});
        const doc=await engine.createDocument({width:64,height:64});
        let layer; await doc.transact("layer",tx=>{layer=tx.addRaster();});
        const stroke=doc.tools.begin("brush",{layer,color:[1,0,0,1],tip:{size:16,hardness:1}});
        stroke.input({x:32,y:32,pressure:1}); await stroke.end();
        const tile=await unstable(engine).readTile(0,0), i=(32*256+32)*4;
        assert(Array.from(tile.slice(i,i+4)).join()==="255,0,0,255",`brush at ${budget}`);
        const frame=await unstable(engine).settle();
        assert(frame.gpu.fixed-frame.gpu.retiredFixed===fixed*1048576,`reservation at ${budget}: ${frame.gpu.fixed}`);
        assert(frame.gpuBytes<=budget*1048576,"GPU budget exceeded");
        await cleanUp();
      }
      const el=container();
      let rejected=false;
      try { const bad=await createEngine({container:el,gpu:{memoryBudgetMB:63},storage:{namespace:name("too-small")}}); await bad.dispose(); }
      catch(e){ rejected=String(e).includes("64 MiB"); }
      el.remove(); assert(rejected,"budget under 64 MiB was accepted");
    });
    await test("bounded readback crosses 16/32 tile boundaries, preserves outside keys and one Undo", async () => {
      for(const budget of [64,512]) {
        const engine=await engineIn(container(128,128),{gpu:{memoryBudgetMB:budget},storage:{recovery:false}});
        for(const count of budget===64 ? [1,16,17] : [31,32,33]) {
          const doc=await engine.createDocument({width:256,height:256});
          let layer;await doc.transact("layer",tx=>{layer=tx.addRaster();});
          const before=await doc.snapshotHash(),history=doc.getSnapshot().history.undo;
          const s=doc.tools.begin("brush",{layer,tip:{size:8,hardness:1,spacing:0.2},color:[1,0,0,1]});
          for(let i=0;i<count;i++) s.input({x:128+i*256,y:128,pressure:1});
          await s.end();
          const tiles=(await doc.nodeTiles(layer)).tiles;
          assert(Object.keys(tiles).length===count,`lost canonical keys at ${count}: ${Object.keys(tiles)}`);
          assert(doc.getSnapshot().history.undo===history+1,"not one transaction");
          const f=await unstable(engine).settle(),capacity=budget===64?16:32;
          assert(f.readback.capacity===capacity*262144,"pool capacity");
          assert(f.readback.batches===Math.ceil(count/capacity),"batch count");
          assert(f.readback.completed===count && f.readback.mapped===0,"readback ownership not released");
          assert(f.gpu.readback===capacity*262144 && f.gpu.budgetUsed<=budget*1048576 && f.gpu.operationPeak<=budget*1048576,"quota exceeded");
          await doc.undo();assert(await doc.snapshotHash()===before,"Undo mismatch");
          await doc.redo();assert(JSON.stringify((await doc.nodeTiles(layer)).tiles)===JSON.stringify(tiles),"Redo hashes");
        }
        await cleanUp();
      }
    });
    await test("first/intermediate/final readback failure never commits partial tiles or history", async () => {
      const engine=await engineIn(container(),{storage:{recovery:false}});
      for(const after of [0,32,64]) {
        const doc=await engine.createDocument({width:256,height:256});let layer;
        await doc.transact("layer",tx=>{layer=tx.addRaster();});
        const before=await doc.snapshotHash(),snap=doc.getSnapshot();
        const s=doc.tools.begin("brush",{layer,tip:{size:8,hardness:1},color:[1,0,0,1]});
        for(let i=0;i<65;i++) s.input({x:128+i*256,y:128});
        await testing(engine).request("injectReadbackFailure",{after});
        assert(await rejectsWith(s.end(),"INTERNAL"),"failure missing");
        assert(await doc.snapshotHash()===before,"partial CPU commit");
        const now=doc.getSnapshot();assert(now.revision===snap.revision && JSON.stringify(now.history)===JSON.stringify(snap.history),"history changed");
        const f=await unstable(engine).settle();assert(f.gpu.stroke===0 && f.readback.mapped===0,"failed session leaked");
        const retry=doc.tools.begin("brush",{layer});retry.input({x:32,y:32});await retry.end();
        assert(doc.getSnapshot().revision===snap.revision+1,"tool did not recover");
      }
    });
    await test("cancel during asynchronous finish discards the whole staged stroke", async () => {
      const engine=await engineIn(container(),{storage:{recovery:false}});
      const doc=await engine.createDocument({width:256,height:256});let layer;
      await doc.transact("layer",tx=>{layer=tx.addRaster();});const before=await doc.snapshotHash();
      const s=doc.tools.begin("brush",{layer,tip:{size:8,hardness:1}});
      for(let i=0;i<65;i++) s.input({x:128+i*256,y:128});
      let cancel;
      const off=unstable(engine).onDiagnostic(d=>{if(d.finishStage==="readback") {off();cancel=s.cancel();}});
      const failed=await rejectsWith(s.end(),"CONFLICT");await cancel;
      assert(failed,"cancel did not interrupt readback");assert(await doc.snapshotHash()===before,"cancel committed");
      const f=await unstable(engine).settle();assert(f.gpu.stroke===0 && f.readback.mapped===0,"cancel leaked");
    });
    await test("post-commit journal failure publishes the complete CPU edit and freezes storage", async () => {
      const engine=await engineIn(container());const doc=await engine.createDocument({width:256,height:256});let layer;
      await doc.transact("layer",tx=>{layer=tx.addRaster();});await doc.durable();
      const before=doc.getSnapshot(),s=doc.tools.begin("brush",{layer});s.input({x:32,y:32});
      await testing(engine).request("injectFinishJournalFailure");await s.end();
      assert(engine.status.state==="storage-failed","storage did not freeze");
      assert(doc.getSnapshot().revision===before.revision+1 && doc.getSnapshot().history.undo===before.history.undo+1,"mirror lost committed edit");
      const t=await unstable(engine).readTile(0,0);assert(t[(32*256+32)*4+3]>0,"CPU committed pixel missing");
      assert((await engine.recovery.exportEmergency()).size>0,"emergency export failed");
    });
    await test("presentation readiness, viewport holding and stroke coordinate transform stay consistent", async () => {
      const engine=await engineIn(container());const doc=await engine.createDocument({width:512,height:256});let layer;
      await doc.transact("layer",tx=>{layer=tx.addRaster();});
      await unstable(engine).resetRenderCache(true); // optional prefetch disabled
      await unstable(engine).settle();
      assert(engine.presentation.interactionReady && engine.presentation.phase==="ready","required work blocked by idle prefetch");
      assert(Object.isFrozen(engine.presentation) && Object.isFrozen(engine.presentation.presentedViewport.center),"mutable snapshot");
      const old=engine.presentation,view=engine.viewport.get();
      const s=doc.tools.begin("brush",{layer,tip:{size:8,hardness:1}});
      const [x,y]=engine.viewport.documentToScreen(128,128);
      s.input(new PointerEvent("pointermove",{clientX:x,clientY:y,pointerType:"mouse"}));
      engine.viewport.set({center:[400,128],zoom:view.zoom*1.2,rotation:0.2});
      assert(!engine.presentation.interactionReady && engine.presentation.phase==="holding","not holding requested view");
      assert(engine.viewport.get().center[0]===400,"synchronous viewport contract changed");
      s.input(new PointerEvent("pointermove",{clientX:x,clientY:y,pointerType:"mouse"}));
      const held=await unstable(engine).settle();assert(held.viewVersion<engine.presentation.requestedViewVersion,"viewport was sent during stroke");
      await s.end();await unstable(engine).settle();
      assert(engine.presentation.interactionReady && engine.presentation.presentedViewVersion===engine.presentation.requestedViewVersion,"view did not catch up");
      const tile=await unstable(engine).readTile(0,0);assert(tile[(128*256+128)*4+3]>0,"stroke transform moved");
      engine.viewport.panBy(100,0);const blocked=doc.tools.begin("brush",{layer});let rejected=false;
      try{blocked.input(new PointerEvent("pointerdown",{clientX:x,clientY:y}));}catch(e){rejected=e.code==="CONFLICT";}
      await blocked.cancel();assert(rejected,"accepted pointer on unseen view");
      await unstable(engine).settle();
      const interrupted=doc.tools.begin("brush",{layer});
      interrupted.input({x:32,y:32});await interrupted.flush();
      engine.viewport.panBy(20,0);
      const restored=new Promise((resolve,reject)=>{
        const timer=setTimeout(()=>{off();reject(Error("GPU recovery timed out"));},15000);
        const off=unstable(engine).onGpu(e=>{if(e==="restored"){clearTimeout(timer);off();resolve();}});
      });
      await testing(engine).destroyGpu();await restored;await unstable(engine).settle();
      assert(engine.presentation.interactionReady && engine.presentation.presentedViewVersion===engine.presentation.requestedViewVersion,"GPU restore stranded deferred viewport");
      await engine.createDocument({width:256,height:256});assert(engine.presentation.phase==="preparing" && engine.presentation.presentedViewport===null,"cross-document fallback");
      await unstable(engine).settle();assert(engine.presentation.documentSession!==old.documentSession,"document session did not change");
    });
    await test("low-budget display admits a complete coarser level without changing exact readback", async () => {
      const engine=await engineIn(container(1024,1024),{gpu:{memoryBudgetMB:64},storage:{recovery:false}});
      await engine.createDocument({width:8192,height:8192});
      engine.viewport.set({center:[4096,4096],zoom:1/unstable(engine).pixelRatio(),rotation:0.3});
      const frames=[];const off=unstable(engine).onFrame(f=>frames.push(f));
      const f=await unstable(engine).settle();off();
      assert(f.display.admittedLevel>f.display.requestedLevel && engine.presentation.degradedByBudget,"LOD admission missing");
      assert(engine.presentation.interactionReady && f.pending===0,"coverage never ready");
      assert(f.gpu.displayLayers===42 && f.gpu.budgetUsed<=64*1048576,"display allocation grew");
      for(const frame of frames) if(frame.pending && !frame.gpu.overviewReady) assert(!frame.display.hasPresented,"partial first present");
      const exact=await unstable(engine).readTileLevel(0,0,0);assert(exact.length===262144 && exact.every(x=>x===0),"exact readback degraded");
    });
    await test("the browser is supported", async () => {
      const support = await isSupported();
      assert(support.ok, "isSupported: " + JSON.stringify(support));
    });
    await test("viewport: screen and document points map both ways; zoomAt and fit", async () => {
      const el = container(400, 300);
      const engine = await engineIn(el);
      await engine.createDocument({ width: 800, height: 600 });
      const v = engine.viewport;
      v.set({ center: [400, 300], zoom: 2.5, rotation: 0.3 });
      const r = engine.element.getBoundingClientRect();
      const p = [r.left + 37, r.top + 211];
      const back = v.documentToScreen(...v.screenToDocument(...p));
      assert(near(back[0], p[0]) && near(back[1], p[1]), "round trip " + back);
      const under = v.screenToDocument(...p);
      v.zoomAt(p[0], p[1], 1.7);
      const still = v.screenToDocument(...p);
      assert(near(still[0], under[0], 1e-6) && near(still[1], under[1], 1e-6), "zoomAt moved the point " + still);
      assert(near(v.get().zoom, 2.5 * 1.7), "zoomAt zoom " + v.get().zoom);
      v.fit();
      for (const [x, y] of [[0, 0], [800, 0], [0, 600], [800, 600]]) {
        const s = v.documentToScreen(x, y);
        assert(s[0] >= r.left - 1e-6 && s[0] <= r.right + 1e-6 && s[1] >= r.top - 1e-6 && s[1] <= r.bottom + 1e-6,
          `corner ${x},${y} outside after fit: ${s}`);
      }
    });
    await test("a Worker restart keeps the container, document handle and view", async () => {
      const el = container();
      const engine = await engineIn(el);
      const doc = await engine.createDocument({ width: 300, height: 200 });
      let layer;
      await doc.transact("layer.add", (tx) => (layer = tx.addRaster({ name: "paint" })));
      engine.viewport.set({ center: [120, 80], zoom: 1.75, rotation: 0.2 });
      const view = engine.viewport.get();
      // Edits resolve when applied; a crash keeps only what reached the journal.
      await doc.durable();
      await testing(engine).crash();
      assert(engine.document === doc, "the document handle changed");
      assert(el.firstElementChild === engine.element && el.childElementCount === 1, "the container's content was not one canvas");
      assert(el.isConnected, "the container was removed");
      assert(JSON.stringify(engine.viewport.get()) === JSON.stringify(view), "the view changed: " + JSON.stringify(engine.viewport.get()));
      await doc.transact("layer.props", (tx) => tx.setProps(layer, { name: "after" }));
      assert(doc.getSnapshot().nodes.get(layer).name === "after", "editing failed after the restart");
    });
    await test("a stroke cut by a Worker restart ends with WORKER_CRASHED; editing continues", async () => {
      const engine = await engineIn(container());
      const doc = await engine.createDocument({ width: 256, height: 256 });
      let layer;
      await doc.transact("layer.add", (tx) => (layer = tx.addRaster()));
      await doc.durable();
      const stroke = doc.tools.begin("brush", { layer, color: [1, 0, 0, 1] });
      stroke.input({ x: 20, y: 20 });
      stroke.input({ x: 120, y: 60 });
      await testing(engine).crash();
      assert(await rejectsWith(stroke.end(), "WORKER_CRASHED"), "end() did not report the restart");
      assert(!doc.activeTool, "the stroke stayed active");
      await doc.transact("layer.props", (tx) => tx.setProps(layer, { name: "next" }));
      assert(doc.getSnapshot().nodes.get(layer).name === "next", "an edit after the cut stroke failed");
    });
    await test("the canvas follows the container's size, not the window's", async () => {
      const el = container(300, 200);
      const engine = await engineIn(el);
      await engine.createDocument({ width: 256, height: 256 });
      const ratio = unstable(engine).pixelRatio();
      await frameWhere(engine, (f) => f.surface[0] === Math.round(300 * ratio));
      el.style.width = "420px";
      el.style.height = "260px";
      const f = await frameWhere(engine, (f) => f.surface[0] === Math.round(420 * ratio));
      assert(f.surface[1] === Math.round(260 * ratio), "surface height " + f.surface);
    });
    await test("a container whose size flickers does not resize the canvas every frame", async () => {
      // A Worker-owned canvas resized while the GPU is busy can lose the device in Chrome;
      // a layout that flips a few pixels each frame must not reach the GPU each frame.
      const el = container(300, 200);
      const engine = await engineIn(el);
      await engine.createDocument({ width: 256, height: 256 });
      const ratio = unstable(engine).pixelRatio();
      await frameWhere(engine, (f) => f.surface[1] === Math.round(200 * ratio));
      let last = null,
        changes = 0;
      const off = unstable(engine).onFrame((f) => {
        const size = f.surface.join("x");
        if (last !== null && size !== last) changes++;
        last = size;
      });
      for (let i = 0; i < 60; i++) {
        el.style.height = i % 2 ? "200px" : "203px";
        await new Promise(requestAnimationFrame);
      }
      el.style.height = "260px";
      const settled = await frameWhere(engine, (f) => f.surface[1] === Math.round(260 * ratio));
      off();
      void settled;
      // Without the debounce this is 59. A slow runner can stall one frame past the
      // 100 ms debounce, which lets a size through mid-flicker; that is allowed.
      assert(changes <= 10, `the canvas changed size ${changes} times while its container flickered`);
    });
    await test("a frame held back for the GPU is presented once the GPU catches up", async () => {
      // The renderer presents at most one frame ahead of the GPU. Whatever it holds back
      // must still reach the screen: the last frame shows the last view, not a stale one.
      const el = container(300, 200);
      const engine = await engineIn(el);
      const doc = await engine.createDocument({ width: 512, height: 256 });
      await quadrants(engine, doc);
      let held = 0, newest = -1, last = null, lastPresented = null;
      const off = unstable(engine).onFrame((f) => {
        if (f.presentDeferred) held++;
        newest = Math.max(newest, f.viewVersion ?? -1);
        last = f;
        if (!f.presentDeferred) lastPresented = f;
      });
      for (let i = 0; i < 40; i++) engine.viewport.panBy(i % 2 ? 7 : -5, 1);
      await frameWhere(engine, (f) => f.viewVersion === newest && f.pending === 0 && !f.presentDeferred);
      // A later tick of the same view can be deferred again while that present is
      // still on the GPU. The screen is already the newest view; only an older
      // presented view, or a newest view that never presents, is a miss.
      await new Promise((resolve) => setTimeout(resolve, 300));
      off();
      log(`  ${held} frames held back for the GPU`);
      assert(last.viewVersion === newest, `the last frame is view ${last.viewVersion} of ${newest}`);
      assert(lastPresented && lastPresented.viewVersion === newest,
        `the newest view stayed held back; last presented view ${lastPresented?.viewVersion ?? "none"} of ${newest}`);
    });
    await test("an engine starts in a hidden container and draws once it is shown", async () => {
      const el = container(280, 180);
      el.style.display = "none";
      const engine = await engineIn(el);
      const doc = await engine.createDocument({ width: 256, height: 256 });
      await quadrants(engine, doc);
      await frameWhere(engine, (f) => f.surface[0] === 1 && f.surface[1] === 1);
      el.style.display = "block";
      const ratio = unstable(engine).pixelRatio();
      await frameWhere(engine, (f) => f.surface[0] === Math.round(280 * ratio));
      const tile = await unstable(engine).readTile(0, 0);
      assert(tile[3] === 255 && tile[0] > 250, "the shown engine did not draw: " + Array.from(tile.slice(0, 4)));
    });
    await test("dispose during edits rejects the waiting calls; confirmed edits recover", async () => {
      const el = container();
      const namespace = name("dispose");
      let engine = await engineIn(el, { storage: { namespace } });
      let doc = await engine.createDocument({ width: 256, height: 256 });
      let layer;
      await doc.transact("layer.add", (tx) => (layer = tx.addRaster({ name: "kept" })));
      await doc.durable();
      const confirmed = doc.getSnapshot().revision;
      const edits = [];
      for (let i = 0; i < 40; i++)
        edits.push(doc.transact("layer.props", (tx) => tx.setProps(layer, { name: "late " + i })));
      const disposed = engine.dispose();
      const outcomes = await Promise.allSettled(edits);
      await disposed;
      assert(outcomes.every((o) => o.status === "fulfilled" || o.reason?.code === "WORKER_UNAVAILABLE"),
        "an edit failed with another code: " + JSON.stringify(outcomes.find((o) => o.status === "rejected")?.reason?.code));
      assert(engine.status.state === "disposed" && !engine.element.isConnected, "dispose left the engine running");
      engine = await engineIn(el, { storage: { namespace } });
      assert(engine.recovery.available, "no journal to recover");
      doc = await engine.recovery.restore();
      assert(doc.getSnapshot().revision >= confirmed, "a confirmed edit was lost");
      assert(doc.getSnapshot().nodes.has(layer), "the confirmed layer was lost");
    });
    await test("two engines with different namespaces run side by side; one crashing leaves the other", async () => {
      const a = await engineIn(container(200, 150));
      const b = await engineIn(container(200, 150));
      const da = await a.createDocument({ width: 256, height: 256 });
      const db = await b.createDocument({ width: 256, height: 256 });
      await quadrants(b, db);
      await quadrants(a, da);
      await da.durable();
      const before = await unstable(b).readTile(0, 0);
      await testing(a).crash();
      let layer;
      await db.transact("layer.add", (tx) => (layer = tx.addRaster({ name: "b" })));
      const after = await unstable(b).readTile(0, 0);
      assert(before.every((v, i) => v === after[i]), "B's pixels changed when A restarted");
      assert(db.getSnapshot().nodes.has(layer) && a.document === da, "an engine lost its document");
    });
    await test("a namespace in use is refused; invalid namespaces are refused", async () => {
      const namespace = name("busy");
      await engineIn(container(), { storage: { namespace } });
      // A refused engine never draws, so its container need not be in the page.
      assert(await rejectsWith(createEngine({ container: document.createElement("div"), storage: { namespace } }), "CONFLICT", "namespace-in-use"),
        "a second engine took the namespace");
      for (const bad of ["", "x".repeat(129), "a b", "..", "a/../b", "日本"])
        assert(await rejectsWith(createEngine({ container: document.createElement("div"), storage: { namespace: bad } }), "INVALID_ARGUMENT", "namespace"),
          "accepted namespace " + JSON.stringify(bad));
    });
    await test("a waiting journal blocks new documents until restored or discarded", async () => {
      const el = container();
      const namespace = name("pending");
      let engine = await engineIn(el, { storage: { namespace } });
      const doc = await engine.createDocument({ width: 128, height: 128 });
      await doc.transact("layer.add", (tx) => tx.addRaster());
      await doc.durable();
      await engine.dispose();
      engine = await engineIn(el, { storage: { namespace } });
      assert(engine.recovery.available, "no waiting journal");
      assert(await rejectsWith(engine.createDocument({ width: 64, height: 64 }), "CONFLICT", "recovery-pending"),
        "createDocument overwrote the waiting journal");
      await engine.recovery.discard();
      assert(!engine.recovery.available, "discard left the journal waiting");
      const fresh = await engine.createDocument({ width: 64, height: 64 });
      await fresh.transact("layer.add", (tx) => tx.addRaster());
      await fresh.durable();
      await engine.dispose();
      engine = await engineIn(el, { storage: { namespace } });
      assert(engine.recovery.available, "the new document was not journaled after discard");
      const restored = await engine.recovery.restore();
      assert(restored.getSnapshot().meta.width === 64, "restored the discarded document");
    });
    await test("an edit queued on the old handle cannot select the replacement document", async () => {
      const engine = await engineIn(container());
      const old = await engine.createDocument({ width: 64, height: 64 });
      let replacement, selected, triggered = false;
      old.subscribe(() => {
        if (triggered) return;
        triggered = true;
        replacement = engine.createDocument({ width: 128, height: 128 });
        selected = rejectsWith(old.selection.all(), "CONFLICT", "document-closed");
      });
      await old.transact("layer.add", (tx) => tx.addRaster({ name: "old" }));
      const next = await replacement;
      assert(await selected, "the queued selection on the old handle succeeded");
      assert(next.getSnapshot().selection === null, "the old handle selected the new document");
      assert(old.getSnapshot().meta.width === 64, "the old handle adopted the replacement's state");
      await next.transact("layer.add", (tx) => tx.addRaster({ name: "new" }));
    });
    await test("discard refuses a live document and preserves its confirmed recovery journal", async () => {
      const engine = await engineIn(container());
      const doc = await engine.createDocument({ width: 64, height: 64 });
      await doc.transact("layer.add", (tx) => tx.addRaster({ name: "kept" }));
      await doc.durable();
      const hash = await doc.snapshotHash();
      const before = await testing(engine).recoveryStore();
      assert(await rejectsWith(engine.recovery.discard(), "CONFLICT", "no-recovery"),
        "discard deleted a live document's recovery journal");
      // The Worker must enforce the condition too, even without the SDK's guard.
      assert(await rejectsWith(testing(engine).request("discardRecovery"), "CONFLICT", "no-recovery"),
        "the Worker discarded the live journal");
      const after = await testing(engine).recoveryStore();
      assert(JSON.stringify(after) === JSON.stringify(before), "discard changed the recovery store");
      await testing(engine).crash();
      assert((await doc.snapshotHash()) === hash, "the confirmed document did not recover");
    });
    await test("archive and folder opens reject edits queued on the replaced handle", async () => {
      for (const format of ["archive", "folder"]) {
        const engine = await engineIn(container());
        const old = await engine.createDocument({ width: 64, height: 64 });
        await old.transact("layer.add", (tx) => tx.addRaster({ name: "saved" }));
        const target = blobTarget(), folder = new MemoryObjectProvider();
        if (format === "archive") await old.save(target);
        else await old.saveFolder(folder);
        let replacement, selected, triggered = false;
        old.subscribe(() => {
          if (triggered) return;
          triggered = true;
          replacement = format === "archive" ? engine.open(target.blob) : engine.openFolder(folder);
          selected = rejectsWith(old.selection.all(), "CONFLICT", "document-closed");
        });
        await old.transact("layer.add", (tx) => tx.addRaster({ name: "after save" }));
        const next = await replacement;
        assert(await selected, format + " let an old edit through");
        assert(next !== old && next.getSnapshot().selection === null, format + " selected the new document");
        await engine.dispose();
      }
    });
    await test("the Worker rejects obsolete document epochs for edits, reads and durability", async () => {
      const engine = await engineIn(container());
      await engine.createDocument({ width: 64, height: 64 });
      const next = await engine.createDocument({ width: 128, height: 128 });
      // Epoch 1 belonged to the first handle. Bypass the SDK check to test the
      // Worker's execution-time guard, including durable's separate reply path.
      for (const [request, payload] of [
        ["select", { spec: { kind: "all" } }], ["snapshotHash", {}], ["durable", {}],
      ])
        assert(await rejectsWith(testing(engine).request(request, { ...payload, documentEpoch: 1 }), "CONFLICT", "document-closed"),
          "the Worker accepted a stale " + request);
      assert(next.getSnapshot().selection === null, "a stale request changed the new document");
      await next.transact("layer.add", (tx) => tx.addRaster());
      await next.durable();
      const hash = await next.snapshotHash();
      await testing(engine).crash();
      assert((await next.snapshotHash()) === hash, "the current epoch stopped working after restart");
    });
    await test("document replacement finishes the stroke on the old document first", async () => {
      const engine = await engineIn(container());
      const old = await engine.createDocument({ width: 64, height: 64 });
      let layer;
      await old.transact("layer.add", (tx) => (layer = tx.addRaster()));
      const stroke = old.tools.begin("brush", { layer, color: [1, 0, 0, 1] });
      stroke.input({ x: 10, y: 10 });
      stroke.input({ x: 40, y: 40 });
      const next = await engine.createDocument({ width: 128, height: 128 });
      await stroke.end();
      assert(!stroke.active && !old.activeTool, "the old stroke was left active");
      assert(old.getSnapshot().history.undo === 2, "the stroke was not committed to the old document");
      assert(next.getSnapshot().history.undo === 0 && next.getSnapshot().nodes.size === 1,
        "the old stroke changed the replacement document");
    });
    await test("restore and discard cannot race to delete the restored journal", async () => {
      const el = container();
      const namespace = name("restore-discard");
      let engine = await engineIn(el, { storage: { namespace } });
      const doc = await engine.createDocument({ width: 64, height: 64 });
      await doc.transact("layer.add", (tx) => tx.addRaster({ name: "kept" }));
      await doc.durable();
      const hash = await doc.snapshotHash();
      await engine.dispose();
      engine = await engineIn(el, { storage: { namespace } });
      const restored = engine.recovery.restore();
      const discarded = rejectsWith(engine.recovery.discard(), "CONFLICT", "no-recovery");
      const next = await restored;
      assert(await discarded, "discard deleted the journal after restore");
      await testing(engine).crash();
      assert((await next.snapshotHash()) === hash, "the restored journal was lost");
    });
    await test("journals are listed by namespace and deleted only when unused", async () => {
      const namespace = name("listed");
      const engine = await engineIn(container(), { storage: { namespace } });
      const doc = await engine.createDocument({ width: 64, height: 64 });
      await doc.durable();
      const listed = (await listRecoveryNamespaces()).find((n) => n.namespace === namespace);
      assert(listed && listed.bytes > 0 && listed.modified > 0, "namespace not listed: " + JSON.stringify(listed));
      assert(await rejectsWith(deleteRecovery(namespace), "CONFLICT", "namespace-in-use"), "deleted a journal in use");
      await engine.dispose();
      await deleteRecovery(namespace);
      assert(!(await listRecoveryNamespaces()).some((n) => n.namespace === namespace), "deleted namespace still listed");
    });
    await test("an engine without a journal writes nothing and shares no lock", async () => {
      const namespace = name("none");
      const a = await engineIn(container(), { storage: { namespace, recovery: false } });
      const b = await engineIn(container(), { storage: { namespace, recovery: false } });
      const doc = await a.createDocument({ width: 64, height: 64 });
      await doc.transact("layer.add", (tx) => tx.addRaster());
      await doc.durable();
      await b.createDocument({ width: 64, height: 64 });
      assert(!(await listRecoveryNamespaces()).some((n) => n.namespace === namespace), "a journal-free engine wrote a journal");
      assert(await rejectsWith(a.recovery.restore(), "NOT_FOUND"), "restore without a journal did not report NOT_FOUND");
    });
    await test("exportImage writes the displayed pixels, scaled and cropped", async () => {
      const engine = await engineIn(container(400, 300));
      const doc = await engine.createDocument({ width: 512, height: 256 });
      await quadrants(engine, doc);
      const png = await doc.exportImage();
      assert(png.type === "image/png", "type " + png.type);
      const bitmap = await createImageBitmap(png, { premultiplyAlpha: "none", colorSpaceConversion: "none" });
      assert(bitmap.width === 512 && bitmap.height === 256, `size ${bitmap.width}×${bitmap.height}`);
      const c = new OffscreenCanvas(512, 256), ctx = c.getContext("2d", { colorSpace: "srgb" });
      ctx.drawImage(bitmap, 0, 0);
      const exported = ctx.getImageData(0, 0, 512, 256).data;
      const tiles = [await unstable(engine).readTile(0, 0), await unstable(engine).readTile(1, 0)];
      for (const [x, y] of [[10, 10], [300, 10], [10, 200], [300, 200], [255, 127]]) {
        const t = tiles[x >> 8], s = ((y % 256) * 256 + (x % 256)) * 4, d = (y * 512 + x) * 4;
        const a = t[s + 3];
        for (let ch = 0; ch < 3; ch++) {
          const straight = a ? Math.round((t[s + ch] * 255) / a) : 0;
          assert(Math.abs(exported[d + ch] - straight) <= 1, `pixel ${x},${y} channel ${ch}: ${exported[d + ch]} vs ${straight}`);
        }
        assert(Math.abs(exported[d + 3] - a) <= 1, `alpha at ${x},${y}`);
      }
      const half = await createImageBitmap(await doc.exportImage({ scale: 0.5, type: "image/webp" }));
      assert(half.width === 256 && half.height === 128, `half size ${half.width}×${half.height}`);
      const crop = await createImageBitmap(await doc.exportImage({ region: { x0: 256, y0: 0, x1: 384, y1: 64 } }));
      assert(crop.width === 128 && crop.height === 64, `crop size ${crop.width}×${crop.height}`);
      for (const scale of [0, 1.5, Number.NaN])
        assert(await rejectsWith(doc.exportImage({ scale }), "INVALID_ARGUMENT", "scale"), "accepted scale " + scale);
    });
    await test("a document saved to a blob target opens with the same content", async () => {
      const engine = await engineIn(container());
      const doc = await engine.createDocument({ width: 256, height: 128 });
      await quadrants(engine, doc);
      const target = blobTarget();
      await doc.save(target);
      assert(!doc.getSnapshot().dirty, "save left the document dirty");
      const hash = await doc.snapshotHash();
      const reopened = await engine.open(target.blob);
      assert(reopened !== doc && engine.document === reopened, "open did not give a new handle");
      assert((await reopened.snapshotHash()) === hash, "reopened content differs");
    });
    await test("a replaced document handle refuses calls instead of editing the new document", async () => {
      const engine = await engineIn(container());
      const old = await engine.createDocument({ width: 64, height: 64 });
      await old.transact("layer.add", (tx) => tx.addRaster());
      const fresh = await engine.createDocument({ width: 32, height: 32 });
      const revision = fresh.getSnapshot().revision;
      assert(await rejectsWith(old.undo(), "CONFLICT", "document-closed"), "undo on the replaced handle was not refused");
      assert(await rejectsWith(old.transact("layer.add", (tx) => tx.addRaster()), "CONFLICT", "document-closed"),
        "transact on the replaced handle was not refused");
      assert(fresh.getSnapshot().revision === revision, "the replaced handle changed the new document");
    });
    await test("saving keeps unchanged nodes the same objects", async () => {
      const engine = await engineIn(container());
      const doc = await engine.createDocument({ width: 64, height: 64 });
      let a, b;
      await doc.transact("layer.add", (tx) => { a = tx.addRaster({ name: "a" }); b = tx.addRaster({ name: "b" }); });
      const before = doc.getSnapshot();
      let change;
      doc.subscribe((c) => (change = c));
      await doc.save(blobTarget());
      const after = doc.getSnapshot();
      assert(after.nodes.get(a) === before.nodes.get(a) && after.nodes.get(b) === before.nodes.get(b), "save replaced unchanged nodes");
      assert(change && change.nodes.updated.length === 0 && change.dirty, "the save reported node changes: " + JSON.stringify(change?.nodes));
    });
    await test("a stroke that cannot start reports its error once", async () => {
      const engine = await engineIn(container());
      const doc = await engine.createDocument({ width: 64, height: 64 });
      const errors = [];
      engine.on("error", (e) => errors.push(e));
      const stroke = doc.tools.begin("brush", { layer: "00000000-0000-7000-8000-000000000000" });
      stroke.input({ x: 10, y: 10 });
      await frameWhere(engine, () => true);
      for (let i = 0; i < 10; i++) await new Promise(requestAnimationFrame);
      assert(errors.length === 1, `${errors.length} error events for one failed stroke`);
      let code = null;
      try { await stroke.end(); } catch (e) { code = e.code; }
      assert(code && code === errors[0].code, `end() reported ${code}, the error event ${errors[0].code}`);
      assert(!doc.activeTool, "the failed stroke stayed active");
    });
    await test("each edit notifies subscribers once", async () => {
      const engine = await engineIn(container());
      const doc = await engine.createDocument({ width: 64, height: 64 });
      const changes = [];
      doc.subscribe((c) => changes.push(c));
      let layer;
      await doc.transact("layer.add", (tx) => (layer = tx.addRaster()));
      await doc.undo();
      await doc.redo();
      const stroke = doc.tools.begin("brush", { layer, color: [1, 0, 0, 1] });
      stroke.input({ x: 10, y: 10 });
      await stroke.end();
      assert(changes.length === 4, `${changes.length} notifications for 4 edits`);
    });
    await test("zoom stays within what the renderer draws", async () => {
      const engine = await engineIn(container());
      await engine.createDocument({ width: 64, height: 64 });
      const ratio = unstable(engine).pixelRatio();
      engine.viewport.set({ zoom: 10000 });
      assert(engine.viewport.get().zoom <= 128 / ratio + 1e-9, "zoom above the renderer's limit: " + engine.viewport.get().zoom);
      engine.viewport.set({ zoom: 1e-6 });
      assert(engine.viewport.get().zoom >= 0.01 / ratio - 1e-12, "zoom below the renderer's limit: " + engine.viewport.get().zoom);
    });
    await test("a reduced export reads the matching pyramid level", async () => {
      const engine = await engineIn(container(400, 300));
      const doc = await engine.createDocument({ width: 512, height: 256 });
      await quadrants(engine, doc);
      const half = await doc.exportImage({ scale: 0.5 });
      const bitmap = await createImageBitmap(half, { premultiplyAlpha: "none", colorSpaceConversion: "none" });
      assert(bitmap.width === 256 && bitmap.height === 128, `size ${bitmap.width}×${bitmap.height}`);
      const c = new OffscreenCanvas(256, 128), ctx = c.getContext("2d");
      ctx.drawImage(bitmap, 0, 0);
      const exported = ctx.getImageData(0, 0, 256, 128).data;
      const tile = await unstable(engine).readTileLevel(1, 0, 0);
      for (const [x, y] of [[5, 5], [200, 5], [5, 100], [200, 100]]) {
        const s = (y * 256 + x) * 4, d = (y * 256 + x) * 4, a = tile[s + 3];
        for (let ch = 0; ch < 3; ch++) {
          const straight = a ? Math.round((tile[s + ch] * 255) / a) : 0;
          assert(Math.abs(exported[d + ch] - straight) <= 1, `pixel ${x},${y} channel ${ch}: ${exported[d + ch]} vs ${straight}`);
        }
      }
    });
    await test("the back/forward cache pauses a journaled engine and restores it", async () => {
      const namespace = name("bfcache");
      const engine = await engineIn(container(), { storage: { namespace } });
      const doc = await engine.createDocument({ width: 128, height: 128 });
      await doc.transact("layer.add", (tx) => tx.addRaster({ name: "cached" }));
      await doc.durable();
      const hash = await doc.snapshotHash();
      const lockHeld = async () => (await navigator.locks.query()).held.some((l) => l.name === `alto-engine:${namespace}`);
      dispatchEvent(new PageTransitionEvent("pagehide", { persisted: true }));
      assert(engine.status.state === "restarting", "a cached page left the engine " + engine.status.state);
      let held = true;
      for (let i = 0; i < 50 && held; i++) held = (await lockHeld()) && (await new Promise((r) => setTimeout(r, 20)), true);
      assert(!held, "the cached page kept the namespace");
      await event(engine, "status", () => dispatchEvent(new PageTransitionEvent("pageshow", { persisted: true })), (s) => s.state === "ready");
      assert(engine.document === doc && (await doc.snapshotHash()) === hash, "the restored page lost its document");
      assert(await lockHeld(), "the restored engine runs without its namespace");
      await doc.transact("layer.add", (tx) => tx.addRaster());
    });
    await test("returning from the cache to a namespace taken meanwhile fails, and retry recovers", async () => {
      const namespace = name("bfcache-taken");
      const engine = await engineIn(container(), { storage: { namespace } });
      const doc = await engine.createDocument({ width: 64, height: 64 });
      await doc.durable();
      dispatchEvent(new PageTransitionEvent("pagehide", { persisted: true }));
      await new Promise((r) => setTimeout(r, 50));
      const other = await engineIn(container(), { storage: { namespace } });
      await event(engine, "status", () => dispatchEvent(new PageTransitionEvent("pageshow", { persisted: true })), (s) => s.state === "failed");
      assert(engine.status.error?.details?.reason === "namespace-in-use", "the failure was " + engine.status.error?.code);
      await other.dispose();
      await engine.retry();
      assert(engine.status.state === "ready" && engine.document === doc, "retry did not restore the document");
    });
    log("ALL " + results.length + " EMBED TESTS PASSED");
  } catch (error) {
    results.push({ name: String(error.stack || error), pass: false });
    log("FAIL " + error);
    throw error;
  } finally {
    const report = { date: new Date().toISOString(), suite: "embed", userAgent: navigator.userAgent, results };
    document.querySelector("#test-results").dataset.report = JSON.stringify(report);
  }
  return results;
}
