const assert = (ok, m) => {
  if (!ok) throw Error(m);
};
const close = (a, b, t = 2) => a.every((v, i) => Math.abs(v - b[i]) <= t);
const pixel = (a, x = 32, y = 32) =>
  Array.from(a.slice((y * 256 + x) * 4, (y * 256 + x) * 4 + 4));
const same = (a, b) => a.length === b.length && a.every((v, i) => v === b[i]);
const delay = (ms) => new Promise((r) => setTimeout(r, ms));
async function image(engine, color, w = 256, h = 256) {
  const c = new OffscreenCanvas(w, h),
    ctx = c.getContext("2d");
  ctx.fillStyle = color;
  ctx.fillRect(0, 0, w, h);
  return engine.assets.importImage(await c.convertToBlob());
}
async function stroke(
  doc,
  layer,
  tool = "brush",
  samples = [
    { x: 240, y: 128, pressure: 1 },
    { x: 280, y: 128, pressure: 0.5 },
  ],
) {
  const s = doc.tools.begin(tool, {
    layer,
    tip: { size: 48, hardness: 0.8, spacing: 0.1 },
    color: [0.2, 0.4, 0.8, 1],
  });
  for (const p of samples) s.input(p);
  await s.end();
}
async function event(engine, name, action) {
  let off;
  const p = new Promise((resolve, reject) => {
    const t = setTimeout(() => {
      off?.();
      reject(Error("timeout " + name));
    }, 15000);
    off = engine.on(name, () => {
      clearTimeout(t);
      off();
      resolve();
    });
  });
  await action();
  await p;
}
async function settled(engine) {
  return engine.unstable.settle();
}
export async function runTests(engine, benchmark, log) {
  const results = [];
  async function test(name, fn) {
    const start = performance.now();
    await fn();
    const r = { name, pass: true, ms: performance.now() - start };
    results.push(r);
    log("PASS " + name + " " + r.ms.toFixed(1) + " ms");
  }
  try {
    if (benchmark) {
      await benches(engine, log, results);
      return;
    }
    let doc, lower, upper, assetRed, assetBlue;
    await test("empty scene GPU validation / Normal / Multiply / Screen / opacity / visibility", async () => {
      doc = await engine.createDocument({ width: 512, height: 256 });
      await engine.request("validateFrame");
      assetRed = await image(engine, "rgb(255,0,0)");
      assetBlue = await image(engine, "rgba(0,0,255,.5)");
      await doc.transact("two images", (tx) => {
        lower = tx.addRasterFromAsset(assetRed);
        upper = tx.addRasterFromAsset(assetBlue);
      });
      let tile = await engine.unstable.readTile(0, 0);
      assert(close(pixel(tile), [127, 0, 128, 255]), "normal " + pixel(tile));
      await doc.transact("multiply", (tx) =>
        tx.setProps(upper, { blend: "multiply" }),
      );
      tile = await engine.unstable.readTile(0, 0);
      assert(close(pixel(tile), [127, 0, 0, 255]), "multiply " + pixel(tile));
      await doc.transact("screen", (tx) =>
        tx.setProps(upper, { blend: "screen" }),
      );
      tile = await engine.unstable.readTile(0, 0);
      assert(close(pixel(tile), [255, 0, 128, 255]), "screen " + pixel(tile));
      await doc.transact("opacity", (tx) =>
        tx.setProps(upper, { blend: "normal", opacity: 0.5 }),
      );
      tile = await engine.unstable.readTile(0, 0);
      assert(close(pixel(tile), [191, 0, 64, 255]), "opacity " + pixel(tile));
      await doc.transact("hide", (tx) =>
        tx.setProps(upper, { visible: false }),
      );
      tile = await engine.unstable.readTile(0, 0);
      assert(close(pixel(tile), [255, 0, 0, 255]), "visibility " + pixel(tile));
    });
    await test("non-destructive transform / reorder / group isolation", async () => {
      await doc.transact("transform", (tx) => {
        tx.setProps(upper, { visible: true, opacity: 1 });
        tx.setTransform(upper, {
          x: 64,
          y: 0,
          scaleX: 1,
          scaleY: 1,
          rotation: 0,
        });
      });
      let tile = await engine.unstable.readTile(0, 0);
      assert(close(pixel(tile, 32), [255, 0, 0, 255]), "translation left");
      assert(close(pixel(tile, 96), [127, 0, 128, 255]), "translation right");
      await doc.transact("reorder", (tx) =>
        tx.moveNode(upper, doc.view.root.id, 0),
      );
      tile = await engine.unstable.readTile(0, 0);
      assert(close(pixel(tile, 96), [255, 0, 0, 255]), "reorder");
      let group;
      await doc.transact("isolation", (tx) => {
        group = tx.addGroup({ isolated: true });
        tx.moveNode(lower, group, 0);
        tx.moveNode(upper, group, 1);
        tx.setProps(group, { opacity: 0.5 });
      });
      tile = await engine.unstable.readTile(0, 0);
      assert(
        close(pixel(tile, 32), [128, 0, 0, 128]),
        "isolated group " + pixel(tile, 32),
      );
      await doc.undo();
      await doc.redo();
      assert(
        same(tile, await engine.unstable.readTile(0, 0)),
        "group undo/redo",
      );
    });
    await test("low-zoom prefetch matches direct composite pixels", async () => {
      for (const size of [512, 300]) {
        doc = await engine.createDocument({ width: size, height: size });
        const red = await image(engine, "rgba(255,0,0,.5)", size, size);
        const blue = await image(engine, "rgba(0,0,255,.5)", size, size);
        await doc.transact("two translucent layers", (tx) => {
          tx.addRasterFromAsset(red);
          tx.addRasterFromAsset(blue);
        });
        engine.viewport.set({ center: [size / 2, size / 2], zoom: 0.5, rotation: 0 });
        await engine.request("setVisibleTileJobs", { enabled: false });
        await engine.request("resetRenderCache", { cpu: true });
        const direct = await engine.unstable.readTileLevel(1, 0, 0);
        await engine.request("setVisibleTileJobs", { enabled: true });
        await engine.request("resetRenderCache", { cpu: true });
        engine.viewport.set({ center: [size / 2, size / 2], zoom: 0.5, rotation: 0.17 });
        await settled(engine);
        assert(same(direct, await engine.unstable.readTileLevel(1, 0, 0)),
          `cold visible job differs from direct composite for ${size}px canvas`);
        await engine.request("enablePrefetch");
        await engine.request("validateFrame"); // Clear the direct tile and rebuild the visible level.
        engine.viewport.set({ center: [size / 2, size / 2], zoom: 1, rotation: 0 });
        let frame = await settled(engine);
        for (let i = 0; i < 20 && frame.prefetch.ready < frame.prefetch.candidates; i++) {
          await delay(10);
          frame = await settled(engine);
        }
        assert(frame.prefetch.candidates === 1 && frame.prefetch.ready === 1,
          `low-zoom parent was not prepared for ${size}px canvas`);
        assert(same(direct, await engine.unstable.readTileLevel(1, 0, 0)),
          `prefetched parent differs from direct composite for ${size}px canvas`);
      }
    });
    await test("cold jobs preserve transformed isolated groups through edit and Undo", async () => {
      doc = await engine.createDocument({ width: 600, height: 400 });
      const a = await image(engine, "rgba(240,30,10,.6)", 600, 400);
      const b = await image(engine, "rgba(10,50,240,.5)", 600, 400);
      let top;
      await doc.transact("group", tx => {
        const group = tx.addGroup({ isolated: true });
        tx.setProps(group, { opacity: 0.7 });
        tx.addRasterFromAsset(a);
        top = tx.addRasterFromAsset(b);
        tx.moveNode(top, group, 0);
        tx.setProps(top, { blend: "multiply" });
        tx.setTransform(top, { x: 23, y: 11, scaleX: 0.9, scaleY: 1.1, rotation: 0.13 });
      });
      for (const edit of [false, true, false]) {
        if (edit) await doc.transact("hide", tx => tx.setProps(top, { visible: false }));
        // Set the target level before clearing: otherwise an automatic tick
        // can populate level-0 children and change the reference into a pyramid
        // downsample. Transformed edge sampling differs from direct level 1.
        engine.viewport.set({ center: [256, 200], zoom: 0.5, rotation: 0.1 });
        await engine.request("setVisibleTileJobs", { enabled: false });
        await engine.request("resetRenderCache", { cpu: true });
        const reference = await engine.unstable.readTileLevel(1, 0, 0);
        await engine.request("setVisibleTileJobs", { enabled: true });
        await engine.request("resetRenderCache", { cpu: true });
        engine.viewport.set({ center: [256, 200], zoom: 0.5, rotation: 0.1 });
        await settled(engine);
        assert(same(reference, await engine.unstable.readTileLevel(1, 0, 0)), "cold group pixels differ");
        if (edit) await doc.undo();
      }
      await engine.request("enablePrefetch");
    });
    await test("full display cache can yield and replace tiles without orphaning slots", async () => {
      doc = await engine.createDocument({ width: 300, height: 300 });
      const asset = await image(engine, "rgba(40,120,210,.6)", 300, 300);
      await doc.transact("layer", tx => tx.addRasterFromAsset(asset));
      await engine.request("resetRenderCache", { cpu: true });
      for (let x = 1000; x < 1256; x++) await engine.unstable.readTile(x, 0);
      engine.viewport.set({ center: [150, 150], zoom: 0.5, rotation: 0 });
      await settled(engine);
      const tile = await engine.unstable.readTileLevel(1, 0, 0);
      assert(pixel(tile)[3] > 0, "full cache lost the visible tile");
      await engine.request("enablePrefetch");
    });
    let brushLayer, painted;
    await test("GPU brush / pressure / tile boundary / exact Undo-Redo / eraser", async () => {
      doc = await engine.createDocument({ width: 512, height: 256 });
      await doc.transact("layer", (tx) => (brushLayer = tx.addRaster()));
      const empty = await engine.unstable.readTile(0, 0);
      await stroke(doc, brushLayer);
      painted = await engine.unstable.readTile(0, 0);
      const reference = new Uint8Array(
        await engine.request("cpuReference", {
          config: {
            size: 48,
            hardness: 0.8,
            spacing: 0.1,
            opacity: 1,
            flow: 1,
            pressure_size: true,
            pressure_opacity: false,
          },
          color: [0.2, 0.4, 0.8, 1],
          samples: [240, 128, 1, 0, 0, 0, 280, 128, 0.5, 0, 0, 1],
        }),
      );
      assert(
        painted.every((v, i) => Math.abs(v - reference[i]) <= 2),
        "CPU/GPU brush mismatch",
      );
      assert(
        pixel(painted, 245, 128)[3] > 200,
        "brush did not paint " + pixel(painted, 245, 128),
      );
      const right = await engine.unstable.readTile(1, 0);
      assert(pixel(right, 12, 128)[3] > 0, "tile boundary missing");
      await doc.undo();
      assert(
        same(empty, await engine.unstable.readTile(0, 0)),
        "undo changed pixels",
      );
      await doc.redo();
      assert(
        same(painted, await engine.unstable.readTile(0, 0)),
        "redo changed pixels",
      );
      await stroke(doc, brushLayer, "eraser", [
        { x: 245, y: 128, pressure: 1 },
      ]);
      const erased = await engine.unstable.readTile(0, 0);
      assert(pixel(erased, 245, 128)[3] === 0, "eraser did not clear");
      await doc.undo();
    });
    await test(".alto ZIP save/open preserves pixels and snapshot hash", async () => {
      const hash = doc.view.snapshotHash;
      const bytes = await engine.unstable.saveBytes();
      doc = await engine.openDocument(new Blob([bytes]));
      assert(doc.view.snapshotHash === hash, "snapshot hash changed");
      assert(
        same(painted, await engine.unstable.readTile(0, 0)),
        "saved pixels changed",
      );
    });
    await test("atomic save survives write failure and writer Worker termination", async () => {
      await doc.transact("unsaved marker", (tx) =>
        tx.setProps(brushLayer, { name: "Unsaved paint" }),
      );
      const before = doc.view.snapshotHash;
      const root = await navigator.storage.getDirectory();
      const file = await root.getFileHandle("alto-test-atomic-save.tmp", {
        create: true,
      });
      const original = new Uint8Array([65, 76, 84, 79, 1, 2, 3]);
      let w = await file.createWritable();
      await w.write(original);
      await w.close();
      let rejected = false;
      try {
        await doc.save({
          kind: "injected-abort",
          writeArchive: async (stream) => {
            const bytes = new Uint8Array(
              await new Response(stream).arrayBuffer(),
            );
            const writer = await file.createWritable();
            await writer.write(bytes.subarray(0, Math.floor(bytes.length / 2)));
            await writer.abort();
            throw Error("injected write failure");
          },
        });
      } catch {
        rejected = true;
      }
      assert(rejected, "save should fail");
      assert(
        same(
          original,
          new Uint8Array(await (await file.getFile()).arrayBuffer()),
        ),
        "old file changed",
      );
      assert(
        doc.view.snapshotHash === before && doc.view.dirty,
        "save failure changed document",
      );
      const writerURL = URL.createObjectURL(
        new Blob(
          [
            `let pendingWriter; self.onmessage = async ({data}) => {
          try {
            pendingWriter = await data.file.createWritable();
            await pendingWriter.write(new Uint8Array([99, 98, 97, 96]));
            postMessage({ready: true});
          } catch (error) { postMessage({error: String(error)}); }
        };`,
          ],
          { type: "text/javascript" },
        ),
      );
      const interruptedWriter = new Worker(writerURL);
      try {
        await new Promise((resolve, reject) => {
          const timeout = setTimeout(
            () => reject(Error("writer worker timeout")),
            10000,
          );
          interruptedWriter.onmessage = ({ data }) => {
            clearTimeout(timeout);
            data.error ? reject(Error(data.error)) : resolve();
          };
          interruptedWriter.onerror = (error) => {
            clearTimeout(timeout);
            reject(error);
          };
          interruptedWriter.postMessage({ file });
        });
      } finally {
        interruptedWriter.terminate();
        URL.revokeObjectURL(writerURL);
      }
      await delay(0);
      assert(
        same(
          original,
          new Uint8Array(await (await file.getFile()).arrayBuffer()),
        ),
        "terminated writer changed the previous file",
      );
      await root.removeEntry("alto-test-atomic-save.tmp");
    });
    await test("invalid transaction leaves mirror and history unchanged", async () => {
      const hash = doc.view.snapshotHash;
      const undo = doc.view.history.undo;
      let rejected = false;
      try {
        await doc.transact("invalid", (tx) => {
          const id = tx.addRaster();
          tx.setProps(id, { opacity: 2 });
        });
      } catch {
        rejected = true;
      }
      assert(
        rejected &&
          doc.view.snapshotHash === hash &&
          doc.view.history.undo === undo,
        "atomic validation failed",
      );
    });
    await test("device loss rebuilds GPU from CPU content", async () => {
      await event(engine, "gpuRestored", () => engine.unstable.destroyGpu());
      assert(
        same(painted, await engine.unstable.readTile(0, 0)),
        "device loss pixels changed",
      );
    });
    await test("Worker restart replays flushed OPFS journal", async () => {
      await doc.transact("recover marker", (tx) =>
        tx.setProps(brushLayer, { name: "Recovered paint" }),
      );
      const hash = doc.view.snapshotHash;
      await engine.unstable.crash();
      doc = engine.document;
      assert(doc.view.snapshotHash === hash, "journal recovery hash mismatch");
      assert(
        same(painted, await engine.unstable.readTile(0, 0)),
        "journal recovery pixels mismatch",
      );
    });
    await test("simultaneous Worker restart requests share one recovery", async () => {
      const hash = doc.view.snapshotHash;
      let replacements = 0;
      let nested;
      const off = engine.on("canvasReplaced", () => { replacements++; });
      const offRecovering = engine.on("recovering", () => { nested = engine.unstable.crash(); });
      try {
        const first = engine.unstable.crash();
        const second = engine.unstable.crash();
        assert(first === second, "restart requests did not share a promise");
        assert(first === nested, "reentrant restart did not share a promise");
        await first;
      } finally { off(); offRecovering(); }
      doc = engine.document;
      assert(replacements === 1, "Worker canvas was replaced more than once");
      assert(doc.view.snapshotHash === hash, "concurrent restart changed the document");
    });
    await test("uncloneable request does not leave a pending reply", async () => {
      const before = engine.unstable.pendingRequests();
      let rejected = false;
      try { await engine.request("view", { invalid: () => true }); }
      catch { rejected = true; }
      assert(rejected, "uncloneable request was accepted");
      assert(engine.unstable.pendingRequests() === before, "failed postMessage leaked a pending reply");
      await engine.request("view");
    });
    await test("missing Worker script stops once and retries without corrupting the journal", async () => {
      const hash = doc.view.snapshotHash;
      engine.unstable.setWorkerURL("/missing-alto-engine-worker.js");
      let code = null;
      try { await engine.unstable.crash(); }
      catch (error) { code = error.code; }
      finally { engine.unstable.setWorkerURL(undefined); }
      assert(code === "WORKER_UNAVAILABLE", "missing script was not classified: " + code);
      assert(engine.lifecycleState === "failed" && engine.engineFailure, "Worker failure did not stop the engine");
      assert(!engine.storageFailure, "Worker failure was mislabeled as saved-data corruption");
      assert(engine.document === null, "stale document remained active");
      assert(!document.querySelector("#engine-failure").hidden, "retry control was not shown");
      let blocked = false;
      try { await engine.request("save"); }
      catch (error) { blocked = error.code === "WORKER_UNAVAILABLE"; }
      assert(blocked, "editing request continued after Worker failure");
      await event(engine, "recovered", () => document.querySelector("#retry-worker").click());
      doc = engine.document;
      assert(engine.lifecycleState === "running" && !engine.engineFailure, "retry did not restore the engine");
      assert(doc?.view.snapshotHash === hash, "retry opened the wrong document");
      assert(same(painted, await engine.unstable.readTile(0, 0)), "retry changed saved pixels");
      assert(document.querySelector("#engine-failure").hidden, "failure controls remained open");
    });
    await test("truncated journal tail is excluded during recovery", async () => {
      const hash = doc.view.snapshotHash;
      await engine.unstable.corruptJournalTail();
      await engine.unstable.crash();
      doc = engine.document;
      assert(
        doc.view.snapshotHash === hash,
        "truncated journal lost valid edits",
      );
      assert(
        doc.view.warnings.some((w) => w.includes("不完全")),
        "truncation warning missing",
      );
    });
    await test("damaged latest checkpoint falls back to the previous generation", async () => {
      const before = await engine.unstable.saveBytes();
      const previousHash = doc.view.snapshotHash;
      doc = await engine.openDocument(new Blob([before]));
      doc = await engine.createDocument({ width: 64, height: 64 });
      await engine.unstable.corruptCheckpoint();
      await engine.unstable.crash();
      doc = engine.document;
      assert(doc.view.snapshotHash === previousHash, "previous checkpoint was not opened");
      assert(doc.view.warnings.some((w) => w.includes("前の世代")), "fallback warning missing");
      assert(same(painted, await engine.unstable.readTile(0, 0)), "fallback pixels changed");
    });
    await test("checkpoint CRC failure falls back without losing the older journal", async () => {
      const before = await engine.unstable.saveBytes();
      const previousHash = doc.view.snapshotHash;
      doc = await engine.openDocument(new Blob([before]));
      doc = await engine.createDocument({ width: 64, height: 64 });
      await engine.unstable.corruptCheckpointCrc();
      await engine.unstable.crash();
      doc = engine.document;
      assert(doc.view.snapshotHash === previousHash, "CRC fallback opened the wrong generation");
      assert(doc.view.warnings.some((w) => w.includes("前の世代")), "CRC fallback warning missing");
    });
    await test("V1 checkpoint opens and the next checkpoint migrates to V2", async () => {
      const hash = doc.view.snapshotHash;
      await engine.unstable.legacyCheckpoint();
      await engine.unstable.crash();
      doc = engine.document;
      assert(doc.view.snapshotHash === hash, "V1 checkpoint did not open");
      const bytes = await engine.unstable.saveBytes();
      doc = await engine.openDocument(new Blob([bytes]));
      await engine.unstable.crash();
      doc = engine.document;
      assert(doc.view.snapshotHash === hash, "V2 migration changed the document");
      assert(same(painted, await engine.unstable.readTile(0, 0)), "migration pixels changed");
    });
    await test("failed append freezes edits and preserves live emergency export", async () => {
      const before = doc.view.revision;
      const name = doc.view.node(brushLayer).name;
      const baseline = await engine.unstable.saveBytes();
      await engine.unstable.injectStorageFault("beforeAppend");
      let failed = false;
      try {
        await doc.transact("unconfirmed edit", (tx) =>
          tx.setProps(brushLayer, { name: "Unconfirmed paint" }),
        );
      } catch (error) {
        failed = error.code === "STORAGE";
      }
      assert(failed && engine.storageFailure, "append failure was not surfaced");
      assert(!document.querySelector("#storage-failure").hidden, "recovery controls were not shown");
      assert(document.querySelector("main").inert, "editor was not disabled");
      assert(doc.view.revision === before, "unconfirmed patch reached the view");
      const emergency = await engine.exportEmergencyBytes();
      assert(emergency.length > 100, "emergency export failed");
      let blocked = false;
      try { await doc.undo(); } catch (error) { blocked = error.code === "STORAGE"; }
      assert(blocked, "editing continued after storage failure");
      await event(engine, "recovered", () =>
        document.querySelector("#recover-storage").click(),
      );
      await delay(0);
      doc = engine.document;
      assert(document.querySelector("#storage-failure").hidden, "recovery controls remained open");
      assert(!document.querySelector("main").inert, "editor remained disabled");
      assert(!document.querySelector("#recovery-result").hidden, "recovery result was not shown");
      assert(doc.view.revision === before, "confirmed journal changed unexpectedly");
      assert(doc.view.node(brushLayer).name === name, "unconfirmed edit was replayed");
      doc = await engine.openDocument(new Blob([emergency]));
      assert(doc.view.node(brushLayer).name === "Unconfirmed paint", "emergency export lost live edit");
      doc = await engine.openDocument(new Blob([baseline]));
    });
    await test("lost append reply replays once and reports a changed recovery", async () => {
      const before = doc.view.revision;
      await engine.unstable.injectStorageFault("afterAppend");
      let failed = false;
      try {
        await doc.transact("reply lost", (tx) =>
          tx.setProps(brushLayer, { name: "Confirmed despite lost reply" }),
        );
      } catch (error) {
        failed = error.code === "STORAGE";
      }
      assert(failed && engine.storageFailure, "lost reply was not surfaced");
      assert(doc.view.revision === before, "unacknowledged patch reached the view");
      const recovered = await engine.recoverAfterStorageFailure();
      doc = recovered.document;
      assert(recovered.matchesDisplay === false, "recovery difference was hidden");
      assert(doc.view.revision === before + 1, "lost reply replayed twice or not at all");
      assert(doc.view.node(brushLayer).name === "Confirmed despite lost reply", "journal edit missing");
    });
    await test("PNG/JPEG decoding to raster layers", async () => {
      const c = new OffscreenCanvas(64, 64),
        ctx = c.getContext("2d");
      ctx.fillStyle = "#00ff00";
      ctx.fillRect(0, 0, 64, 64);
      for (const type of ["image/png", "image/jpeg"]) {
        const a = await engine.assets.importImage(
          await c.convertToBlob({ type }),
        );
        assert(a.width === 64 && a.height === 64, "decoded size");
        await doc.transact("image decode", (tx) => tx.addRasterFromAsset(a));
      }
      const p = pixel(await engine.unstable.readTile(0, 0), 16, 16);
      assert(p[1] > 240 && p[3] === 255, "JPEG/PNG color " + p);
    });
    await test("second engine cannot overwrite the active recovery journal", async () => {
      const { AltoEngine } = await import(
        "../../packages/alto-sdk/dist/index.js"
      );
      const hash = doc.view.snapshotHash;
      let rejected = false;
      try {
        await AltoEngine.create({ canvas: document.createElement("canvas") });
      } catch (error) {
        rejected = error.code === "CONFLICT";
      }
      assert(
        rejected && doc.view.snapshotHash === hash,
        "second engine acquired the journal",
      );
    });
    await test("two damaged journals freeze editing until an external file opens", async () => {
      const backup = await engine.unstable.saveBytes();
      const hash = doc.view.snapshotHash;
      await engine.unstable.corruptBothCheckpoints();
      let rejected = false;
      try { await engine.unstable.crash(); }
      catch (error) { rejected = error.code === "CORRUPT_DATA"; }
      assert(rejected && engine.storageFailure?.code === "CORRUPT_DATA", "damage was hidden");
      assert(engine.document === null, "stale document remained editable");
      assert(!document.querySelector("#storage-failure").hidden, "damage controls were not shown");
      assert(!document.querySelector("#open-recovery-file").hidden, "external recovery was not offered");
      doc = await engine.openDocument(new Blob([backup]));
      assert(doc.view.snapshotHash === hash, "external recovery changed the document");
      assert(!engine.storageFailure, "editing stayed frozen after external recovery");
      assert(document.querySelector("#storage-failure").hidden, "damage controls stayed open");
    });
    log("ALL " + results.length + " TESTS PASSED");
  } catch (e) {
    results.push({ pass: false, error: String(e) });
    log("FAIL " + e);
    throw e;
  } finally {
    const canvas = engine.canvas.getBoundingClientRect();
    document.querySelector("#test-results").dataset.report = JSON.stringify({
      date: new Date().toISOString(),
      userAgent: navigator.userAgent,
      crossOriginIsolated,
      viewport: {
        width: innerWidth,
        height: innerHeight,
        dpr: devicePixelRatio,
        canvasCss: [canvas.width, canvas.height],
        canvasPixels: [engine.canvas.width, engine.canvas.height],
        benchmarkView: new URLSearchParams(location.search).get("benchmarkView") || "physical",
      },
      results,
    });
  }
}
async function benches(engine, log, results) {
  const profiling = new URLSearchParams(location.search).has("profile");
  const sameView = new URLSearchParams(location.search).get("benchmarkView") === "css";
  const zoomScale = sameView ? engine.effectiveDpr : 1;
  if (profiling) await engine.request("setProfiling", { enabled: true });
  let doc = await engine.createDocument({ width: 3840, height: 2160 });
  const layers = [];
  log("Preparing 4K × 20 translucent raster layers…");
  for (let i = 0; i < 20; i++) {
    const c = new OffscreenCanvas(3840, 2160),
      ctx = c.getContext("2d");
    ctx.fillStyle = `rgba(${40 + i * 7},${70 + i * 4},${110 + i * 3},.5)`;
    ctx.fillRect(0, 0, 3840, 2160);
    for (let y = 0; y < 2160; y += 256)
      for (let x = 0; x < 3840; x += 256) {
        ctx.fillStyle = `rgba(${((x / 256) * 17 + i * 11) % 255},${((y / 256) * 29 + i * 7) % 255},180,.45)`;
        ctx.fillRect(x + 7, y + 7, 220, 220);
      }
    assert(ctx.getImageData(0, 0, 1, 1).data[3] < 255, "benchmark layer became opaque");
    const a = await engine.assets.importImage(await c.convertToBlob());
    await doc.transact("benchmark image", (tx) =>
      layers.push(tx.addRasterFromAsset(a, { name: "Layer " + i })),
    );
    if (i % 5 === 4) log("Prepared " + (i + 1) + " layers");
  }
  // Cold runs have no opportunity for parent prefetch. The hook clears GPU
  // inputs and outputs; C3 additionally drops the decoded CPU tile cache.
  for (const jobs of [false, true]) {
    await engine.request("setVisibleTileJobs", { enabled: jobs });
    for (const cpu of [false, true]) {
      await engine.request("resetRenderCache", { cpu });
      engine.viewport.set({ center: [1920, 1080], zoom: 0.5 * zoomScale, rotation: 0 });
      const began = performance.now();
      const coldFrame = await settled(engine);
      results.push({ id: (cpu ? "C3 CPU+GPU cold ms" : "C2 GPU cold ms") + (jobs ? "" : " (synchronous reference)"),
        value: performance.now() - began, pass: true, profile: coldFrame.profile,
        canvasPixels: [engine.canvas.width, engine.canvas.height],
        physicalZoom: engine.viewport.zoom });
      log("COLD " + JSON.stringify(results.at(-1)));
    }
  }
  await engine.request("resetRenderCache", { cpu: false });
  await engine.request("enablePrefetch");
  engine.viewport.set({ center: [1920, 1080], zoom: zoomScale, rotation: 0 });
  let frame = await settled(engine);
  const record = (id, value, target, pass) => {
    results.push({ id, value, target, pass });
    log(
      `${pass ? "PASS" : "MISS"} ${id}: ${JSON.stringify(value)} target ${target}`,
    );
  };
  const memory = [];
  memory.push(frame);
  const frames = [];
  for (let i = 0; i < 120; i++) {
    const start = performance.now();
    engine.viewport.set({
      center: [1920 + Math.sin(i * 0.07) * 100, 1080 + Math.cos(i * 0.07) * 60],
    });
    frame = await settled(engine);
    frames.push(performance.now() - start);
    await new Promise(requestAnimationFrame);
  }
  frames.sort((a, b) => a - b);
  record(
    "B1 GPU-complete frame p95 ms",
    frames[Math.floor(frames.length * 0.95)],
    16.7,
    frames[Math.floor(frames.length * 0.95)] <= 16.7,
  );
  const prefetchBeforeZoom = frame.prefetch;
  let start = performance.now();
  engine.viewport.set({ center: [1920, 1080], zoom: 0.5 * zoomScale });
  frame = await settled(engine);
  const zoom = performance.now() - start;
  if (profiling) log("B2 profile: " + JSON.stringify(frame.profile));
  record("B2 zoom ms", zoom, 250, zoom <= 250);
  if (profiling) results.at(-1).profile = frame.profile;
  results.at(-1).prefetchBeforeZoom = prefetchBeforeZoom;
  const canvasCss = engine.canvas.getBoundingClientRect();
  results.at(-1).canvasCss = [canvasCss.width, canvasCss.height];
  results.at(-1).canvasPixels = [engine.canvas.width, engine.canvas.height];
  results.at(-1).physicalZoom = engine.viewport.zoom;
  results.at(-1).cssZoom = engine.viewport.getCssZoom();
  results.at(-1).visibleDocument = {
    width: engine.canvas.width / engine.viewport.zoom,
    height: engine.canvas.height / engine.viewport.zoom,
  };
  engine.viewport.set({ zoom: zoomScale });
  await settled(engine);
  const s = doc.tools.begin("brush", {
    layer: layers.at(-1),
    tip: { size: 100 },
    color: [1, 0, 0, 1],
  });
  const latencies = [];
  for (let i = 0; i < 30; i++) {
    start = performance.now();
    s.input({ x: 1800 + i * 8, y: 1080 + Math.sin(i * 0.2) * 40, pressure: 1 });
    await s.flush();
    memory.push(await settled(engine));
    latencies.push(performance.now() - start);
    await new Promise(requestAnimationFrame);
  }
  latencies.sort((a, b) => a - b);
  const latency = latencies[Math.floor(latencies.length * 0.95)];
  await s.end();
  record(
    "B3 continuous input-to-present GPU-complete p95 ms",
    latency,
    "2 frames (~33.4 ms)",
    latency <= 33.4,
  );
  const full = doc.tools.begin("brush", {
    layer: layers.at(-1),
    tip: { size: 2048, spacing: 0.15 },
    color: [0.1, 0.2, 0.3, 1],
  });
  for (let y = 0; y < 2160; y += 512) {
    for (let x = 0; x < 3840; x += 512) full.input({ x, y, pressure: 1 });
  }
  await full.flush();
  memory.push(await settled(engine));
  await full.end();
  memory.push(await settled(engine));
  start = performance.now();
  await doc.undo();
  const undo = performance.now() - start;
  record("B4 full-canvas Undo ms", undo, 50, undo <= 50);
  start = performance.now();
  const bytes = await engine.unstable.saveBytes();
  const save = performance.now() - start;
  start = performance.now();
  doc = await engine.openDocument(new Blob([bytes]));
  const open = performance.now() - start;
  record(
    "B5 save/open ms",
    { save, open },
    "3000 / 2000",
    save <= 3000 && open <= 2000,
  );
  frame = await settled(engine);
  memory.push(frame);
  const peakGpu = Math.max(...memory.map((f) => f.gpuBytes)),
    peakWasm = Math.max(...memory.map((f) => f.wasmBytes));
  record(
    "B6 sampled peak memory MiB",
    { gpu: peakGpu / 1048576, wasm: peakWasm / 1048576 },
    "512 / 1024",
    peakGpu <= 512 * 1048576 && peakWasm <= 1024 * 1048576,
  );
  const bootHash = doc.view.snapshotHash;
  const bootStart = performance.now();
  await engine.unstable.crash();
  assert(
    engine.document.view.snapshotHash === bootHash,
    "restart lost saved document",
  );
  await settled(engine);
  const boot = performance.now() - bootStart;
  record("B7 cached Worker restart + first draw ms", boot, 1500, boot <= 1500);
  log("BENCHMARK COMPLETE");
}
