import { gpuRestored } from "./gpu-recovery.js";
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
const ready = (s) => s.state === "ready";
async function settled(engine) {
  return unstable(engine).settle();
}
/**
 * Twelve layers whose 256 px tiles all differ (content addressing cannot merge them),
 * seen at zoom 0.5. A level-1 tile is the downsample of its level-0 tiles, and each of
 * those needs 12 inputs, more than a job decodes inline (`INLINE_DECODE_MAX`, 8).
 * cold(workers) renders from empty caches with or without the decode pool.
 */
async function decodeFixture(engine) {
  const doc = await engine.createDocument({ width: 1024, height: 512 });
  const assets = [];
  for (let i = 0; i < 12; i++) {
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
  const view = { center: [512, 256], zoom: 0.5 / unstable(engine).pixelRatio(), rotation: 0 };
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
export async function runTests(engine, benchmark, log, { decodeFaultInjection = false } = {}) {
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
    await test("wasm decode rejects truncated batches and 32-bit length overflow", async () => {
      const { default: init, decodeBatch } = await import("../../packages/alto-sdk/dist/pkg/alto_decode.js");
      await init();
      const overflow = new Uint8Array(4 + 38 + 38);
      const view = new DataView(overflow.buffer);
      view.setUint32(0, 2, true);
      view.setUint32(38, 0xffffffff, true);
      assert(decodeBatch(overflow).length === 0, "overflowed batch was not rejected");
      const truncated = new Uint8Array(4 + 38);
      new DataView(truncated.buffer).setUint32(0, 2, true);
      assert(decodeBatch(truncated).length === 0, "truncated batch was not rejected");
      const empty = decodeBatch(new Uint8Array(4));
      assert(empty.length === 4 && new DataView(empty.buffer, empty.byteOffset, 4).getUint32(0, true) === 0,
        "empty valid batch was rejected");
      const worker = new Worker(new URL("../../packages/alto-sdk/dist/decode-worker.js", import.meta.url), { type: "module" });
      try {
        const reply = await new Promise((resolve, reject) => {
          const timer = setTimeout(() => reject(Error("decode worker timeout")), 15000);
          worker.onerror = (event) => { clearTimeout(timer); reject(Error(event.message)); };
          worker.onmessage = ({ data }) => {
            if (data.ready) worker.postMessage({ id: 7, batch: overflow.buffer });
            else { clearTimeout(timer); resolve(data); }
          };
        });
        assert(reply.id === 7 && reply.error === "invalid decode request frame", "worker did not report malformed batch");
      } finally {
        worker.terminate();
      }
    });
    let doc, lower, upper, assetRed, assetBlue;
    await test("queue-ordered reuse and destroy preserve earlier copies without a CPU fence", async () => {
      const adapter=await navigator.gpu.requestAdapter(), device=await adapter.requestDevice();
      device.pushErrorScope("validation");
      const texture=device.createTexture({size:[1,1],format:"rgba8unorm",usage:GPUTextureUsage.COPY_SRC|GPUTextureUsage.COPY_DST|GPUTextureUsage.RENDER_ATTACHMENT});
      const reads=Array.from({length:4},()=>device.createBuffer({size:256,usage:GPUBufferUsage.COPY_DST|GPUBufferUsage.MAP_READ}));
      const write=color=>device.queue.writeTexture({texture},new Uint8Array(color),{bytesPerRow:4},[1,1]);
      const copy=(encoder,buf)=>encoder.copyTextureToBuffer({texture},{buffer:buf,bytesPerRow:256},[1,1]);
      write([255,0,0,255]); let encoder=device.createCommandEncoder(); copy(encoder,reads[0]); device.queue.submit([encoder.finish()]);
      let completed=false; const done=device.queue.onSubmittedWorkDone().then(()=>completed=true);
      write([0,255,0,255]); encoder=device.createCommandEncoder(); copy(encoder,reads[1]); device.queue.submit([encoder.finish()]);
      assert(!completed,"test yielded before queue reuse");
      encoder=device.createCommandEncoder(); copy(encoder,reads[2]);
      encoder.beginRenderPass({colorAttachments:[{view:texture.createView(),loadOp:"clear",storeOp:"store",clearValue:[0,0,1,1]}]}).end();
      copy(encoder,reads[3]); device.queue.submit([encoder.finish()]); texture.destroy();
      await Promise.all(reads.map(b=>b.mapAsync(GPUMapMode.READ)));
      const expected=[[255,0,0,255],[0,255,0,255],[0,255,0,255],[0,0,255,255]];
      reads.forEach((b,i)=>{assert(Array.from(new Uint8Array(b.getMappedRange()).slice(0,4)).join()===expected[i].join(),`copy ${i} changed after reuse`);b.unmap();b.destroy();});
      await done; const error=await device.popErrorScope(); device.destroy(); assert(!error, String(error?.message));
    });
    await test("GPU memory categories sum without counting occupied slots twice", async () => {
      await engine.createDocument({width:64,height:64});
      const f=await settled(engine), g=f.gpu;
      assert(g && f.gpuBytes === g.fixed+g.effects+g.stroke+g.vector+g.readback,"GPU category total");
      assert(g.liveFixed+g.retiredFixed <= g.fixed,"fixed subsets exceed reservation");
      assert(g.fixed-g.retiredFixed===(g.atlasLayers+g.displayLayers)*262144+1048576,`default cache reservation changed: ${JSON.stringify(g)}`);
    });
    await test("off-canvas brush tiles survive movement, history, save and GPU recovery", async () => {
      doc=await engine.createDocument({width:64,height:64});
      let layer;
      await doc.transact("layer",tx=>{layer=tx.addRaster();});
      const stroke=doc.tools.begin("brush",{layer,tip:{size:32,hardness:1,spacing:0.15},color:[1,0,0,1]});
      for(let x=32;x>=-384;x-=32) stroke.input({x,y:32,pressure:1});
      await stroke.end();
      const canonical=JSON.stringify(await doc.nodeTiles(layer));
      assert(Boolean(JSON.parse(canonical).tiles["-2,0"]),"wholly outside canonical tile was lost");
      await doc.transact("move",tx=>tx.setTransform(layer,{x:416,y:0,scaleX:1,scaleY:1,rotation:0}));
      const red=async()=>{
        const t=await unstable(engine).readTile(0,0), i=(32*256+32)*4;
        assert(Array.from(t.slice(i,i+4)).join() === "255,0,0,255","moved outside pixel");
        assert(JSON.stringify(await doc.nodeTiles(layer))===canonical,"canonical tile hashes changed");
      };
      await red(); await doc.undo(); await doc.redo(); await red();
      const bytes=await testing(engine).saveBytes();
      doc=await engine.open(new Blob([bytes])); await red();
      await gpuRestored(engine); await red();
    });
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
      const ratio = unstable(engine).pixelRatio();
      for (const size of [512, 300]) {
        doc = await engine.createDocument({ width: size, height: size });
        const red = await image(engine, "rgba(255,0,0,.5)", size, size);
        const blue = await image(engine, "rgba(0,0,255,.5)", size, size);
        await doc.transact("two translucent layers", (tx) => {
          tx.addRasterFromAsset(red);
          tx.addRasterFromAsset(blue);
        });
        engine.viewport.set({ center: [size / 2, size / 2], zoom: 0.5 / ratio, rotation: 0 });
        await testing(engine).request("setVisibleTileJobs", { enabled: false });
        await testing(engine).request("resetRenderCache", { cpu: true });
        const direct = await unstable(engine).readTileLevel(1, 0, 0);
        await testing(engine).request("setVisibleTileJobs", { enabled: true });
        await testing(engine).request("resetRenderCache", { cpu: true });
        engine.viewport.set({ center: [size / 2, size / 2], zoom: 0.5 / ratio, rotation: 0.17 });
        await settled(engine);
        assert(same(direct, await unstable(engine).readTileLevel(1, 0, 0)),
          `cold visible job differs from direct composite for ${size}px canvas`);
        engine.viewport.set({ center: [size / 2, size / 2], zoom: 1 / ratio, rotation: 0 });
        // Remove the parent used by the preceding parity probes so this must
        // actually prepare a new parent from the level-0 view.
        await testing(engine).request("resetRenderCache", { cpu: false });
        await testing(engine).request("enablePrefetch");
        let frame = await settled(engine);
        const deadline = performance.now() + 5000;
        while (frame.prefetch.ready !== 1 && performance.now() < deadline) {
          await delay(125);
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
    if (decodeFaultInjection) {
      await test("malformed decode requests stop the pool and retry every tile inline", async () => {
        const toggle = async (enabled) => {
          const result = await fetch(`/__alto-test/decode-fault?enabled=${enabled ? 1 : 0}`, { cache: "no-store" });
          assert(result.ok, "decode fault injection unavailable");
        };
        const startPool = async () => {
          for (let i = 0; i < 100; i++) {
            if (await testing(engine).request("setDecodeWorkers", { enabled: true })) return;
            await delay(20);
          }
          throw Error("decode worker pool did not start");
        };
        await startPool();
        let cold;
        ({ doc, cold } = await decodeFixture(engine));
        let off;
        let injected = false;
        let poolStopped = false;
        try {
          // Disabling the pool retains Workers. Stop it before changing the script
          // so the replacement Workers load the fault-injected version.
          assert((await testing(engine).request("stallDecodeWorkers")) > 0, "no existing decode workers");
          await cold(true, { keepPool: true });
          const inline = await cold(false);
          injected = true;
          await toggle(true);
          await startPool();
          off = unstable(engine).onDiagnostic((event) => {
            if (String(event?.message).includes("invalid decode request frame")) poolStopped = true;
          });
          const recovered = await cold(true, { keepPool: true });
          assert(poolStopped, "no diagnostic for the malformed decode request");
          recovered.tiles.forEach((tile, i) => assert(same(tile, inline.tiles[i]), `tile ${i} differs after invalid batch`));
        } finally {
          off?.();
          try {
            if (injected) await toggle(false);
          } finally {
            try {
              if (injected && !poolStopped && await testing(engine).request("stallDecodeWorkers"))
                await cold(true, { keepPool: true });
              await startPool();
            } finally {
              await testing(engine).request("enablePrefetch");
            }
          }
        }
      });
    } else {
      log("SKIP malformed decode request injection: requires the browser verification server");
    }
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
    await test("cache pressure builds and pins an overview through pan and rotation", async () => {
      doc=await engine.createDocument({width:1024,height:1024});
      const a=await image(engine,"rgba(40,120,210,.6)",1024,1024);
      await doc.transact("overview input",tx=>tx.addRasterFromAsset(a));
      engine.viewport.set({center:[128,128],zoom:1,rotation:0.2});
      await testing(engine).request("resetRenderCache",{cpu:false});
      await settled(engine);
      for(let x=1000;x<1256;x++) await unstable(engine).readTile(x,0);
      await testing(engine).request("enablePrefetch");
      let frame;
      for(let i=0;i<150;i++) {
        await delay(20); frame=await settled(engine);
        if(frame.gpu.overviewReady===frame.gpu.overviewTiles && frame.gpu.overviewTiles>0) break;
      }
      assert(frame.gpu.overviewTiles===1 && frame.gpu.overviewReady===1,"pressure did not build overview");
      const before=await unstable(engine).readTileLevel(2,0,0);
      for(let x=1256;x<1512;x++) await unstable(engine).readTile(x,0);
      engine.viewport.set({center:[800,800],zoom:0.25,rotation:-0.4});
      frame=await settled(engine);
      assert(frame.gpu.overviewReady===1,"overview was evicted");
      assert(same(before,await unstable(engine).readTileLevel(2,0,0)),"overview pixels changed");
    });
    await test("elliptical brush rotates with its angle and a round brush stays unchanged", async () => {
      const draw = async (angle, roundness) => {
        doc = await engine.createDocument({ width: 256, height: 256 });
        let layer;
        await doc.transact("layer", tx => { layer = tx.addRaster(); });
        const color = [0, 0, 0, 1];
        const s = doc.tools.begin("brush", { layer, tip: { size: 64, angle, roundness }, color });
        s.input({ x: 128.5, y: 128.5, pressure: 1 });
        await s.end();
        const painted = await unstable(engine).readTile(0, 0);
        const reference = new Uint8Array(await testing(engine).request("cpuReference", {
          config: { size: 64, hardness: 0.8, spacing: 0.15, opacity: 1, flow: 1, angle, roundness },
          color,
          samples: [128.5, 128.5, 1, 0, 0, 0],
        }));
        assert(painted.every((v, i) => Math.abs(v - reference[i]) <= 2), "CPU/GPU brush shape mismatch");
        return painted;
      };
      const horizontal = await draw(0, 0.4);
      const vertical = await draw(90, 0.4);
      for (const offset of [16, 20, 24]) {
        assert(pixel(horizontal, 128 + offset, 128)[3] > 200 && pixel(horizontal, 128, 128 + offset)[3] === 0,
          "zero degrees should extend along the horizontal axis");
        assert(pixel(vertical, 128, 128 + offset)[3] > 200 && pixel(vertical, 128 + offset, 128)[3] === 0,
          "ninety degrees should extend along the vertical axis");
      }
      assert(same(await draw(0, 1), await draw(45, 1)), "a circular brush changed when rotated");
    });
    await test("elliptical brush crosses a tile boundary and follows the layer's rotation", async () => {
      doc = await engine.createDocument({ width: 512, height: 256 });
      let layer;
      await doc.transact("layer", tx => { layer = tx.addRaster(); });
      const dab = async (x, y, angle) => {
        const s = doc.tools.begin("brush", { layer, tip: { size: 64, angle, roundness: 0.4 }, color: [0, 0, 0, 1] });
        s.input({ x, y, pressure: 1 });
        await s.end();
      };
      await dab(250.5, 128.5, 90);
      for (const tileX of [0, 1]) {
        const gpu = await unstable(engine).readTile(tileX, 0);
        const cpu = new Uint8Array(await testing(engine).request("cpuReference", {
          config: { size: 64, hardness: 0.8, spacing: 0.15, opacity: 1, flow: 1, angle: 90, roundness: 0.4 },
          color: [0, 0, 0, 1],
          samples: [250.5 - tileX * 256, 128.5, 1, 0, 0, 0],
        }));
        assert(pixel(gpu, tileX === 0 ? 255 : 0, 128)[3] > 200, `brush missed tile ${tileX}`);
        assert(gpu.every((v, i) => Math.abs(v - cpu[i]) <= 2), `CPU/GPU brush mismatch in tile ${tileX}`);
      }
      await doc.undo();
      await doc.transact("rotate layer", tx => tx.setTransform(layer, {
        x: 256, y: 0, scaleX: 1, scaleY: 1, rotation: Math.PI / 2,
      }));
      // Layer center (64.5, 64.5) maps to document center (191.5, 64.5).
      await dab(191.5, 64.5, 0);
      const rotated = await unstable(engine).readTile(0, 0);
      assert(pixel(rotated, 191, 84)[3] > 200 && pixel(rotated, 211, 64)[3] === 0,
        "the brush's local horizontal axis did not rotate with the layer");
    });
    await test("pen tilt rotates an elliptical dab while round pixels stay identical", async () => {
      const draw = async (tiltY, roundness) => {
        doc = await engine.createDocument({ width: 256, height: 256 });
        let layer;
        await doc.transact("layer", tx => { layer = tx.addRaster(); });
        const s = doc.tools.begin("brush", {
          layer, tip: { size: 64, roundness }, dynamics: { angleFromTilt: true },
        });
        s.input({ x: 128.5, y: 128.5, tiltY });
        await s.end();
        return unstable(engine).readTile(0, 0);
      };
      const horizontal = await draw(0, 0.4);
      const vertical = await draw(45, 0.4);
      assert(pixel(horizontal, 148, 128)[3] > 200 && pixel(horizontal, 128, 148)[3] === 0,
        "the default tip was not horizontal");
      assert(pixel(vertical, 128, 148)[3] > 200 && pixel(vertical, 148, 128)[3] === 0,
        "pen tilt did not rotate the tip");
      assert(same(await draw(0, 1), await draw(45, 1)), "pen tilt changed a round tip");
    });
    await test("pressure-driven opacity and speed-driven flow match the CPU reference", async () => {
      doc = await engine.createDocument({ width: 256, height: 256 });
      let layer;
      await doc.transact("layer", tx => { layer = tx.addRaster(); });
      const opacity = { input: "pressure", points: [[0, 0], [1, 1]] };
      const flow = { input: "speed", points: [[0, 0.5], [1, 1]] };
      const color = [0.2, 0.4, 0.8, 1];
      const s = doc.tools.begin("brush", {
        layer, tip: { size: 20, spacing: 1 }, color, dynamics: { size: null, opacity, flow },
      });
      s.input({ x: 40.5, y: 128.5, pressure: 0.5, time: 0 });
      s.input({ x: 120.5, y: 128.5, pressure: 1, time: 20 });
      await s.end();
      const gpu = await unstable(engine).readTile(0, 0);
      const cpu = new Uint8Array(await testing(engine).request("cpuReference", {
        config: {
          size: 20, hardness: 0.8, spacing: 1, opacity: 1, flow: 1,
          dynamics: { size: { input: "pressure", points: [[0, 1], [1, 1]] }, opacity, flow },
        },
        color, samples: [40.5, 128.5, 0.5, 0, 0, 0, 120.5, 128.5, 1, 0, 0, 20],
      }));
      assert(Math.abs(pixel(gpu, 40, 128)[3] - 64) <= 2, "opacity and flow did not multiply");
      assert(pixel(gpu, 47, 128)[3] > 40, "null size did not disable pressure scaling");
      assert(pixel(gpu, 120, 128)[3] > 245, "faster strokes did not reach full flow");
      assert(gpu.every((v, i) => Math.abs(v - cpu[i]) <= 2), "CPU/GPU dynamics mismatch");
    });
    await test("an invalid curve is refused before the stroke starts", async () => {
      doc = await engine.createDocument({ width: 256, height: 256 });
      let layer;
      await doc.transact("layer", tx => { layer = tx.addRaster(); });
      const empty = await unstable(engine).readTile(0, 0);
      // The two distinct JS input positions round to the same Rust f32 value.
      const size = { input: "pressure", points: [[0, 0], [0.5, 0.5], [0.500000001, 0.6], [1, 1]] };
      let sdkError;
      try { doc.tools.begin("brush", { layer, dynamics: { size } }); }
      catch (e) { sdkError = e; }
      assert(sdkError?.code === "INVALID_ARGUMENT" && /brush curve/.test(sdkError.message),
        "the SDK did not reject the invalid curve");
      // Bypass the SDK to check that WASM independently validates both values and shape.
      const beginRaw = dynamics => testing(engine).request("beginBrush", {
        layer,
        config: { size: 24, hardness: 0.8, spacing: 0.15, opacity: 1, flow: 1, dynamics },
        color: [0, 0, 0, 1], erase: false, mask: false,
      });
      for (const [dynamics, message] of [[{ size }, /brush curve/], [{ angleFromTilt: true }, /brush config/]]) {
        await beginRaw(dynamics).then(
          () => { throw new Error("invalid wire config started a stroke"); },
          e => assert(e.code === "INVALID_ARGUMENT" && message.test(e.message), String(e)),
        );
      }
      const s = doc.tools.begin("brush", { layer, tip: { size: 0 } });
      s.input({ x: 128.5, y: 128.5 });
      await s.end().then(
        () => { throw new Error("an invalid brush size started a stroke"); },
        e => assert(e.code === "INVALID_ARGUMENT" && /brush size/.test(e.message), String(e)),
      );
      assert(same(empty, await unstable(engine).readTile(0, 0)), "an invalid curve painted pixels");
      const valid = doc.tools.begin("brush", { layer });
      valid.input({ x: 128.5, y: 128.5 });
      await valid.end();
      assert(pixel(await unstable(engine).readTile(0, 0), 128, 128)[3] === 255,
        "an invalid curve prevented the next valid stroke");
    });
    await test("a reusable image tip stamps rotated coverage and saves as ordinary tiles", async () => {
      const c = new OffscreenCanvas(32, 16);
      const ctx = c.getContext("2d");
      ctx.fillStyle = "white";
      ctx.fillRect(5, 2, 4, 12);
      ctx.fillRect(5, 11, 22, 3);
      ctx.fillStyle = "rgba(255, 0, 0, 0.5)";
      ctx.fillRect(0, 0, 1, 1);
      const imported = await engine.assets.importBrushTip(await c.convertToBlob());
      assert(imported.width === 32 && imported.height === 16 && imported.pixels[0] === 27 &&
        imported.pixels[1] === 0 && imported.pixels[11 * 32 + 26] === 255, "tip coverage conversion");
      // A caller can pass an R8 subarray and retain its backing buffer across strokes.
      const backing = new Uint8Array(imported.pixels.length + 2);
      backing.set(imported.pixels, 1);
      const image = { ...imported, pixels: backing.subarray(1, -1) };
      const color = [0, 0, 0, 1];
      const draw = async (angle) => {
        doc = await engine.createDocument({ width: 512, height: 256 });
        let layer;
        await doc.transact("layer", tx => { layer = tx.addRaster(); });
        const tool = doc.tools.begin("brush", { layer, color, tip: { size: 64, angle, image } });
        tool.input({ x: 250.5, y: 128.5 });
        await tool.end();
        const tiles = [];
        for (const tileX of [0, 1]) {
          const gpu = await unstable(engine).readTile(tileX, 0);
          const cpu = new Uint8Array(await testing(engine).request("cpuReference", {
            config: { size: 64, hardness: 0.8, spacing: 0.15, opacity: 1, flow: 1, angle, roundness: 1 },
            tipImage: { width: image.width, height: image.height, pixels: image.pixels.slice().buffer },
            color,
            samples: [250.5 - tileX * 256, 128.5, 1, 0, 0, 0],
          }));
          assert(close(gpu, cpu), `tile ${tileX} differs from the CPU tip reference at ${angle}°`);
          tiles.push(gpu);
        }
        return tiles;
      };
      const straight = await draw(0);
      assert(pixel(straight[1], 8, 138)[3] > 200, "image tip did not cross the tile boundary");
      const rotated = await draw(90);
      assert(pixel(rotated[0], 240, 142)[3] > 200 && pixel(rotated[1], 8, 138)[3] === 0,
        "image did not rotate with roundness 1");
      assert(same(image.pixels, imported.pixels), "beginBrush changed the reusable image");
      const saved = await testing(engine).saveBytes();
      doc = await engine.open(new Blob([saved]));
      assert(doc.getSnapshot().assets.length === 0, "a stroke kept its brush tip as a document asset");
      for (const tileX of [0, 1]) {
        assert(same(rotated[tileX], await unstable(engine).readTile(tileX, 0)), "reopen changed tipped pixels");
      }
    });
    await test("invalid image tips cannot start a stroke", async () => {
      doc = await engine.createDocument({ width: 256, height: 256 });
      let layer;
      await doc.transact("layer", tx => { layer = tx.addRaster(); });
      const before = await doc.snapshotHash();
      for (const [width, height, length] of [[8, 16, 128], [2048, 16, 32768], [32, 16, 511]]) {
        let code;
        try {
          doc.tools.begin("brush", { layer, tip: { image: { width, height, pixels: new Uint8Array(length) } } });
        } catch (e) { code = e.code; }
        assert(code === "INVALID_ARGUMENT", "SDK accepted an invalid image tip");
        await testing(engine).request("beginBrush", {
          layer, color: [0, 0, 0, 1], erase: false, mask: false,
          config: { size: 24, hardness: 0.8, spacing: 0.15, opacity: 1, flow: 1 },
          tipImage: { width, height, pixels: new ArrayBuffer(length) },
        }).then(
          () => { throw Error("the Worker accepted an invalid image tip"); },
          e => assert(e.code === "INVALID_ARGUMENT" && /brush tip/.test(e.message), String(e)),
        );
      }
      const small = new OffscreenCanvas(8, 16);
      small.getContext("2d").fillRect(0, 0, 8, 16);
      await engine.assets.importBrushTip(await small.convertToBlob()).then(
        () => { throw Error("the importer accepted an undersized image"); },
        e => assert(e.code === "INVALID_ARGUMENT", String(e)),
      );
      assert(await doc.snapshotHash() === before, "invalid image tip changed the document");
      const tool = doc.tools.begin("brush", { layer });
      await tool.cancel();
    });
    await test("GPU loss cancels an image-tip stroke and the same tip works after recovery", async () => {
      doc = await engine.createDocument({ width: 256, height: 256 });
      let layer;
      await doc.transact("layer", tx => { layer = tx.addRaster(); });
      const before = await doc.snapshotHash();
      const image = { width: 16, height: 16, pixels: new Uint8Array(256).fill(255) };
      const start = () => doc.tools.begin("brush", { layer, tip: { image } });
      const tool = start();
      tool.input({ x: 128.5, y: 128.5 });
      await tool.flush();
      await gpuRestored(engine, () => testing(engine).destroyGpu());
      await tool.end().then(
        () => { throw Error("device loss committed an open tipped stroke"); },
        e => assert(e.code === "GPU_LOST", String(e)),
      );
      assert(await doc.snapshotHash() === before, "device loss changed the document");
      const retry = start();
      retry.input({ x: 128.5, y: 128.5 });
      await retry.end();
      assert(pixel(await unstable(engine).readTile(0, 0), 128, 128)[3] === 255,
        "the same tip did not draw after recovery");
    });
    await test("brush texture stays fixed across strokes and follows the layer", async () => {
      const c = new OffscreenCanvas(17, 16);
      const ctx = c.getContext("2d");
      ctx.fillStyle = "#fff";
      ctx.fillRect(0, 0, 1, 16);
      const pattern = await engine.assets.importBrushTexture(await c.convertToBlob());
      assert(pattern.width === 17 && pattern.pixels[0] === 255 && pattern.pixels[1] === 0, "brush texture conversion");
      doc = await engine.createDocument({ width: 256, height: 256 });
      let layer;
      await doc.transact("layer", tx => { layer = tx.addRaster(); });
      const color = [0, 0, 0, 1];
      const paint = async (x, texture, image) => {
        const tool = doc.tools.begin("brush", { layer, tip: { size: 80, texture, image }, color });
        tool.input({ x, y: 128.5 });
        await tool.end();
        return unstable(engine).readTile(0, 0);
      };
      const texture = { image: pattern };
      const first = await paint(128.5, texture);
      const second = await paint(135.5, texture);
      const reference = new Uint8Array(await testing(engine).request("cpuReference", {
        config: { size: 80, hardness: 0.8, spacing: 0.15, opacity: 1, flow: 1 },
        texture: { image: { width: 17, height: 16, pixels: pattern.pixels.slice().buffer }, scale: 1, depth: 1 },
        color, samples: [128.5, 128.5, 1, 0, 0, 0],
      }));
      assert(close(first, reference), "CPU and GPU paper grain differ");
      for (const pixels of [first, second]) {
        assert(pixel(pixels, 119, 128)[3] === 255 && pixel(pixels, 120, 128)[3] === 0, "paper grain shifted or filled");
      }
      await doc.transact("translate layer", tx => tx.setTransform(layer, { x: 5, y: 0, scaleX: 1, scaleY: 1, rotation: 0 }));
      const moved = await paint(133.5, texture);
      assert(pixel(moved, 124, 128)[3] === 255 && pixel(moved, 125, 128)[3] === 0,
        "paper grain did not move with its layer");
      const tip = { width: 16, height: 16, pixels: new Uint8Array(256).fill(255) };
      const plain = await paint(133.5, undefined, tip);
      await doc.undo();
      const neutral = await paint(133.5, { image: pattern, depth: 0 }, tip);
      assert(same(plain, neutral), "a zero-depth texture changed image-tip pixels");
      await doc.undo();
      const tinted = await paint(133.5, { image: pattern, scale: 0.5, depth: 1 }, tip);
      assert(Math.abs(pixel(tinted, 108, 95)[3] - 191) <= 2 && pixel(tinted, 110, 95)[3] === 0,
        "combining tip and texture lost the non-default scale");
      assert((await settled(engine)).gpu.stroke === 0, "finished brush retained GPU textures");
      await doc.transact("move layer past the stamp", tx => tx.setTransform(layer, {
        x: 145, y: 0, scaleX: 1, scaleY: 1, rotation: 0,
      }));
      const negative = await paint(128.5, texture);
      assert(pixel(negative, 128, 128)[3] === 255 && pixel(negative, 129, 128)[3] === 0,
        "the GPU did not wrap grain at negative layer coordinates");
    });
    await test("invalid brush textures cannot start a stroke", async () => {
      doc = await engine.createDocument({ width: 256, height: 256 });
      let layer;
      await doc.transact("layer", tx => { layer = tx.addRaster(); });
      const before = await doc.snapshotHash();
      for (const [scale, depth, length] of [[0.049, 1, 256], [17, 1, 256], [1, -0.01, 256], [1, 1.01, 256], [NaN, 1, 256], [1, 1, 255], [1, 0, 255]]) {
        const image = { width: 16, height: 16, pixels: new Uint8Array(length) };
        let code;
        try {
          doc.tools.begin("brush", { layer, tip: { texture: { image, scale, depth } } });
        } catch (error) { code = error.code; }
        assert(code === "INVALID_ARGUMENT", "the SDK accepted an invalid brush texture");
        await testing(engine).request("beginBrush", {
          layer, color: [0, 0, 0, 1], erase: false, mask: false,
          config: { size: 24, hardness: 0.8, spacing: 0.15, opacity: 1, flow: 1 },
          texture: { image: { ...image, pixels: image.pixels.buffer }, scale, depth },
        }).then(
          () => { throw Error("the Worker accepted an invalid brush texture"); },
          error => assert(error.code === "INVALID_ARGUMENT" && /brush texture/.test(error.message), String(error)),
        );
      }
      assert(await doc.snapshotHash() === before, "an invalid brush texture changed the document");
      await doc.tools.begin("brush", { layer }).cancel();
    });
    await test("GPU loss cancels a textured brush and the texture can be reused", async () => {
      doc = await engine.createDocument({ width: 256, height: 256 });
      let layer;
      await doc.transact("layer", tx => { layer = tx.addRaster(); });
      const before = await doc.snapshotHash();
      const image = { width: 16, height: 16, pixels: new Uint8Array(256).fill(125) };
      const start = () => doc.tools.begin("brush", { layer, tip: { texture: { image } } });
      const tool = start();
      tool.input({ x: 128.5, y: 128.5 });
      await tool.flush();
      await gpuRestored(engine, () => testing(engine).destroyGpu());
      await tool.end().then(
        () => { throw Error("device loss committed a textured stroke"); },
        error => assert(error.code === "GPU_LOST", String(error)),
      );
      assert(await doc.snapshotHash() === before, "device loss changed the document");
      const retry = start();
      retry.input({ x: 128.5, y: 128.5 });
      await retry.end();
      assert(Math.abs(pixel(await unstable(engine).readTile(0, 0), 128, 128)[3] - 125) <= 2,
        "the textured brush did not draw after recovery");
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
            const root = await navigator.storage.getDirectory();
            const file = await root.getFileHandle(data.name);
            pendingWriter = await file.createWritable();
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
          interruptedWriter.postMessage({ name: file.name });
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
    await test("document listeners can call the engine while a restart applies the recovered state", async () => {
      const replies = [];
      const off = doc.subscribe(() => {
        replies.push(doc.snapshotHash().then(() => "ok", (error) => error.message));
      });
      try {
        await testing(engine).crash();
      } finally {
        off();
      }
      assert(engine.document === doc, "the restart replaced the document handle");
      const settled = await Promise.all(replies);
      assert(settled.length > 0, "the restart did not notify document listeners");
      assert(
        settled.every((r) => r === "ok"),
        "a listener's request failed during the restart: " + settled.join(", "),
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
    await test("the package size against the .alto cap is reported after a commit and a save", async () => {
      const seen = [];
      const off = engine.on("packageSize", (size) => seen.push(size));
      try {
        await stroke(doc, brushLayer, "brush", [{ x: 400, y: 300, pressure: 1 }]);
        await doc.versions.commit("package size");
        assert(seen.length === 1, `a commit sent ${seen.length} packageSize events`);
        const now = await doc.packageSize();
        assert(now.limit === 512 * 1024 * 1024 && now.near === false, JSON.stringify(now));
        assert(now.bytes === seen[0].bytes && now.bytes > 0, `${now.bytes} != ${seen[0].bytes}`);
        assert(seen[0].limit === now.limit && seen[0].near === false, JSON.stringify(seen[0]));
        let written = 0;
        await doc.save({ write: async (stream) => { written = (await new Response(stream).arrayBuffer()).byteLength; } });
        assert(seen.length === 2 && seen[1].bytes === now.bytes && seen[1].limit === now.limit && seen[1].near === false, JSON.stringify(seen));
        // The cap counts the entries' data; the archive adds ZIP headers to it.
        assert(written > now.bytes, `archive ${written} <= payload ${now.bytes}`);
      } finally {
        off();
      }
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
      await doc.durable();
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
      // An earlier edit can still have a queued append on slower OPFS implementations.
      await doc.durable();
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
      await doc.durable();
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
    await test("undo limit is validated and persists across document creation and restart", async () => {
      const { createEngine, deleteRecovery } = await import("../../packages/alto-sdk/dist/index.js");
      for (const steps of [0, 1001, 1.5, NaN]) {
        let rejected = false;
        try { await createEngine({ container: document.createElement("div"), undo: { steps } }); }
        catch (error) { rejected = error.code === "INVALID_ARGUMENT" && error.details?.reason === "undo-steps"; }
        assert(rejected, `invalid undo limit ${steps} accepted`);
      }
      const namespace = `test-undo/${crypto.randomUUID()}`;
      const other = await createEngine({ container: document.createElement("div"), undo: { steps: 2 }, storage: { namespace } });
      try {
        let d = await other.createDocument({ width: 64, height: 64 });
        const edit = async () => { for (let i = 0; i < 4; i++) await d.transact("layer", tx => tx.addRaster()); };
        await edit();
        assert(d.getSnapshot().history.undo === 2, "limit not applied to new document");
        await d.undo(); await d.undo();
        assert(d.getSnapshot().history.undo === 0, "undo did not stop at the configured limit");
        const exhausted = await d.snapshotHash();
        await d.undo();
        assert(await d.snapshotHash() === exhausted, "undo passed the configured limit");
        await edit();
        const beforeRestart = await d.snapshotHash();
        await d.durable();
        await testing(other).crash();
        d = other.document;
        assert(d.getSnapshot().history.undo === 2, "undo history changed during recovery");
        assert(await d.snapshotHash() === beforeRestart, "document changed during recovery");
        await edit();
        assert(d.getSnapshot().history.undo === 2, "limit lost after restart");
        const saved = await testing(other).saveBytes();
        d = await other.open(new Blob([saved]));
        await edit();
        assert(d.getSnapshot().history.undo === 2, "limit lost when opening a package");
        d = await other.createDocument({ width: 64, height: 64 });
        await edit();
        assert(d.getSnapshot().history.undo === 2, "limit lost when replacing document");
      } finally {
        await other.dispose();
        await deleteRecovery(namespace);
      }
    });
    await test("recovery replays every undo before applying a smaller undo limit", async () => {
      const { createEngine, deleteRecovery } = await import("../../packages/alto-sdk/dist/index.js");
      const namespace = `test-undo/${crypto.randomUUID()}`;
      const start = steps => createEngine({ container: document.createElement("div"), undo: { steps }, storage: { namespace } });
      let session;
      try {
        session = await start(8);
        let d = await session.createDocument({ width: 64, height: 64 });
        for (let i = 0; i < 5; i++) await d.transact("layer", tx => tx.addRaster());
        await d.versions.commit("checkpoint with undo history");
        await d.durable();
        for (let i = 0; i < 3; i++) await d.undo();
        const expected = await d.snapshotHash();
        await d.durable();
        const store = await testing(session).recoveryStore();
        assert(store.records === 3, "expected three undo records after the checkpoint");
        await session.dispose();

        session = await start(1);
        await testing(session).holdStorage(true);
        d = await session.recovery.restore();
        assert(await d.snapshotHash() === expected, "recovery stopped before the final recorded undo");
        assert(d.getSnapshot().history.undo === 1, "recovery did not apply the configured undo limit");
        assert(d.getSnapshot().history.redo === 3, "recovery lost redo history");
        assert(await testing(session).storageHeld() === "checkpoint", "checkpoint was not queued first");
        await d.redo();
        const expectedAfterRedo = await d.snapshotHash();
        assert(await testing(session).storageHeld() === "checkpoint",
          "redo bypassed the held checkpoint");
        await testing(session).holdStorage(false);
        await d.durable();
        const migrated = await testing(session).recoveryStore();
        assert(migrated.latestGeneration > store.latestGeneration && migrated.records === 1,
          "redo was not appended after checkpointing the new undo limit");
        await testing(session).crash();
        d = session.document;
        assert(await d.snapshotHash() === expectedAfterRedo, "recovery lost an edit recorded under the new limit");
        assert(d.getSnapshot().history.undo === 1 && d.getSnapshot().history.redo === 2,
          "recovery with the new limit changed the undo or redo history");
      } finally {
        if (session?.status.state === "ready") await testing(session).holdStorage(false).catch(() => {});
        await session?.dispose();
        await deleteRecovery(namespace);
      }
    });
    for (const steps of [1, 8]) {
      await test(`recovery from an older pack checkpoints at undo limit ${steps}`, async () => {
        const { createEngine, deleteRecovery } = await import("../../packages/alto-sdk/dist/index.js");
        const { namespaceFolder } = await import("../../packages/alto-sdk/dist/storage-core.js");
        const namespace = `test-undo/${crypto.randomUUID()}`;
        const start = steps => createEngine({ container: document.createElement("div"), undo: { steps }, storage: { namespace } });
        let session;
        let unreadableWorkerURL;
        let unreadableSetupURL;
        try {
          session = await start(8);
          const d = await session.createDocument({ width: 64, height: 64 });
          const add = async color => {
            const asset = await image(session, color, 64, 64);
            await d.transact("image", tx => tx.addRasterFromAsset(asset));
            await d.versions.commit(color);
            await d.durable();
            return (await testing(session).recoveryStore()).latestGeneration;
          };
          const previous = await add("#ff0000");
          const latest = await add("#0000ff");
          // The previous checkpoint's journal also contains the committed edits.
          const expected = await d.snapshotHash();
          let latestOnly;
          if (steps === 8) {
            await d.transact("latest generation only", tx => tx.addRaster());
            latestOnly = await d.snapshotHash();
            await d.durable();
          }
          await session.dispose();

          const root = await (await navigator.storage.getDirectory()).getDirectoryHandle("alto-engine");
          const directory = await root.getDirectoryHandle(namespaceFolder(namespace));
          const packs = await directory.getDirectoryHandle("packs");
          await packs.getFileHandle(`${previous}.pack`);
          if (steps === 8) {
            // Use the inline storage path in this test Worker to inject a transient OS read error.
            const workerModule = new URL("../../packages/alto-sdk/dist/worker.js", import.meta.url);
            unreadableSetupURL = URL.createObjectURL(new Blob([`
              const RealWorker = self.Worker;
              self.Worker = class extends RealWorker {
                constructor(url, options) {
                  if (String(url).endsWith("/storage-worker.js")) throw new Error("use inline test storage");
                  super(url, options);
                }
              };
              const read = FileSystemFileHandle.prototype.getFile;
              FileSystemFileHandle.prototype.getFile = function () {
                if (this.name === ${JSON.stringify(`${latest}.pack`)})
                  throw new DOMException("temporary test read failure", "NotReadableError");
                return read.call(this);
              };
            `], { type: "text/javascript" }));
            unreadableWorkerURL = URL.createObjectURL(new Blob([`
              import ${JSON.stringify(unreadableSetupURL)};
              import ${JSON.stringify(workerModule.href)};
            `], { type: "text/javascript" }));
            session = await start(steps);
            testing(session).setWorkerURL(unreadableWorkerURL);
            let failure;
            try { await testing(session).crash(); }
            catch (error) { failure = error; }
            assert(failure?.code === "STORAGE" && failure.message.includes("NotReadableError"),
              "transient read failure did not stop recovery: " + failure);
            testing(session).setWorkerURL(null);
            await session.retry();
            assert(await session.document.snapshotHash() === latestOnly, "retry lost the latest confirmed edit");
            await session.dispose();
          }
          await packs.removeEntry(`${latest}.pack`);

          session = await start(steps);
          const restored = await session.recovery.restore();
          assert(await restored.snapshotHash() === expected, "recovery lost edits from the older checkpoint and its journal");
          assert(restored.getSnapshot().warnings.some(w => w.code === "RECOVERED_OLDER_GENERATION"),
            "older generation warning missing");
          await restored.durable();
          assert((await testing(session).recoveryStore()).latestGeneration > latest,
            "the recovered checkpoint could not be written");
          await testing(session).crash();
          assert(await session.document.snapshotHash() === expected, "the new checkpoint did not reopen");
          assert(!session.document.getSnapshot().warnings.some(w => w.code === "RECOVERED_OLDER_GENERATION"),
            "the new checkpoint still pointed to the missing pack");
        } finally {
          await session?.dispose();
          if (unreadableWorkerURL) URL.revokeObjectURL(unreadableWorkerURL);
          if (unreadableSetupURL) URL.revokeObjectURL(unreadableSetupURL);
          await deleteRecovery(namespace);
        }
      });
    }
    await test("failed recovery checkpoint leaves the legacy journal recoverable", async () => {
      const { createEngine, deleteRecovery } = await import("../../packages/alto-sdk/dist/index.js");
      const namespace = `test-undo/${crypto.randomUUID()}`;
      const start = steps => createEngine({ container: document.createElement("div"), undo: { steps }, storage: { namespace } });
      let session;
      let faultWorkerURL;
      try {
        session = await start(100);
        let d = await session.createDocument({ width: 64, height: 64 });
        await d.durable();
        await testing(session).legacyCheckpoint();
        for (let i = 0; i < 4; i++) await d.transact("layer", tx => tx.addRaster());
        for (let i = 0; i < 3; i++) await d.undo();
        const expected = await d.snapshotHash();
        await d.durable();
        await session.dispose();

        session = await start(1);
        await testing(session).injectStorageFault("beforeCheckpoint");
        d = await session.recovery.restore();
        assert(await d.snapshotHash() === expected, "legacy replay changed the document");
        assert(d.getSnapshot().history.undo === 1 && d.getSnapshot().history.redo === 3,
          "legacy replay did not retain the expected undo and redo history");
        let failed = false;
        try { await d.durable(); } catch (error) { failed = error.code === "STORAGE"; }
        assert(failed && session.status.state === "storage-failed", "checkpoint failure was not reported");
        // Re-arm a different fault in the replacement Worker, before its recovery request.
        const workerModule = new URL("../../packages/alto-sdk/dist/worker.js", import.meta.url);
        faultWorkerURL = URL.createObjectURL(new Blob([`
          import ${JSON.stringify(workerModule.href)};
          const receive = self.onmessage;
          self.onmessage = event => {
            if (event.data.t === "recover") {
              receive({ data: { t: "injectStorageFault", id: -1, position: "beforeCheckpointParts" } });
            }
            receive(event);
          };
        `], { type: "text/javascript" }));
        testing(session).setWorkerURL(faultWorkerURL);
        const interrupted = await session.recovery.recoverAfterStorageFailure();
        assert(interrupted.document === d && await d.snapshotHash() === expected,
          "restart lost the recovered document when checkpoint generation failed");
        assert(session.status.state === "storage-failed" && session.status.error.message.includes("before checkpoint generation"),
          "restart hid the checkpoint generation failure");
        testing(session).setWorkerURL(null);
        const report = await session.recovery.recoverAfterStorageFailure();
        d = report.document;
        await d.durable();
        assert(await d.snapshotHash() === expected, "retry lost edits from the original journal");
      } finally {
        await session?.dispose();
        if (faultWorkerURL) URL.revokeObjectURL(faultWorkerURL);
        await deleteRecovery(namespace);
      }
    });
    await test("edits after a journal replay mismatch survive the next restart", async () => {
      const { createEngine, deleteRecovery } = await import("../../packages/alto-sdk/dist/index.js");
      const { namespaceFolder } = await import("../../packages/alto-sdk/dist/storage-core.js");
      const namespace = `test-undo/${crypto.randomUUID()}`;
      const start = () => createEngine({ container: document.createElement("div"), storage: { namespace } });
      let session;
      try {
        session = await start();
        let d = await session.createDocument({ width: 64, height: 64 });
        let first, second;
        await d.transact("layers", tx => { first = tx.addRaster(); second = tx.addRaster(); });
        await d.versions.commit("base");
        await d.durable();
        const rename = (layer, name) => d.transact("rename", tx => tx.setProps(layer, { name }));
        await rename(first, "retained");
        const prefix = await d.snapshotHash();
        await rename(second, "removed from the journal");
        await rename(first, "depends on the missing edit");
        await d.durable();
        const store = await testing(session).recoveryStore();
        assert(store.records === 3, "expected three records after the checkpoint");
        await session.dispose();

        // Remove one intact record from the test namespace, leaving the remaining
        // checksums valid so replay, rather than the journal reader, detects the gap.
        const root = await (await navigator.storage.getDirectory()).getDirectoryHandle("alto-engine");
        const directory = await root.getDirectoryHandle(namespaceFolder(namespace));
        let removed = false;
        for (const name of ["journal-a.bin", "journal-b.bin"]) {
          const handle = await directory.getFileHandle(name);
          const bytes = await (await handle.getFile()).arrayBuffer();
          const view = new DataView(bytes);
          if (view.getUint32(8, true) !== store.latestGeneration) continue;
          const next = at => at + 8 + view.getUint32(at, true);
          const omitted = next(next(12));
          const writer = await handle.createWritable();
          await writer.write(bytes.slice(0, omitted));
          await writer.write(bytes.slice(next(omitted)));
          await writer.close();
          removed = true;
          break;
        }
        assert(removed, "could not find the active journal");

        session = await start();
        d = await session.recovery.restore();
        assert(d.getSnapshot().warnings.some(w => w.code === "RECOVERY_DIVERGED"), "missing replay warning");
        assert(await d.snapshotHash() === prefix, "recovery did not retain the valid prefix");
        await rename(second, "saved after recovery");
        const expected = await d.snapshotHash();
        await d.durable();
        await testing(session).crash();
        d = session.document;
        assert(await d.snapshotHash() === expected, "restart lost the edit saved after recovery");
      } finally {
        await session?.dispose();
        await deleteRecovery(namespace);
      }
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
  const finishStarted=performance.now();
  await full.end();
  const finishMs=performance.now()-finishStarted;
  const afterFullStroke = await settled(engine);
  memory.push(afterFullStroke);
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
      gpuBreakdown: memory.reduce((a, b) => a.gpuBytes >= b.gpuBytes ? a : b).gpu,
      afterStroke: afterFullStroke.gpu,
      operationPeak: afterFullStroke.gpu.operationPeak / 1048576,
      readback: afterFullStroke.readback,
      finishMs,
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
