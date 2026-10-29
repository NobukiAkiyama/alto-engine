import assert from "node:assert/strict";
import { test } from "node:test";

import { renderVectorV3 } from "../../../tests/e2e/cpu-composite.js";

// The hand-computed values of doc/design/2026-10-28-v3-integer-vector-fill.md
// ("vector_even_odd_shared_edge"); Rust pins the same bytes in `raster_q8.rs`.

const rect = (x0, y0, x1, y1) => [
  { op: "move", x: x0, y: y0 },
  { op: "line", x: x1, y: y0 },
  { op: "line", x: x1, y: y1 },
  { op: "line", x: x0, y: y1 },
  { op: "close" },
];
const white = (commands, rule = "non_zero") => [{ data: { commands }, fill: [1, 1, 1, 1], stroke: null, fill_rule: rule }];
const alpha = (tile, x, y) => tile[(y * 256 + x) * 4 + 3];

test("v3 vector reference: shared edges leave no halo", () => {
  const square = rect(32.5, 32.5, 200.5, 200.5);
  // (a) The same square twice, same direction.
  const twiceEven = renderVectorV3({ paths: white([...square, ...square], "even_odd") });
  assert.ok(twiceEven.every((v) => v === 0));
  const twice = renderVectorV3({ paths: white([...square, ...square]) });
  assert.equal(alpha(twice, 100, 100), 255);
  assert.equal(alpha(twice, 100, 32), 255);
  for (const [x, y] of [[32, 32], [200, 32], [32, 200], [200, 200]]) assert.equal(alpha(twice, x, y), 128);
  // (b) A and B share the right, top and bottom edges.
  const ab = [...square, ...rect(96.5, 32.5, 200.5, 200.5)];
  const even = renderVectorV3({ paths: white(ab, "even_odd") });
  for (let x = 33; x <= 95; x++) {
    assert.equal(alpha(even, x, 100), 255);
    assert.equal(alpha(even, x, 32), 128);
  }
  for (const x of [32, 96]) {
    assert.equal(alpha(even, x, 100), 128);
    assert.equal(alpha(even, x, 32), 64);
  }
  for (let x = 97; x <= 200; x++) assert.equal(alpha(even, x, 100), 0);
  const nonzero = renderVectorV3({ paths: white(ab) });
  assert.equal(alpha(nonzero, 32, 100), 128);
  assert.equal(alpha(nonzero, 96, 100), 255);
  assert.equal(alpha(nonzero, 200, 100), 255);
  // (c) Side by side: no seam.
  const pair = [...rect(32.5, 32.5, 96.5, 200.5), ...rect(96.5, 32.5, 160.5, 200.5)];
  for (const rule of ["non_zero", "even_odd"]) {
    const tile = renderVectorV3({ paths: white(pair, rule) });
    for (let y = 33; y <= 199; y++) assert.equal(alpha(tile, 96, y), 255);
    assert.equal(alpha(tile, 96, 32), 128);
    assert.equal(alpha(tile, 96, 200), 128);
  }
});

test("v3 vector reference: half-pixel edges and premultiplied paint", () => {
  // Red at alpha 0.5, edges at x = 32.5 and 96.5 (Rust `v3_vector_fill_reaches_metal…`).
  const tile = renderVectorV3({
    paths: [{ data: { commands: rect(32.5, 40, 96.5, 100) }, fill: [1, 0, 0, 0.5], stroke: null, fill_rule: "non_zero" }],
  });
  const at = (x, y) => Array.from(tile.subarray((y * 256 + x) * 4, (y * 256 + x) * 4 + 4));
  assert.deepEqual(at(0, 0), [0, 0, 0, 0]);
  assert.deepEqual(at(33, 50), [128, 0, 0, 128]);
  assert.deepEqual(at(32, 50), [64, 0, 0, 64]);
  assert.deepEqual(at(97, 50), [0, 0, 0, 0]);
});

test("v3 vector reference: raster transform, disabled masks and the canvas origin", () => {
  const image = { w: 256, h: 256, pixels: new Uint8Array(256 * 256 * 4) };
  for (let i = 0; i < 256; i++) image.pixels.set([255, 0, 0, 255], (i * 256 + 20) * 4);
  // A raster moved 10 px right reads its column 20 at pixel 30 (Q16 nearest lookup).
  const moved = renderVectorV3({ image, transform: [1, 0, 0, 1, 10, 0] });
  assert.equal(alpha(moved, 30, 5), 255);
  assert.equal(alpha(moved, 20, 5), 0);
  // A disabled vector mask or clip path is no mask: the raster is untouched.
  const off = { path: { commands: rect(0, 0, 1, 1) }, enabled: false };
  assert.deepEqual(renderVectorV3({ image, vectorMask: off, clipPath: off }), renderVectorV3({ image }));
  // An enabled one does cut it.
  const on = { path: { commands: rect(0, 0, 1, 1) }, enabled: true };
  assert.equal(alpha(renderVectorV3({ image, vectorMask: on }), 20, 5), 0);
  // Left of and above the canvas origin is transparent, as the resolve clips.
  const left = renderVectorV3({ paths: white(rect(-200, 10, -10, 100)) }, [-1, 0]);
  assert.ok(left.every((v) => v === 0));
  // A transform outside the fixed affine range is refused, as Rust refuses it.
  assert.throws(() => renderVectorV3({ paths: white(rect(0, 0, 1, 1)), transform: [1e20, 0, 0, 1, 0, 0] }), /fixed affine/);
});
