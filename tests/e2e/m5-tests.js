const assert = (v, m) => {
  if (!v) throw Error(m);
};
const same = (a, b) => a.length === b.length && a.every((v, i) => v === b[i]);
const close = (a, b, t = 2) => a.every((v, i) => Math.abs(v - b[i]) <= t);
const pixel = (t, x, y) =>
  Array.from(t.slice((y * 256 + x) * 4, (y * 256 + x) * 4 + 4));
async function at(engine, x, y) {
  return pixel(
    await engine.unstable.readTile(Math.floor(x / 256), Math.floor(y / 256)),
    x % 256,
    y % 256,
  );
}
async function image(engine, w, h, draw) {
  const c = new OffscreenCanvas(w, h),
    ctx = c.getContext("2d");
  draw(ctx);
  return engine.assets.importImage(await c.convertToBlob());
}
async function paint(
  doc,
  layer,
  x,
  y,
  { target = "content", color = [0, 0, 1, 1], size = 64, tool = "brush" } = {},
) {
  const s = doc.tools.begin(tool, {
    layer,
    target,
    tip: { size, hardness: 0.9 },
    color,
  });
  s.input({ x, y, pressure: 1 });
  await s.end();
}
function weights(radius) {
  const r = Math.ceil(radius),
    sigma = Math.max(radius / 3, 0.333333),
    a = [];
  let total = 0;
  for (let i = -r; i <= r; i++) {
    const w = Math.exp((-i * i) / (2 * sigma * sigma));
    a.push(w);
    total += w;
  }
  return a.map((v) => v / total);
}
function intervalBlur(x, left, right, radius) {
  const w = weights(radius),
    r = Math.ceil(radius);
  let sum = 0;
  for (let i = -r; i <= r; i++)
    if (x + i >= left && x + i < right) sum += w[i + r];
  return sum;
}
export async function runM5Tests(engine, log) {
  const results = [];
  async function test(name, fn) {
    const start = performance.now();
    await fn();
    results.push({ name, pass: true, ms: performance.now() - start });
    log("PASS " + name);
  }
  let doc, layer;
  try {
    await test("R8 selection combinations / feather / ellipse / lasso / Undo", async () => {
      doc = await engine.createDocument({ width: 512, height: 512 });
      await doc.transact("layer", (tx) => (layer = tx.addRaster()));
      await doc.selection.apply({
        kind: "rectangle",
        x: 200,
        y: 100,
        width: 80,
        height: 100,
      });
      const initial = JSON.stringify(doc.view.selection);
      assert(doc.view.selection.format === "r8", "selection format");
      await doc.selection.apply({
        kind: "rectangle",
        x: 240,
        y: 100,
        width: 80,
        height: 100,
        mode: "subtract",
      });
      assert(
        JSON.stringify(doc.view.selection) !== initial,
        "subtract did not change selection",
      );
      await doc.undo();
      assert(JSON.stringify(doc.view.selection) === initial, "selection Undo");
      await doc.redo();
      await doc.selection.apply({
        kind: "ellipse",
        x: 128,
        y: 128,
        width: 256,
        height: 256,
        feather: 8,
      });
      assert((await doc.selection.outline()).includes("M"), "ellipse outline");
      await doc.selection.apply({
        kind: "lasso",
        points: [
          [200, 220],
          [300, 220],
          [256, 320],
        ],
      });
      await doc.selection.invert();
      await doc.undo();
      await paint(doc, layer, 256, 250);
      assert((await at(engine, 256, 250))[3] > 200, "lasso interior");
      assert((await at(engine, 285, 270))[3] === 0, "lasso exterior");
      await doc.selection.apply({ kind: "all" });
      await doc.selection.apply({
        kind: "rectangle",
        x: 0,
        y: 0,
        width: 128,
        height: 128,
        mode: "intersect",
      });
      await paint(doc, layer, 64, 64);
      assert((await at(engine, 64, 64))[3] > 200, "intersection");
      await doc.selection.clear();
    });
    await test("document-space selection restricts GPU brush on a transformed layer", async () => {
      doc = await engine.createDocument({ width: 512, height: 256 });
      await doc.transact("transformed layer", (tx) => {
        layer = tx.addRaster();
        tx.setTransform(layer, {
          x: 64,
          y: 0,
          scaleX: 1,
          scaleY: 1,
          rotation: 0,
        });
      });
      await doc.selection.apply({
        kind: "rectangle",
        x: 200,
        y: 0,
        width: 56,
        height: 256,
      });
      await paint(doc, layer, 250, 128, { size: 100 });
      const painted = await engine.unstable.readTile(0, 0);
      assert((await at(engine, 240, 128))[3] > 200, "selected paint missing");
      assert((await at(engine, 268, 128))[3] === 0, "paint escaped selection");
      await doc.undo();
      assert((await at(engine, 240, 128))[3] === 0, "selected stroke Undo");
      await doc.redo();
      assert(
        same(painted, await engine.unstable.readTile(0, 0)),
        "selected stroke Redo differs",
      );
      await doc.selection.clear();
      await paint(doc, layer, 400, 128, { size: 64 });
      await paint(doc, layer, 400, 128, { size: 48, tool: "eraser" });
      const canonical = JSON.stringify(await doc.nodeTiles(layer));
      await doc.selection.apply({
        kind: "rectangle",
        x: 0,
        y: 0,
        width: 32,
        height: 256,
      });
      const selectedHistory = doc.view.history.undo;
      await paint(doc, layer, 400, 128, { size: 64, color: [1, 0, 0, 1] });
      assert(
        JSON.stringify(await doc.nodeTiles(layer)) === canonical &&
          doc.view.history.undo === selectedHistory,
        "unselected hidden RGB bytes changed",
      );
      await doc.selection.clear();
    });
    await test("layer mask / enable / linked transform / GPU mask brush preview / exact Undo", async () => {
      doc = await engine.createDocument({ width: 512, height: 256 });
      const red = await image(engine, 512, 256, (c) => {
        c.fillStyle = "red";
        c.fillRect(0, 0, 512, 256);
      });
      await doc.transact("red", (tx) => (layer = tx.addRasterFromAsset(red)));
      await doc.selection.apply({
        kind: "rectangle",
        x: 0,
        y: 0,
        width: 256,
        height: 256,
      });
      await doc.masks.fromSelection(layer);
      await doc.selection.clear();
      await engine.request("validateFrame");
      assert(
        close(await at(engine, 64, 128), [255, 0, 0, 255]),
        "mask interior",
      );
      assert((await at(engine, 300, 128))[3] === 0, "mask exterior");
      await doc.transact("mask off", (tx) =>
        tx.setProps(layer, { mask_enabled: false }),
      );
      assert((await at(engine, 300, 128))[3] === 255, "disabled mask");
      await doc.transact("linked move", (tx) => {
        tx.setProps(layer, { mask_enabled: true });
        tx.setTransform(layer, {
          x: 64,
          y: 0,
          scaleX: 1,
          scaleY: 1,
          rotation: 0,
        });
      });
      assert(
        (await at(engine, 280, 128))[3] === 255,
        "linked mask did not move",
      );
      await doc.transact("unlink", (tx) =>
        tx.setProps(layer, { mask_linked: false }),
      );
      assert(
        (await at(engine, 280, 128))[3] === 255,
        "unlink changed mask position",
      );
      await doc.transact("move independently", (tx) =>
        tx.setTransform(layer, {
          x: 128,
          y: 0,
          scaleX: 1,
          scaleY: 1,
          rotation: 0,
        }),
      );
      assert(
        (await at(engine, 350, 128))[3] === 0,
        "unlinked mask followed layer",
      );
      await doc.undo();
      await doc.undo();
      const before = await engine.unstable.readTile(0, 0),
        source = JSON.stringify(await doc.nodeTiles(layer));
      const session = doc.tools.begin("brush", {
        layer,
        target: "mask",
        tip: { size: 64, hardness: 0.9 },
        color: [0, 0, 0, 1],
      });
      session.input({ x: 200, y: 128, pressure: 1 });
      await session.flush();
      assert((await at(engine, 200, 128))[3] === 0, "mask preview missing");
      await session.end();
      const masked = await engine.unstable.readTile(0, 0);
      assert(doc.view.node(layer).props.mask.format === "r8", "mask not R8");
      assert(
        JSON.stringify(await doc.nodeTiles(layer)) === source,
        "mask painted source pixels",
      );
      await doc.undo();
      assert(same(before, await engine.unstable.readTile(0, 0)), "mask Undo");
      await doc.redo();
      assert(same(masked, await engine.unstable.readTile(0, 0)), "mask Redo");
    });
    await test("clipping chain preserves the base alpha including translucent bases", async () => {
      doc = await engine.createDocument({ width: 512, height: 256 });
      const base = await image(engine, 512, 256, (c) => {
          c.fillStyle = "rgba(255,0,0,.5)";
          c.fillRect(0, 0, 128, 256);
        }),
        blue = await image(engine, 512, 256, (c) => {
          c.fillStyle = "blue";
          c.fillRect(0, 0, 512, 256);
        });
      let top;
      await doc.transact("clipping pair", (tx) => {
        layer = tx.addRasterFromAsset(base);
        top = tx.addRasterFromAsset(blue);
        tx.setProps(top, { clip_to_below: true });
      });
      await engine.request("validateFrame");
      assert(
        close(await at(engine, 64, 128), [0, 0, 128, 128]),
        "clipping altered base alpha " + (await at(engine, 64, 128)),
      );
      assert((await at(engine, 200, 128))[3] === 0, "clipping escaped base");
      await doc.transact("clip off", (tx) =>
        tx.setProps(top, { clip_to_below: false }),
      );
      assert(close(await at(engine, 200, 128), [0, 0, 255, 255]), "clip off");
      await doc.undo();
    });
    await test("Gaussian blur matches CPU reference across both tile seams and preserves source", async () => {
      doc = await engine.createDocument({ width: 768, height: 512 });
      const a = await image(engine, 768, 512, (c) => {
        c.fillStyle = "white";
        c.fillRect(220, 220, 70, 70);
      });
      await doc.transact(
        "seam image",
        (tx) => (layer = tx.addRasterFromAsset(a)),
      );
      const original = JSON.stringify(await doc.nodeTiles(layer));
      await doc.effects.set(layer, [{ type: "gaussian_blur", radius: 12 }]);
      await engine.request("validateFrame");
      const tiles = new Map();
      for (const y of [0, 1])
        for (const x of [0, 1])
          tiles.set(x + "," + y, await engine.unstable.readTile(x, y));
      for (let y = 208; y < 302; y++)
        for (let x = 208; x < 302; x++) {
          const expected = Math.round(
              intervalBlur(x, 220, 290, 12) *
                intervalBlur(y, 220, 290, 12) *
                255,
            ),
            got = pixel(
              tiles.get(Math.floor(x / 256) + "," + Math.floor(y / 256)),
              x % 256,
              y % 256,
            );
          assert(
            close(got, [expected, expected, expected, expected]),
            `blur ${x},${y}: ${got} != ${expected}`,
          );
        }
      assert(
        JSON.stringify(await doc.nodeTiles(layer)) === original,
        "blur changed canonical pixels",
      );
      const blurred = await engine.unstable.readTile(0, 0);
      await doc.undo();
      assert((await at(engine, 214, 250))[3] === 0, "blur Undo");
      await doc.redo();
      assert(same(blurred, await engine.unstable.readTile(0, 0)), "blur Redo");
      await doc.effects.set(
        layer,
        [
          { type: "gaussian_blur", radius: 6 },
          { type: "gaussian_blur", radius: 9 },
        ],
        { useSelection: false },
      );
      await engine.request("validateFrame");
      assert((await at(engine, 215, 250))[3] > 0, "serial blur stack");
    });
    await test("effect captures selection and group blur receives a halo", async () => {
      doc = await engine.createDocument({ width: 512, height: 512 });
      const a = await image(engine, 512, 512, (c) => {
        c.fillStyle = "white";
        c.fillRect(250, 220, 12, 70);
      });
      let group;
      await doc.transact("group source", (tx) => {
        group = tx.addGroup({ isolated: true });
        layer = tx.addRasterFromAsset(a, { parent: group });
      });
      await doc.selection.apply({
        kind: "rectangle",
        x: 0,
        y: 0,
        width: 256,
        height: 512,
      });
      await doc.effects.set(group, [{ type: "gaussian_blur", radius: 12 }]);
      await doc.selection.clear();
      await engine.request("validateFrame");
      assert((await at(engine, 246, 250))[3] > 0, "selected filter missing");
      assert(
        (await at(engine, 260, 250))[3] === 255,
        "filter escaped captured selection",
      );
      assert(
        doc.view.node(group).props.effects[0].selection.format === "r8",
        "effect mask not captured",
      );
    });
    await test("adjustment only affects lower siblings in its group and supports a mask", async () => {
      doc = await engine.createDocument({ width: 512, height: 256 });
      const red = await image(engine, 512, 256, (c) => {
          c.fillStyle = "red";
          c.fillRect(0, 0, 512, 256);
        }),
        gray = await image(engine, 256, 256, (c) => {
          c.fillStyle = "rgb(128,128,128)";
          c.fillRect(0, 0, 256, 256);
        });
      let group, adjustment;
      await doc.transact("group adjustment", (tx) => {
        tx.addRasterFromAsset(red);
        group = tx.addGroup({ isolated: false });
        layer = tx.addRasterFromAsset(gray, { parent: group });
        adjustment = tx.addAdjustment(
          { type: "color_adjust", brightness: 0.2, contrast: 1, saturation: 1 },
          { parent: group },
        );
      });
      await engine.request("validateFrame");
      assert(
        close(await at(engine, 64, 128), [179, 179, 179, 255]),
        "adjustment color " + (await at(engine, 64, 128)),
      );
      assert(
        close(await at(engine, 300, 128), [255, 0, 0, 255]),
        "adjustment escaped group",
      );
      await doc.transact("adjustment blend", (tx) =>
        tx.setProps(adjustment, { blend: "multiply" }),
      );
      assert(
        close(await at(engine, 64, 128), [90, 90, 90, 255]),
        "adjustment blend",
      );
      await doc.undo();
      await doc.selection.apply({
        kind: "rectangle",
        x: 0,
        y: 0,
        width: 128,
        height: 256,
      });
      await doc.masks.fromSelection(adjustment);
      await doc.selection.clear();
      assert(
        close(await at(engine, 192, 128), [128, 128, 128, 255]),
        "adjustment mask",
      );
      await doc.transact("adjustment change", (tx) =>
        tx.setAdjustment(adjustment, {
          type: "color_adjust",
          brightness: 0,
          contrast: 1,
          saturation: 0,
        }),
      );
      await doc.undo();
      assert(
        close(await at(engine, 64, 128), [179, 179, 179, 255]),
        "adjustment Undo",
      );
      // Blur the backdrop across x=256; an insufficient group halo would create a seam here.
      await doc.transact("blur adjustment", (tx) =>
        tx.setAdjustment(adjustment, { type: "gaussian_blur", radius: 12 }),
      );
      await doc.masks.remove(adjustment);
      await engine.request("validateFrame");
      const near = await at(engine, 255, 128),
        far = await at(engine, 256, 128);
      assert(Math.abs(near[1] - far[1]) < 20, "backdrop blur seam");
    });
    await test("M5 objects survive ZIP, GPU loss, journal recovery and invalid edits remain atomic", async () => {
      await doc.selection.apply({
        kind: "ellipse",
        x: 128,
        y: 32,
        width: 256,
        height: 192,
        feather: 8,
      });
      await doc.effects.set(layer, [{ type: "gaussian_blur", radius: 8 }]);
      const hash = (await doc.snapshotHash()),
        reference = await engine.unstable.readTile(0, 0),
        bytes = await engine.unstable.saveBytes();
      doc = await engine.openDocument(new Blob([bytes]));
      assert((await doc.snapshotHash()) === hash, "M5 snapshot changed on open");
      assert(
        same(reference, await engine.unstable.readTile(0, 0)),
        "M5 ZIP pixels",
      );
      const restored = new Promise((resolve) => {
        const off = engine.on("gpuRestored", () => {
          off();
          resolve();
        });
      });
      await engine.unstable.destroyGpu();
      await restored;
      assert(
        same(reference, await engine.unstable.readTile(0, 0)),
        "M5 GPU loss",
      );
      await doc.transact("journal mask state", (tx) =>
        tx.setProps(layer, { mask_enabled: false }),
      );
      const recoveryHash = (await doc.snapshotHash());
      await doc.durable();
      await engine.unstable.crash();
      doc = engine.document;
      assert((await doc.snapshotHash()) === recoveryHash, "M5 journal hash");
      assert(
        same(reference, await engine.unstable.readTile(0, 0)),
        "M5 recovery pixels",
      );
      const before = (await doc.snapshotHash()),
        undo = doc.view.history.undo;
      let rejected = false;
      try {
        await doc.effects.set(layer, [{ type: "gaussian_blur", radius: -1 }]);
      } catch {
        rejected = true;
      }
      assert(
        rejected &&
          (await doc.snapshotHash()) === before &&
          doc.view.history.undo === undo,
        "invalid effect was not atomic",
      );
      rejected = false;
      try {
        await doc.effects.set(layer, [42]);
      } catch {
        rejected = true;
      }
      assert(
        rejected && engine.document === doc && (await doc.snapshotHash()) === before,
        "invalid effect crashed or changed document",
      );
    });
    log("ALL " + results.length + " M5 TESTS PASSED");
  } catch (error) {
    results.push({ pass: false, error: String(error) });
    log("FAIL " + error);
    throw error;
  } finally {
    document.querySelector("#test-results").dataset.report = JSON.stringify({
      date: new Date().toISOString(),
      milestone: "M5",
      userAgent: navigator.userAgent,
      crossOriginIsolated,
      viewport: {
        width: innerWidth,
        height: innerHeight,
        dpr: devicePixelRatio,
      },
      results,
    });
  }
}
