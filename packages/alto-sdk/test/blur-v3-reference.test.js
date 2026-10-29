import assert from "node:assert/strict";
import { test } from "node:test";

import {
  blurCoarsenK,
  blurKernelQ16,
  blurPassQ14,
  blurRadiusQ8,
  blurRadiusQ8F32,
  renderBlurV3,
} from "../../../tests/e2e/cpu-composite.js";

// The fixed vectors of doc/design/2026-10-28-v3-integer-blur.md. Rust pins the
// same values (`blur_kernel_q16_matches_locked_vectors`, `blur_coarsen_k_exact`).

test("v3 blur kernel matches the memo's vectors", () => {
  assert.deepEqual(blurKernelQ16(1, 256), [64112, 712]);
  assert.deepEqual(blurKernelQ16(2, 256), [64112, 712, 0]);
  assert.deepEqual(blurKernelQ16(8, 2048), [9818, 9151, 7411, 5214, 3187, 1693, 781, 313, 109]);
  assert.deepEqual(
    blurKernelQ16(12, 3072),
    [6548, 6346, 5778, 4943, 3971, 2998, 2125, 1416, 886, 521, 288, 149, 73],
  );
  const w = blurKernelQ16(64, 16384);
  assert.deepEqual(w.slice(0, 4), [1228, 1228, 1223, 1216]);
  assert.deepEqual(w.slice(61), [21, 18, 15, 14]);
  assert.equal(w[0] + 2 * w.slice(1).reduce((a, b) => a + b, 0), 65536);
});

test("v3 blur radius decodes the stored bits to Q8 with ties up", () => {
  for (let k = 0; k <= 32768; k += 97) {
    const want = Math.ceil(k / 2);
    assert.equal(blurRadiusQ8(k / 512), want, `f64 ${k}/512`);
    assert.equal(blurRadiusQ8F32(k / 512), want, `f32 ${k}/512`);
  }
  assert.equal(blurRadiusQ8(1 / 512 - 2 ** -40), 0);
  assert.equal(blurRadiusQ8(1 / 512 + 2 ** -40), 1);
  assert.equal(blurRadiusQ8(-0), 0);
  assert.equal(blurRadiusQ8(Number.MIN_VALUE), 0);
  assert.equal(blurRadiusQ8(64), 16384);
  assert.equal(blurRadiusQ8F32(2 ** -149), 0);
  assert.equal(blurRadiusQ8F32(64), 16384);
});

test("v3 blur coarsening level comes from the exact f32", () => {
  const up = (x) => {
    const view = new DataView(new ArrayBuffer(4));
    view.setFloat32(0, x);
    view.setUint32(0, view.getUint32(0) + 1);
    return view.getFloat32(0);
  };
  for (const [radius, k] of [
    [2 ** -149, -155],
    [2 ** -126 - 2 ** -149, -132],
    [1 / 512, -15],
    [up(1), -5],
    [up(2), -4],
    [32.001, 0],
    [64, 0],
    [64.001, 1],
    [128, 1],
    [128.001, 2],
  ]) {
    assert.equal(blurCoarsenK(radius), k, String(radius));
  }
  assert.equal(blurCoarsenK(0), null);
  for (let n = -15; n <= 1; n++) {
    const x = 64 * 2 ** n;
    const down = (() => {
      const view = new DataView(new ArrayBuffer(4));
      view.setFloat32(0, x);
      view.setUint32(0, view.getUint32(0) - 1);
      return view.getFloat32(0);
    })();
    assert.equal(blurCoarsenK(x), n, `64*2^${n}`);
    assert.equal(blurCoarsenK(down), n, `below 64*2^${n}`);
    assert.equal(blurCoarsenK(up(x)), n + 1, `above 64*2^${n}`);
  }
});

test("v3 blur passes are horizontal first and keep a constant plane constant", () => {
  const w = blurKernelQ16(1, 256);
  const rows = [[13576, 5669, 12029], [12278, 2833, 14383], [3536, 5364, 12886]];
  const plane = rows.flat().map((v) => [v, v, v, v]);
  const hFirst = blurPassQ14(blurPassQ14(plane, 3, 3, w, false), 3, 3, w, true);
  const vFirst = blurPassQ14(blurPassQ14(plane, 3, 3, w, true), 3, 3, w, false);
  assert.equal(hFirst[4][0], 3117);
  assert.equal(vFirst[4][0], 3116);
  // An opaque layer far wider than the halo stays opaque in its middle.
  const image = { w: 256, h: 256, pixels: new Uint8Array(256 * 256 * 4).fill(255) };
  const tile = renderBlurV3(image, [12]);
  assert.deepEqual(Array.from(tile.subarray((128 * 256 + 128) * 4, (128 * 256 + 128) * 4 + 4)), [255, 255, 255, 255]);
  assert.ok(tile[3] < 255, "the corner fades into the transparent outside");
});

test("v3 blur reference refuses what Rust refuses and clips at the canvas origin", () => {
  for (const radius of [-1, 65, NaN, Infinity]) assert.throws(() => blurRadiusQ8(radius), RangeError, String(radius));
  assert.throws(() => blurRadiusQ8F32(-1), RangeError);
  for (const rf of [-1, NaN, Infinity]) assert.equal(blurCoarsenK(rf), null, String(rf));
  assert.equal(blurCoarsenK(-0), null);
  for (const [r, rho8] of [[0, 256], [65, 16384], [64, 16385], [1.5, 256]]) {
    assert.throws(() => blurKernelQ16(r, rho8), RangeError, `(${r}, ${rho8})`);
  }
  const image = { w: 256, h: 256, pixels: new Uint8Array(256 * 256 * 4).fill(255) };
  assert.ok(renderBlurV3(image, [8], [-1, 0]).every((v) => v === 0));
  assert.ok(renderBlurV3(image, [8], [0, -1]).every((v) => v === 0));
});
