// Investigation fixture, not a product test: real container resizes while the GPU is
// busy. Each size is held past the engine's 100 ms resize debounce, so every step
// reaches the Worker's canvas. Counts unexpected device losses and how long each
// resize takes to reach the drawing buffer.
import { unstable } from "../../packages/alto-sdk/dist/unstable.js";

const STEPS = 40;

function percentile(values, p) {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))];
}

export async function runResizeStress(engine, log) {
  const results = [];
  const U = unstable(engine);
  let losses = 0;
  const offGpu = U.onGpu((event) => {
    if (event === "lost") losses++;
  });
  const stage = document.querySelector("#stage");
  try {
    const doc = await engine.createDocument({ width: 3072, height: 2048 });
    // Translucent layers with a different pattern per tile, so every display tile
    // composes many inputs (content addressing cannot merge identical tiles).
    for (let i = 0; i < 8; i++) {
      const c = new OffscreenCanvas(3072, 2048), ctx = c.getContext("2d");
      ctx.fillStyle = `rgba(${30 * i},${200 - 20 * i},140,.45)`;
      ctx.fillRect(0, 0, 3072, 2048);
      for (let y = 0; y < 2048; y += 256)
        for (let x = 0; x < 3072; x += 256) {
          ctx.fillStyle = `rgba(${(x / 8 + i * 29) % 255},${(y / 4 + i * 41) % 255},${(i * 53) % 255},.5)`;
          ctx.fillRect(x + 9 + i, y + 9 + i, 200, 200);
        }
      const asset = await engine.assets.importImage(await c.convertToBlob());
      await doc.transact("image.import", (tx) => tx.addRasterFromAsset(asset));
    }
    let layer;
    await doc.transact("layer.add", (tx) => (layer = tx.addRaster({ name: "stress" })));
    engine.viewport.set({ center: [1536, 1024], zoom: 1, rotation: 0 });
    await U.settle();
    const ratio = U.pixelRatio();
    const latencies = [];
    let strokeErrors = 0;
    for (let i = 0; i < STEPS; i++) {
      // A mix of the 3 px shifts seen at startup and larger layout changes.
      const height = 380 + (i % 4) * 3 + (i % 2 ? 41 : 0);
      stage.style.height = height + "px";
      const start = performance.now();
      const target = () => Math.max(1, Math.round(engine.element.getBoundingClientRect().height * ratio));
      const stroke = doc.tools.begin("brush", { layer, color: [i % 2, 0.3, 0.8, 1], tip: { size: 60 } });
      let reached = null;
      const off = U.onFrame((f) => {
        if (reached === null && f.surface?.[1] === target()) reached = performance.now();
      });
      // Keep work flowing until the new size is in the drawing buffer.
      for (let frame = 0; reached === null && frame < 240; frame++) {
        engine.viewport.panBy(frame % 2 ? 7 : -7, 0);
        stroke.input({ x: 1200 + frame * 6, y: 900 + i * 4, pressure: 1 });
        await new Promise(requestAnimationFrame);
      }
      off();
      try {
        await stroke.end();
      } catch {
        strokeErrors++;
      }
      if (reached !== null) latencies.push(reached - start);
      await U.settle();
    }
    const pass = losses === 0 && latencies.length === STEPS;
    results.push({
      name: "resize stress",
      pass,
      losses,
      strokeErrors,
      resizesReached: latencies.length,
      resizeLatencyMs: latencies.length
        ? { p50: percentile(latencies, 0.5), p95: percentile(latencies, 0.95), max: Math.max(...latencies) }
        : null,
    });
    log(`${pass ? "PASS" : "FAIL"} resize stress: ${losses} device losses, ${latencies.length}/${STEPS} resizes reached the canvas`);
  } catch (error) {
    results.push({ name: "resize stress: " + String(error.stack || error), pass: false, losses });
    log("FAIL " + error);
  } finally {
    offGpu();
    stage.style.height = "";
    document.querySelector("#test-results").dataset.report = JSON.stringify({ suite: "resize-stress", results });
  }
  return results;
}
