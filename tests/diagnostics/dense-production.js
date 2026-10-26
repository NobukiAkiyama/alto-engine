// Explicit opt-in benchmark of the shipped SDK, document edits, M5 tiles and GPU completion.
// It deliberately uses the same public calls on the before and after packages.
import { unstable } from "../../packages/alto-sdk/dist/unstable.js";

export async function runDenseProduction(engine) {
  const doc = await engine.createDocument({ width: 512, height: 512 });
  engine.viewport.set({ center: [256, 256], zoom: 1, rotation: 0 });
  await unstable(engine).settle();
  const shapes = await Promise.all(Array.from({ length: 5 }, (_, i) =>
    engine.vectors.parseSvg(`M${100 + i} 100Q300 20 400 300L100 400Z`)));
  const paths = Array.from({ length: 1000 }, (_, i) => ({
    data: shapes[i % shapes.length], fill: [0.2, 0.4, 0.8, 0.002],
    stroke: { color: [0.1, 0.5, 0.2, 0.5], width: 1.5, cap: "round", join: "round", miter_limit: 4 },
    fill_rule: "non_zero",
  }));
  // The document API allows at most 128 paths per layer. Keep the 1,000-path
  // document valid while making every source layer dense and every edit unique.
  const groups = Array.from({ length: 8 }, (_, i) => paths.slice(i * 125, (i + 1) * 125));
  const layers = [];
  const coldStart = performance.now();
  await doc.transact("dense source", tx => {
    for (const group of groups) layers.push(tx.addVector(group));
  });
  await unstable(engine).settle();
  const cold = performance.now() - coldStart;
  const samples = [];
  const colors = [];
  const beforeHash = await doc.snapshotHash();
  for (let i = 0; i < 23; i++) {
    // A unique style each time prevents an older content-hash tile from satisfying the edit.
    for (const group of groups) {
      group[124] = { ...group[124], fill: [i % 2, 0.3 + i * 0.001, 0.8, 0.25] };
    }
    const start = performance.now();
    await doc.transact("dense recolor", tx => {
      for (let j = 0; j < layers.length; j++) tx.setVector(layers[j], groups[j]);
    });
    await unstable(engine).settle();
    if (i >= 3) samples.push(performance.now() - start);
    // Pixel checks and readback are outside the measured edit-to-complete interval.
    if (i >= 21) {
      const tile = await unstable(engine).readTile(0, 0);
      colors.push(Array.from(tile.slice((250 * 256 + 250) * 4, (250 * 256 + 250) * 4 + 4)));
    }
  }
  const pass = (await doc.snapshotHash()) !== beforeHash && colors.length === 2 &&
    Math.abs(colors[0][0] - colors[1][0]) > 30;
  const sorted = [...samples].sort((a, b) => a - b);
  const report = {
    date: new Date().toISOString(), userAgent: navigator.userAgent,
    paths: paths.length, vectorLayers: layers.length, pathsPerLayer: 125,
    warmupEdits: 3, measuredEdits: samples.length,
    viewport: { width: innerWidth, height: innerHeight, dpr: devicePixelRatio, center: [256, 256], zoom: 1 },
    sceneColdCommitGpuCompleteMs: cold,
    medianEditGpuCompleteMs: sorted[Math.floor(sorted.length / 2)],
    p95EditGpuCompleteMs: sorted[Math.ceil(sorted.length * 0.95) - 1], samplesMs: samples,
    colors, results: [{ name: "dense SDK edits produce updated GPU pixels", pass }],
    notes: ["Includes document commit, SDK/Worker scheduling, visible M5 tiles and GPU completion.",
      "Readback is outside timings; not compositor presentation or whole-application startup.",
      "Single-profile before/after observation; use the separate five-profile renderer benchmark for adoption."],
  };
  document.querySelector("#test-results").dataset.report = JSON.stringify(report);
  if (!pass) throw Error("dense SDK edit did not change the sampled GPU pixels");
  return report;
}
