import assert from "node:assert/strict";
import test from "node:test";
import { DocumentHandle } from "../dist/index.js";

const identity = { in_black: 0, in_white: 1, gamma: 1, out_black: 0, out_white: 1 };
function histogram(entries) {
  const bins = Array(256).fill(0);
  for (const [value, count] of entries) bins[value] = count;
  return bins;
}
function document(channels, alpha = [[255, 1]]) {
  const state = { revision: 0, document: { root: "root", nodes: {}, selection: null, meta: {} } };
  return new DocumentHandle({
    requestForDocument: async (_doc, type, args) => {
      assert.equal(type, "imageHistogram");
      assert.deepEqual(args, { node: "raster" });
      const [r, g, b] = channels.map(histogram);
      return { r, g, b, a: histogram(alpha), pixels: r.reduce((sum, count) => sum + count, 0) };
    },
  }, state);
}

test("autoLevels clips histogram tails and shares the outer RGB endpoints", async () => {
  const doc = document([
    [[1, 1], [40, 999], [180, 1000]],
    [[60, 1000], [200, 999], [255, 1]],
    [[70, 1000], [160, 1000]],
  ]);
  assert.deepEqual(await doc.effects.autoLevels("raster"), { ...identity, in_black: 40 / 255, in_white: 200 / 255 });
  assert.deepEqual(await doc.effects.autoLevels("raster", { clip: 0 }), { ...identity, in_black: 1 / 255 });
});

test("autoLevels excludes transparent black without changing the snapshot", async () => {
  const channel = [[0, 100], [64, 1], [192, 1]];
  const doc = document([channel, channel, channel], [[0, 100], [255, 2]]);
  const before = doc.getSnapshot();
  assert.deepEqual(await doc.effects.autoLevels("raster"), { ...identity, in_black: 64 / 255, in_white: 192 / 255 });
  assert.equal(doc.getSnapshot(), before);
});

test("autoLevels returns identity for flat grayscale and empty histograms", async () => {
  for (const [channel, alpha] of [[[[80, 5]], [[255, 5]]], [[[0, 5]], [[0, 5]]]]) {
    const doc = document([channel, channel, channel], alpha);
    assert.deepEqual(await doc.effects.autoLevels("raster"), identity);
  }
});

test("autoLevels returns identity for transparent pixels with retained RGB", async () => {
  const doc = document([[[0, 5]], [[50, 5]], [[150, 5]]], [[0, 5]]);
  assert.deepEqual(await doc.effects.autoLevels("raster"), identity);
});

test("autoLevels rejects invalid clip values before requesting a histogram", async () => {
  const doc = document([]);
  for (const clip of [-0.01, 0.101, NaN, Infinity, null]) {
    await assert.rejects(doc.effects.autoLevels("raster", { clip }), { code: "INVALID_ARGUMENT" });
  }
});
