import assert from "node:assert/strict";
import { test } from "node:test";
import { exportGrid, PngExport, readExportPixels } from "../dist/png-export.js";

test("each PNG worker pull supplies at most one tile-aligned band", async () => {
  const grid = exportGrid({ x0: 0, y0: 13, x1: 10, y1: 550 }, 1);
  const stream = new PngExport(grid);
  const bands = [];
  const read = async (region, level) => {
    assert.equal(level, 0);
    bands.push(region);
    return new Uint8ClampedArray((region.x1 - region.x0) * (region.y1 - region.y0) * 4);
  };
  const header = await stream.pull(read);
  assert.equal(String.fromCharCode(...header.subarray(12, 16)), "IHDR");
  assert.equal(bands.length, 0);
  let before;
  let last;
  for (;;) {
    before = bands.length;
    const chunk = await stream.pull(read);
    assert.ok(bands.length - before <= 1);
    if (chunk === null) break;
    if (chunk.length) last = chunk;
  }
  assert.deepEqual(bands.map(({ y0, y1 }) => [y0, y1]), [[13, 256], [256, 512], [512, 550]]);
  assert.equal(String.fromCharCode(...last.subarray(4, 8)), "IEND");
  await stream.close();
});

test("readback batches unpremultiply pixels across both tile axes", async () => {
  const calls = [];
  const got = await readExportPixels({ x0: 255, y0: 255, x1: 257, y1: 257 }, 3, 1, async (level, coords) => {
    assert.equal(level, 3);
    const [x, y] = coords;
    calls.push([x, y]);
    const bytes = new Uint8Array(256 * 256 * 4);
    for (let at = 0; at < bytes.length; at += 4) {
      bytes.set([32 * x, 32 * y, 0, 128], at);
    }
    return bytes;
  });
  assert.deepEqual(calls, [[0, 0], [1, 0], [0, 1], [1, 1]]);
  assert.deepEqual([...got], [0, 0, 0, 128, 64, 0, 0, 128, 0, 64, 0, 128, 64, 64, 0, 128]);
});

test("a worker close during readback cannot report a complete PNG", async () => {
  const stream = new PngExport(exportGrid({ x0: 0, y0: 0, x1: 1, y1: 1 }, 1));
  const started = Promise.withResolvers();
  const read = Promise.withResolvers();
  const getBand = () => {
    started.resolve();
    return read.promise;
  };
  await stream.pull(getBand);
  const pending = stream.pull(getBand);
  await started.promise;
  await stream.close();
  read.resolve(new Uint8ClampedArray(4));
  await assert.rejects(pending, /document-changed/);
});
