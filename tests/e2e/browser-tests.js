import { unstable } from "../../packages/alto-sdk/dist/unstable.js";
import { testing } from "../../packages/alto-sdk/dist/testing.js";
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
/** Runs `action` and waits for an engine event that `match` accepts. */
async function event(engine, name, action, match = () => true) {
  let off;
  const p = new Promise((resolve, reject) => {
    const t = setTimeout(() => {
      off?.();
      reject(Error("timeout " + name));
    }, 15000);
    off = engine.on(name, (value) => {
      if (!match(value)) return;
      clearTimeout(t);
      off();
      resolve();
    });
  });
  await action();
  await p;
}
/** Runs `action` and waits until the GPU device is restored. */
async function gpuRestored(engine, action) {
  let off;
  const p = new Promise((resolve, reject) => {
    const t = setTimeout(() => {
      off?.();
      reject(Error("timeout gpuRestored"));
    }, 15000);
    off = unstable(engine).onGpu((e) => {
      if (e !== "restored") return;
      clearTimeout(t);
      off();
      resolve();
    });
  });
  await action();
  await p;
}
const ready = (s) => s.state === "ready";
async function settled(engine) {
  return unstable(engine).settle();
}
/**
 * Six layers whose 256 px tiles all differ (content addressing cannot merge them), seen
 * at zoom 0.5: each display tile needs up to 24 inputs, more than a job decodes inline.
 * cold(workers) renders from empty caches with or without the decode pool.
 */
async function decodeFixture(engine) {
  const doc = await engine.createDocument({ width: 1024, height: 512 });
  const assets = [];
  for (let i = 0; i < 6; i++) {
    const c = new OffscreenCanvas(1024, 512), ctx = c.getContext("2d");
    ctx.fillStyle = `rgba(${40 * i},${200 - 30 * i},120,.4)`;
    ctx.fillRect(0, 0, 1024, 512);
    for (let y = 0; y < 512; y += 256)
      for (let x = 0; x < 1024; x += 256) {
        ctx.fillStyle = `rgba(${(x / 4 + i * 30) % 255},${(y / 2 + i * 50) % 255},60,.5)`;
        ctx.fillRect(x + 16, y + 16, 200, 200);
      }
    assets.push(await engine.assets.importImage(await c.convertToBlob()));
  }
  await doc.transact("layers", (tx) => assets.forEach((a) => tx.addRasterFromAsset(a)));
  const view = { center: [512, 256], zoom: 0.5, rotation: 0 };
  const cold = async (workers, { keepPool = false } = {}) => {
    if (!keepPool && !(await testing(engine).request("setDecodeWorkers", { enabled: workers })) && workers)
      return null;
    // Set the level before clearing, so no tick composes level 0 first.
    engine.viewport.set(view);
    await testing(engine).request("resetRenderCache", { cpu: true });
    engine.viewport.set(view);
    const frame = await settled(engine);
    return {
      frame,
      tiles: [await unstable(engine).readTileLevel(1, 0, 0), await unstable(engine).readTileLevel(1, 1, 0)],
    };
  };
  return { doc, cold };
}
export async function runTests(engine, benchmark, log) {
  const results = [];
  let activeTest = null;
  async function test(name, fn) {
    activeTest = name;
    log("RUN " + name);
    const start = performance.now();
    await fn();
    const r = { name, pass: true, ms: performance.now() - start };
    results.push(r);
    log("PASS " + name + " " + r.ms.toFixed(1) + " ms");
    activeTest = null;
  }
  try {
    if (benchmark) {
      await benches(engine, log, results);
      return;
    }
    let doc, lower, upper, assetRed, assetBlue;
    await test("empty scene GPU validation / Normal / Multiply / Screen / opacity / visibility", async () => {
      doc = await engine.createDocument({ width: 512, height: 256 });
      await testing(engine).request("validateFrame");
      assetRed = await image(engine, "rgb(255,0,0)");
      assetBlue = await image(engine, "rgba(0,0,255,.5)");
      await doc.transact("two images", (tx) => {
        lower = tx.addRasterFromAsset(assetRed);
        upper = tx.addRasterFromAsset(assetBlue);
      });
      let tile = await unstable(engine).readTile(0, 0);
      assert(close(pixel(tile), [127, 0, 128, 255]), "normal " + pixel(tile));
      await doc.transact("multiply", (tx) =>
        tx.setProps(upper, { blend: "multiply" }),
      );
      tile = await unstable(engine).readTile(0, 0);
      assert(close(pixel(tile), [127, 0, 0, 255]), "multiply " + pixel(tile));
      await doc.transact("screen", (tx) =>
        tx.setProps(upper, { blend: "screen" }),
      );
      tile = await unstable(engine).readTile(0, 0);
      assert(close(pixel(tile), [255, 0, 128, 255]), "screen " + pixel(tile));
      await doc.transact("opacity", (tx) =>
        tx.setProps(upper, { blend: "normal", opacity: 0.5 }),
      );
      tile = await unstable(engine).readTile(0, 0);
      assert(close(pixel(tile), [191, 0, 64, 255]), "opacity " + pixel(tile));
      await doc.transact("hide", (tx) =>
        tx.setProps(upper, { visible: false }),
      );
      tile = await unstable(engine).readTile(0, 0);
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
      let tile = await unstable(engine).readTile(0, 0);
      assert(close(pixel(tile, 32), [255, 0, 0, 255]), "translation left");
      assert(close(pixel(tile, 96), [127, 0, 128, 255]), "translation right");
      await doc.transact("reorder", (tx) =>
        tx.moveNode(upper, doc.getSnapshot().root, 0),
      );
      tile = await unstable(engine).readTile(0, 0);
      assert(close(pixel(tile, 96), [255, 0, 0, 255]), "reorder");
      let group;
      await doc.transact("isolation", (tx) => {
        group = tx.addGroup({ isolated: true });
        tx.moveNode(lower, group, 0);
        tx.moveNode(upper, group, 1);
        tx.setProps(group, { opacity: 0.5 });
      });
      tile = await unstable(engine).readTile(0, 0);
      assert(
        close(pixel(tile, 32), [128, 0, 0, 128]),
        "isolated group " + pixel(tile, 32),
      );
      await doc.undo();
      await doc.redo();
      assert(
        same(tile, await unstable(engine).readTile(0, 0)),
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
        await testing(engine).request("setVisibleTileJobs", { enabled: false });
        await testing(engine).request("resetRenderCache", { cpu: true });
        const direct = await unstable(engine).readTileLevel(1, 0, 0);
        await testing(engine).request("setVisibleTileJobs", { enabled: true });
        await testing(engine).request("resetRenderCache", { cpu: true });
        engine.viewport.set({ center: [size / 2, size / 2], zoom: 0.5, rotation: 0.17 });
        await settled(engine);
        assert(same(direct, await unstable(engine).readTileLevel(1, 0, 0)),
          `cold visible job differs from direct composite for ${size}px canvas`);
        engine.viewport.set({ center: [size / 2, size / 2], zoom: 1, rotation: 0 });
        // Remove the parent used by the preceding parity probes so this must
        // actually prepare a new parent from the level-0 view.
        await testing(engine).request("resetRenderCache", { cpu: false });
        await testing(engine).request("enablePrefetch");
        let frame = await settled(engine);
        for (let i = 0; i < 20 && frame.prefetch.ready < frame.prefetch.candidates; i++) {
          await delay(10);
          frame = await settled(engine);
        }
        assert(frame.prefetch.candidates === 1 && frame.prefetch.ready === 1,
          `low-zoom parent was not prepared for ${size}px canvas`);
        assert(same(direct, await unstable(engine).readTileLevel(1, 0, 0)),
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
        await testing(engine).request("setVisibleTileJobs", { enabled: false });
        await testing(engine).request("resetRenderCache", { cpu: true });
        const reference = await unstable(engine).readTileLevel(1, 0, 0);
        await testing(engine).request("setVisibleTileJobs", { enabled: true });
        await testing(engine).request("resetRenderCache", { cpu: true });
        engine.viewport.set({ center: [256, 200], zoom: 0.5, rotation: 0.1 });
        await settled(engine);
        assert(same(reference, await unstable(engine).readTileLevel(1, 0, 0)), "cold group pixels differ");
        if (edit) await doc.undo();
      }
      await testing(engine).request("enablePrefetch");
    });
    await test("edits discard only damaged display tiles and match a full recomposite", async () => {
      doc = await engine.createDocument({ width: 768, height: 512 });
      const a = await image(engine, "rgba(200,40,20,.7)", 768, 512);
      const b = await image(engine, "rgba(20,60,220,.5)", 600, 400);
      let bottom, top;
      await doc.transact("layers", tx => {
        bottom = tx.addRasterFromAsset(a);
        top = tx.addRasterFromAsset(b);
      });
      // Prefetch composes level 1 directly, which samples edges differently from the
      // pyramid. The reset stops it, so both reads below build level 1 the same way.
      engine.viewport.set({ center: [384, 256], zoom: 1, rotation: 0 });
      await testing(engine).request("resetRenderCache", { cpu: false });
      const keys = [[0, 0], [1, 0], [2, 0], [0, 1], [1, 1], [2, 1]];
      // Level 0 first, so level 1 is always built from the same children.
      const readAll = async () => {
        const out = [];
        for (const [x, y] of keys) out.push(await unstable(engine).readTile(x, y));
        out.push(await unstable(engine).readTileLevel(1, 0, 0));
        return out;
      };
      // Tiles that survived the edit must equal tiles composed from scratch.
      const check = async (label) => {
        const kept = await readAll();
        await testing(engine).request("resetRenderCache", { cpu: false });
        const fresh = await readAll();
        kept.forEach((t, i) => assert(same(t, fresh[i]), `${label}: display tile ${i} is stale`));
      };
      await readAll();
      await stroke(doc, top, "brush", [{ x: 300, y: 100, pressure: 1 }, { x: 340, y: 120, pressure: 1 }]);
      await check("brush");
      await doc.transact("move", tx => tx.setTransform(top, { x: 90, y: 40, scaleX: 1, scaleY: 1, rotation: 0 }));
      await check("move");
      await doc.transact("rotate", tx => tx.setTransform(top, { x: 90, y: 40, scaleX: 0.8, scaleY: 1.2, rotation: 0.3 }));
      await check("rotate");
      // Mid-stroke tiles show the stroke preview; each input must discard what it covers.
      const live = doc.tools.begin("brush", {
        layer: top,
        tip: { size: 48, hardness: 0.8, spacing: 0.1 },
        color: [0.2, 0.8, 0.3, 1],
      });
      live.input({ x: 400, y: 300, pressure: 1 });
      await live.flush();
      await readAll();
      live.input({ x: 520, y: 330, pressure: 1 });
      live.input({ x: 560, y: 200, pressure: 1 });
      await live.flush();
      await check("live stroke on a transformed layer");
      await live.end();
      await check("brush on a transformed layer");
      await doc.transact("opacity", tx => tx.setProps(top, { opacity: 0.4 }));
      await check("opacity");
      await doc.transact("hide", tx => tx.setProps(bottom, { visible: false }));
      await check("hide");
      await doc.transact("reorder", tx => tx.moveNode(top, doc.getSnapshot().root, 0));
      await check("reorder");
      for (let i = 0; i < 7; i++) {
        await doc.undo();
        await check("undo " + i);
      }
      await testing(engine).request("enablePrefetch");
    });
    await test("cold frames decode inputs in the decode workers and match inline decoding", async () => {
      let cold;
      ({ doc, cold } = await decodeFixture(engine));
      const inline = await cold(false);
      const pooled = await cold(true);
      // A browser without nested Workers decodes inline; nothing more to compare.
      if (pooled) {
        assert(pooled.frame.workerDecodes > 0, "no input was decoded in a decode worker");
        assert(pooled.frame.decodeWorkerBytes > 0, "decode worker memory was not reported");
        pooled.tiles.forEach((t, i) => assert(same(t, inline.tiles[i]), `tile ${i} differs from inline decoding`));
      }
      await testing(engine).request("enablePrefetch");
    });
    await test("a decode worker that stops answering hands its tiles back to inline decoding", async () => {
      let cold;
      ({ doc, cold } = await decodeFixture(engine));
      const inline = await cold(false);
      if (await testing(engine).request("setDecodeWorkers", { enabled: true })) {
        const stopped = new Promise((resolve) => {
          const off = unstable(engine).onDiagnostic((v) => {
            if (String(v?.message).includes("did not answer")) {
              off();
              resolve(v.message);
            }
          });
        });
        assert((await testing(engine).request("stallDecodeWorkers")) > 0, "no decode worker to stall");
        const started = performance.now();
        const stalled = await cold(true, { keepPool: true });
        const elapsed = performance.now() - started;
        const message = await stopped;
        assert(/batch \d+ within 2000 ms/.test(message), "timeout did not name the batch: " + message);
        assert(elapsed < 6000, `inline decoding took over only after ${elapsed.toFixed(0)} ms`);
        stalled.tiles.forEach((t, i) => assert(same(t, inline.tiles[i]), `tile ${i} differs after the handover`));
        // A new pool starts on request and is used once a Worker is ready.
        let on = false;
        for (let i = 0; i < 100 && !on; i++) {
          on = await testing(engine).request("setDecodeWorkers", { enabled: true });
          if (!on) await delay(20);
        }
        assert(on, "the decode pool did not restart");
      }
      await testing(engine).request("enablePrefetch");
    });
    await test("full display cache can yield and replace tiles without orphaning slots", async () => {
      doc = await engine.createDocument({ width: 300, height: 300 });
      const asset = await image(engine, "rgba(40,120,210,.6)", 300, 300);
      await doc.transact("layer", tx => tx.addRasterFromAsset(asset));
      await testing(engine).request("resetRenderCache", { cpu: true });
      for (let x = 1000; x < 1256; x++) await unstable(engine).readTile(x, 0);
      engine.viewport.set({ center: [150, 150], zoom: 0.5, rotation: 0 });
      await settled(engine);
      const tile = await unstable(engine).readTileLevel(1, 0, 0);
      assert(pixel(tile)[3] > 0, "full cache lost the visible tile");
      await testing(engine).request("enablePrefetch");
    });
    let brushLayer, painted;
    await test("GPU brush / pressure / tile boundary / exact Undo-Redo / eraser", async () => {
      doc = await engine.createDocument({ width: 512, height: 256 });
      await doc.transact("layer", (tx) => (brushLayer = tx.addRaster()));
      const empty = await unstable(engine).readTile(0, 0);
      await stroke(doc, brushLayer);
      painted = await unstable(engine).readTile(0, 0);
      const reference = new Uint8Array(
        await testing(engine).request("cpuReference", {
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
      const right = await unstable(engine).readTile(1, 0);
      assert(pixel(right, 12, 128)[3] > 0, "tile boundary missing");
      await doc.undo();
      assert(
        same(empty, await unstable(engine).readTile(0, 0)),
        "undo changed pixels",
      );
      await doc.redo();
      assert(
        same(painted, await unstable(engine).readTile(0, 0)),
        "redo changed pixels",
      );
      await stroke(doc, brushLayer, "eraser", [
        { x: 245, y: 128, pressure: 1 },
      ]);
      const erased = await unstable(engine).readTile(0, 0);
      assert(pixel(erased, 245, 128)[3] === 0, "eraser did not clear");
      await doc.undo();
    });
    await test(".alto ZIP save/open preserves pixels and snapshot hash", async () => {
      const hash = (await doc.snapshotHash());
      const bytes = await testing(engine).saveBytes();
      doc = await engine.open(new Blob([bytes]));
      assert((await doc.snapshotHash()) === hash, "snapshot hash changed");
      assert(
        same(painted, await unstable(engine).readTile(0, 0)),
        "saved pixels changed",
      );
    });
    await test("atomic save survives write failure and writer Worker termination", async () => {
      await doc.transact("unsaved marker", (tx) =>
        tx.setProps(brushLayer, { name: "Unsaved paint" }),
      );
      const before = (await doc.snapshotHash());
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
          write: async (stream) => {
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
        (await doc.snapshotHash()) === before && doc.getSnapshot().dirty,
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
      const hash = (await doc.snapshotHash());
      const undo = doc.getSnapshot().history.undo;
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
          (await doc.snapshotHash()) === hash &&
          doc.getSnapshot().history.undo === undo,
        "atomic validation failed",
      );
    });
    await test("device loss rebuilds GPU from CPU content", async () => {
      await gpuRestored(engine, () => testing(engine).destroyGpu());
      assert(
        same(painted, await unstable(engine).readTile(0, 0)),
        "device loss pixels changed",
      );
    });
    await test("Worker restart replays flushed OPFS journal", async () => {
      await doc.transact("recover marker", (tx) =>
        tx.setProps(brushLayer, { name: "Recovered paint" }),
      );
      const hash = (await doc.snapshotHash());
      // Edits resolve when applied; a crash test must wait for the journal.
      await doc.durable();
      await testing(engine).crash();
      doc = engine.document;
      assert((await doc.snapshotHash()) === hash, "journal recovery hash mismatch");
      assert(
        same(painted, await unstable(engine).readTile(0, 0)),
        "journal recovery pixels mismatch",
      );
    });
    await test("a checkpoint writes only the objects the recovery store lacks", async () => {
      await doc.durable();
      const start = await testing(engine).recoveryStore();
      // Tile (1,0): later tests compare tile (0,0) with the pixels painted earlier.
      await stroke(doc, brushLayer, "brush", [{ x: 400, y: 40, pressure: 1 }]);
      await doc.versions.commit("one more tile");
      await doc.durable();
      const after = await testing(engine).recoveryStore();
      assert(after.latestGeneration === start.latestGeneration + 1, "the commit did not checkpoint");
      assert(after.records === 0, "the checkpoint kept journal records");
      const added = after.objects - start.objects;
      // The stroke touches one tile; the earlier tile stays as history and is already stored.
      assert(added === 1, `the checkpoint wrote ${added} objects`);
      await doc.undo();
    });
    await test("a long journal is checkpointed once editing pauses", async () => {
      const name = doc.getSnapshot().nodes.get(brushLayer).name;
      const start = await testing(engine).recoveryStore();
      // 500 journal records start an automatic checkpoint after 2 s without messages.
      for (let i = 0; i < 520; i++)
        await doc.transact("rename " + i, (tx) => tx.setProps(brushLayer, { name: "Long " + i }));
      await doc.durable();
      let store = await testing(engine).recoveryStore();
      assert(store.latestGeneration === start.latestGeneration, "checkpointed while editing");
      assert(store.records >= 500, `only ${store.records} records were journaled`);
      // A request counts as activity, so wait without polling.
      await delay(3000);
      store = await testing(engine).recoveryStore();
      assert(store.latestGeneration === start.latestGeneration + 1, "no automatic checkpoint");
      assert(store.records === 0, "records remained after the checkpoint");
      await doc.transact("restore name", (tx) => tx.setProps(brushLayer, { name }));
    });
    await test("rapid edits keep journal order and survive a crash", async () => {
      const before = doc.getSnapshot().revision;
      const name = doc.getSnapshot().nodes.get(brushLayer).name;
      // Edits no longer wait for their journal write, so they can run ahead of it.
      const edits = [];
      for (let i = 0; i < 300; i++)
        edits.push(doc.transact("rename " + i, (tx) => tx.setProps(brushLayer, { name: "Rename " + i })));
      await Promise.all(edits);
      assert(doc.getSnapshot().revision === before + 300, "an edit was lost or merged");
      await doc.durable();
      const hash = await doc.snapshotHash();
      await testing(engine).crash();
      doc = engine.document;
      assert(doc.getSnapshot().nodes.get(brushLayer).name === "Rename 299", "journal order changed");
      assert((await doc.snapshotHash()) === hash, "recovered document differs");
      await doc.transact("restore name", (tx) => tx.setProps(brushLayer, { name }));
    });
    await test("recovery keeps undo history from before the checkpoint", async () => {
      const name = doc.getSnapshot().nodes.get(brushLayer).name;
      for (const label of ["H1", "H2"])
        await doc.transact(label, (tx) => tx.setProps(brushLayer, { name: label }));
      await doc.versions.commit("history checkpoint");
      for (const label of ["H3", "H4"])
        await doc.transact(label, (tx) => tx.setProps(brushLayer, { name: label }));
      await doc.undo();
      const walk = async () => {
        const out = [];
        const redo = doc.getSnapshot().history.redo;
        while (doc.getSnapshot().history.undo) {
          await doc.undo();
          out.push(await doc.snapshotHash());
        }
        while (doc.getSnapshot().history.redo) {
          await doc.redo();
          out.push(await doc.snapshotHash());
        }
        // Back to where the walk started, with the same stacks.
        for (let i = 0; i < redo; i++) await doc.undo();
        return out;
      };
      const history = { ...doc.getSnapshot().history };
      const expected = await walk();
      // The walk's own undos and redos are edits too and change the intent log.
      const pending = doc.getSnapshot().versions.pending_intents;
      await doc.durable();
      await testing(engine).crash();
      doc = engine.document;
      assert(doc.getSnapshot().history.undo === history.undo && doc.getSnapshot().history.redo === history.redo,
        `history ${JSON.stringify(doc.getSnapshot().history)} after recovery, ${JSON.stringify(history)} before`);
      assert(doc.getSnapshot().versions.pending_intents === pending, "commit intents were lost");
      const actual = await walk();
      assert(actual.length === expected.length && actual.every((h, i) => h === expected[i]),
        "undo/redo after recovery reached different states");
      await doc.transact("restore name", (tx) => tx.setProps(brushLayer, { name }));
    });
    await test("simultaneous Worker restart requests share one recovery", async () => {
      const hash = (await doc.snapshotHash());
      const canvas = engine.element;
      const container = canvas.parentElement;
      let nested;
      const off = engine.on("status", (s) => { if (s.state === "restarting") nested = testing(engine).crash(); });
      try {
        const first = testing(engine).crash();
        const second = testing(engine).crash();
        assert(first === second, "restart requests did not share a promise");
        assert(first === nested, "reentrant restart did not share a promise");
        await first;
      } finally { off(); }
      assert(engine.document === doc, "the document handle changed across a restart");
      assert(engine.element !== canvas && !canvas.isConnected, "the Worker canvas was not replaced");
      assert(container.querySelectorAll("canvas").length === 1, "Worker canvas was replaced more than once");
      assert(engine.element.parentElement === container, "the host container was replaced");
      assert((await doc.snapshotHash()) === hash, "concurrent restart changed the document");
    });
    await test("uncloneable request does not leave a pending reply", async () => {
      const before = testing(engine).pendingRequests();
      let rejected = false;
      try { await testing(engine).request("view", { invalid: () => true }); }
      catch { rejected = true; }
      assert(rejected, "uncloneable request was accepted");
      assert(testing(engine).pendingRequests() === before, "failed postMessage leaked a pending reply");
      await testing(engine).request("view");
    });
    await test("missing Worker script stops once and retries without corrupting the journal", async () => {
      const hash = (await doc.snapshotHash());
      testing(engine).setWorkerURL("/missing-alto-engine-worker.js");
      let code = null;
      try { await testing(engine).crash(); }
      catch (error) { code = error.code; }
      finally { testing(engine).setWorkerURL(null); }
      assert(code === "WORKER_UNAVAILABLE", "missing script was not classified: " + code);
      assert(engine.status.state === "failed" && engine.status.error, "Worker failure did not stop the engine");
      assert(engine.status.error.code !== "CORRUPT_DATA", "Worker failure was mislabeled as saved-data corruption");
      let refused = false;
      try { await doc.transact("app:during-failure", (tx) => tx.setProps(brushLayer, { name: "x" })); }
      catch (error) { refused = error.code === "WORKER_UNAVAILABLE"; }
      assert(refused, "the document accepted an edit while the Worker was down");
      assert(!document.querySelector("#engine-failure").hidden, "retry control was not shown");
      let blocked = false;
      try { await testing(engine).request("save"); }
      catch (error) { blocked = error.code === "WORKER_UNAVAILABLE"; }
      assert(blocked, "editing request continued after Worker failure");
      const handle = doc;
      await event(engine, "status", () => document.querySelector("#retry-worker").click(), ready);
      doc = engine.document;
      assert(doc === handle, "retry replaced the document handle");
      assert(engine.status.state === "ready" && !engine.status.error, "retry did not restore the engine");
      assert((await doc?.snapshotHash()) === hash, "retry opened the wrong document");
      assert(same(painted, await unstable(engine).readTile(0, 0)), "retry changed saved pixels");
      assert(document.querySelector("#engine-failure").hidden, "failure controls remained open");
    });
    await test("truncated journal tail is excluded during recovery", async () => {
      const hash = (await doc.snapshotHash());
      await testing(engine).corruptJournalTail();
      await testing(engine).crash();
      doc = engine.document;
      assert(
        (await doc.snapshotHash()) === hash,
        "truncated journal lost valid edits",
      );
      assert(
        doc.getSnapshot().warnings.some((w) => w.code === "RECOVERED_TRUNCATED"),
        "truncation warning missing",
      );
    });
    await test("damaged latest checkpoint falls back to the previous generation", async () => {
      const before = await testing(engine).saveBytes();
      const previousHash = (await doc.snapshotHash());
      doc = await engine.open(new Blob([before]));
      doc = await engine.createDocument({ width: 64, height: 64 });
      await testing(engine).corruptCheckpoint();
      await testing(engine).crash();
      doc = engine.document;
      assert((await doc.snapshotHash()) === previousHash, "previous checkpoint was not opened");
      assert(doc.getSnapshot().warnings.some((w) => w.code === "RECOVERED_OLDER_GENERATION"), "fallback warning missing");
      assert(same(painted, await unstable(engine).readTile(0, 0)), "fallback pixels changed");
    });
    await test("checkpoint CRC failure falls back without losing the older journal", async () => {
      const before = await testing(engine).saveBytes();
      const previousHash = (await doc.snapshotHash());
      doc = await engine.open(new Blob([before]));
      doc = await engine.createDocument({ width: 64, height: 64 });
      await testing(engine).corruptCheckpointCrc();
      await testing(engine).crash();
      doc = engine.document;
      assert((await doc.snapshotHash()) === previousHash, "CRC fallback opened the wrong generation");
      assert(doc.getSnapshot().warnings.some((w) => w.code === "RECOVERED_OLDER_GENERATION"), "CRC fallback warning missing");
    });
    await test("V1 checkpoint opens and the next checkpoint migrates to V2", async () => {
      const hash = (await doc.snapshotHash());
      await testing(engine).legacyCheckpoint();
      await testing(engine).crash();
      doc = engine.document;
      assert((await doc.snapshotHash()) === hash, "V1 checkpoint did not open");
      const bytes = await testing(engine).saveBytes();
      doc = await engine.open(new Blob([bytes]));
      await doc.durable();
      await testing(engine).crash();
      doc = engine.document;
      assert((await doc.snapshotHash()) === hash, "V2 migration changed the document");
      assert(same(painted, await unstable(engine).readTile(0, 0)), "migration pixels changed");
    });
    await test("a checkpoint of one file per object opens and the next one packs them", async () => {
      const hash = await doc.snapshotHash();
      await testing(engine).looseCheckpoint();
      await testing(engine).crash();
      doc = engine.document;
      assert((await doc.snapshotHash()) === hash, "format 3 checkpoint did not open");
      assert(same(painted, await unstable(engine).readTile(0, 0)), "format 3 pixels changed");
      const start = await testing(engine).recoveryStore();
      assert(start.packs === 0, "the format 3 checkpoint named packs");
      const name = doc.getSnapshot().nodes.get(brushLayer).name;
      await doc.transact("rename", (tx) => tx.setProps(brushLayer, { name: "Packed" }));
      await doc.versions.commit("pack the objects");
      await doc.durable();
      const after = await testing(engine).recoveryStore();
      assert(after.latestGeneration === start.latestGeneration + 1, "the commit did not checkpoint");
      assert(after.packs === 1, `the objects went into ${after.packs} packs`);
      const packed = await doc.snapshotHash();
      await testing(engine).crash();
      doc = engine.document;
      assert((await doc.snapshotHash()) === packed, "the packed checkpoint changed the document");
      assert(same(painted, await unstable(engine).readTile(0, 0)), "packed pixels changed");
      await doc.transact("restore name", (tx) => tx.setProps(brushLayer, { name }));
    });
    await test("checkpoints that each add objects keep the packs few", async () => {
      // Each stroke changes tile (1,0) again, so each commit stores one new object.
      for (let i = 0; i < 12; i++) {
        await stroke(doc, brushLayer, "brush", [{ x: 300 + 12 * i, y: 200, pressure: 1 }]);
        await doc.versions.commit("pack " + i);
      }
      await doc.durable();
      const store = await testing(engine).recoveryStore();
      // One pack for the first checkpoint, then sizes doubling with age: about log2(12) more.
      assert(store.packs <= 5, `${store.packs} packs after 12 checkpoints`);
      const hash = await doc.snapshotHash();
      await testing(engine).crash();
      doc = engine.document;
      assert((await doc.snapshotHash()) === hash, "recovery from packs changed the document");
      for (let i = 0; i < 12; i++) await doc.undo();
      assert(same(painted, await unstable(engine).readTile(0, 0)), "recovered pixels changed");
    });
    await test("failed append freezes edits and preserves live emergency export", async () => {
      const before = doc.getSnapshot().revision;
      const name = doc.getSnapshot().nodes.get(brushLayer).name;
      const baseline = await testing(engine).saveBytes();
      await testing(engine).injectStorageFault("beforeAppend");
      // An edit is applied when transact resolves; durable() reports the journal.
      await doc.transact("unconfirmed edit", (tx) =>
        tx.setProps(brushLayer, { name: "Unconfirmed paint" }),
      );
      assert(doc.getSnapshot().revision === before + 1, "edit was not applied in memory");
      let failed = false;
      try {
        await doc.durable();
      } catch (error) {
        failed = error.code === "STORAGE";
      }
      assert(failed && engine.status.state === "storage-failed", "append failure was not surfaced");
      assert(!document.querySelector("#storage-failure").hidden, "recovery controls were not shown");
      assert(document.querySelector("main").inert, "editor was not disabled");
      const emergency = await engine.recovery.exportEmergency();
      assert(emergency.size > 100, "emergency export failed");
      let blocked = false;
      try { await doc.undo(); } catch (error) { blocked = error.code === "STORAGE"; }
      assert(blocked, "editing continued after storage failure");
      await event(engine, "status", () => document.querySelector("#recover-storage").click(), ready);
      await delay(0);
      doc = engine.document;
      assert(document.querySelector("#storage-failure").hidden, "recovery controls remained open");
      assert(!document.querySelector("main").inert, "editor remained disabled");
      assert(!document.querySelector("#recovery-result").hidden, "recovery result was not shown");
      assert(doc.getSnapshot().revision === before, "confirmed journal changed unexpectedly");
      assert(doc.getSnapshot().nodes.get(brushLayer).name === name, "unconfirmed edit was replayed");
      doc = await engine.open(emergency);
      assert(doc.getSnapshot().nodes.get(brushLayer).name === "Unconfirmed paint", "emergency export lost live edit");
      doc = await engine.open(new Blob([baseline]));
    });
    await test("lost append reply replays once and matches the display", async () => {
      const before = doc.getSnapshot().revision;
      await testing(engine).injectStorageFault("afterAppend");
      await doc.transact("reply lost", (tx) =>
        tx.setProps(brushLayer, { name: "Confirmed despite lost reply" }),
      );
      let failed = false;
      try {
        await doc.durable();
      } catch (error) {
        failed = error.code === "STORAGE";
      }
      assert(failed && engine.status.state === "storage-failed", "lost reply was not surfaced");
      const recovered = await engine.recovery.recoverAfterStorageFailure();
      doc = recovered.document;
      // The record reached OPFS, so the journal agrees with what was on screen.
      assert(recovered.matchesDisplay === true, "recovery differs from the display");
      assert(doc.getSnapshot().revision === before + 1, "lost reply replayed twice or not at all");
      assert(doc.getSnapshot().nodes.get(brushLayer).name === "Confirmed despite lost reply", "journal edit missing");
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
      const p = pixel(await unstable(engine).readTile(0, 0), 16, 16);
      assert(p[1] > 240 && p[3] === 255, "JPEG/PNG color " + p);
    });
    await test("second engine cannot overwrite the active recovery journal", async () => {
      const { createEngine } = await import("../../packages/alto-sdk/dist/index.js");
      const hash = (await doc.snapshotHash());
      let rejected = false;
      try {
        await createEngine({ container: document.createElement("div") });
      } catch (error) {
        rejected = error.code === "CONFLICT" && error.details?.reason === "namespace-in-use";
      }
      assert(
        rejected && (await doc.snapshotHash()) === hash,
        "second engine acquired the journal",
      );
    });
    await test("two damaged journals freeze editing until an external file opens", async () => {
      const backup = await testing(engine).saveBytes();
      const hash = (await doc.snapshotHash());
      await testing(engine).corruptBothCheckpoints();
      let rejected = false;
      try { await testing(engine).crash(); }
      catch (error) { rejected = error.code === "CORRUPT_DATA"; }
      assert(rejected && engine.status.error?.code === "CORRUPT_DATA", "damage was hidden");
      assert(engine.document === null, "stale document remained editable");
      assert(!document.querySelector("#storage-failure").hidden, "damage controls were not shown");
      assert(!document.querySelector("#open-recovery-file").hidden, "external recovery was not offered");
      doc = await engine.open(new Blob([backup]));
      assert((await doc.snapshotHash()) === hash, "external recovery changed the document");
      assert(engine.status.state === "ready", "editing stayed frozen after external recovery");
      assert(document.querySelector("#storage-failure").hidden, "damage controls stayed open");
    });
    log("ALL " + results.length + " TESTS PASSED");
  } catch (e) {
    results.push({ name: activeTest ?? "suite", pass: false, error: String(e) });
    log("FAIL " + e);
    throw e;
  } finally {
    const canvas = engine.element.getBoundingClientRect();
    document.querySelector("#test-results").dataset.report = JSON.stringify({
      date: new Date().toISOString(),
      userAgent: navigator.userAgent,
      crossOriginIsolated,
      viewport: {
        width: innerWidth,
        height: innerHeight,
        dpr: devicePixelRatio,
        canvasCss: [canvas.width, canvas.height],
        canvasPixels: [engine.element.width, engine.element.height],
        benchmarkView: new URLSearchParams(location.search).get("benchmarkView") || "physical",
      },
      results,
    });
  }
}
async function benches(engine, log, results) {
  const profiling = new URLSearchParams(location.search).has("profile");
  const sameView = new URLSearchParams(location.search).get("benchmarkView") === "css";
  // Zoom is in CSS pixels. "physical" (the default) maps one document pixel to one
  // device pixel at zoom 1, as before the CSS unit existed.
  const ratio = unstable(engine).pixelRatio();
  const zoomScale = sameView ? 1 : 1 / ratio;
  if (profiling) await testing(engine).request("setProfiling", { enabled: true });
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
    await testing(engine).request("setVisibleTileJobs", { enabled: jobs });
    for (const cpu of [false, true]) {
      await testing(engine).request("resetRenderCache", { cpu });
      engine.viewport.set({ center: [1920, 1080], zoom: 0.5 * zoomScale, rotation: 0 });
      const began = performance.now();
      const coldFrame = await settled(engine);
      results.push({ id: (cpu ? "C3 CPU+GPU cold ms" : "C2 GPU cold ms") + (jobs ? "" : " (synchronous reference)"),
        value: performance.now() - began, pass: true, profile: coldFrame.profile,
        canvasPixels: [engine.element.width, engine.element.height],
        physicalZoom: engine.viewport.get().zoom * ratio });
      log("COLD " + JSON.stringify(results.at(-1)));
    }
  }
  await testing(engine).request("resetRenderCache", { cpu: false });
  await testing(engine).request("enablePrefetch");
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
  const canvasCss = engine.element.getBoundingClientRect();
  results.at(-1).canvasCss = [canvasCss.width, canvasCss.height];
  results.at(-1).canvasPixels = [engine.element.width, engine.element.height];
  results.at(-1).physicalZoom = engine.viewport.get().zoom * ratio;
  results.at(-1).cssZoom = engine.viewport.get().zoom;
  results.at(-1).visibleDocument = {
    width: canvasCss.width / engine.viewport.get().zoom,
    height: canvasCss.height / engine.viewport.get().zoom,
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
  const bytes = await testing(engine).saveBytes();
  const save = performance.now() - start;
  start = performance.now();
  doc = await engine.open(new Blob([bytes]));
  const open = performance.now() - start;
  record(
    "B5 save/open ms",
    { save, open },
    "3000 / 2000",
    save <= 3000 && open <= 2000,
  );
  frame = await settled(engine);
  memory.push(frame);
  // wasmBytes is the engine runtime. Builds with a decode pool also report its
  // Workers' WASM memory and the batches in transit; older builds report neither.
  // The pool's peak is tracked in the Worker for its whole life, cold frames included.
  // Adding the two peaks bounds the total from above: they need not coincide.
  const peakGpu = Math.max(...memory.map((f) => f.gpuBytes)),
    peakWasm = Math.max(...memory.map((f) => f.wasmBytes)),
    peakPool = Math.max(...memory.map((f) => f.decodePeakBytes || 0));
  record(
    "B6 sampled peak memory MiB",
    {
      gpu: peakGpu / 1048576,
      wasm: peakWasm / 1048576,
      decodePool: peakPool / 1048576,
      totalBound: (peakWasm + peakPool) / 1048576,
    },
    "512 / 1024 (WASM, decode pool included)",
    peakGpu <= 512 * 1048576 && peakWasm + peakPool <= 1024 * 1048576,
  );
  const bootHash = await doc.snapshotHash();
  // Older SDKs had no durable(); their open already waited for the checkpoint.
  await doc.durable?.();
  const bootStart = performance.now();
  await testing(engine).crash();
  await settled(engine);
  const boot = performance.now() - bootStart;
  assert((await engine.document.snapshotHash()) === bootHash, "restart lost saved document");
  record("B7 cached Worker restart + first draw ms", boot, 1500, boot <= 1500);
  // B8: what one small edit costs to show. Before incremental invalidation every
  // commit discarded the whole display cache. Public SDK calls only, so the same
  // code measures older builds.
  doc = engine.document;
  // Zoom 1 shows about a dozen display tiles; zoom 0.5 shows the whole canvas, where
  // a full invalidation costs a cold frame.
  for (const [label, zoom] of [["zoom 1", 1], ["whole canvas", 0.5]]) {
    engine.viewport.set({ center: [1920, 1080], zoom: zoom * zoomScale, rotation: 0 });
    await settled(engine);
    const redraws = [];
    for (let i = 0; i < 10; i++) {
      const s = doc.tools.begin("brush", {
        layer: layers.at(-1),
        tip: { size: 24 },
        color: [0, 1, 0, 1],
      });
      s.input({ x: 1700 + i * 40, y: 1000, pressure: 1 });
      await s.flush();
      await settled(engine);
      start = performance.now();
      await s.end();
      await settled(engine);
      redraws.push(performance.now() - start);
    }
    redraws.sort((a, b) => a - b);
    const redraw = redraws[Math.floor(redraws.length / 2)];
    record(`B8 small stroke commit-to-redrawn median ms (${label})`, redraw, "2 frames (~33.4 ms)", redraw <= 33.4);
  }
  log("BENCHMARK COMPLETE");
}
