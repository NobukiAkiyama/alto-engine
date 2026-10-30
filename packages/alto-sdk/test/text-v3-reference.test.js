import assert from "node:assert/strict";
import { test } from "node:test";

import { atlasBandQ, decodeTextBake, glyphMapQ, placeQ, renderTextV3 } from "../../../tests/e2e/cpu-composite.js";
import { FIXED_IDENTITY } from "../../../tests/e2e/fixed-affine.js";

// The fixed values of doc/design/2026-10-28-v3-integer-text.md; Rust pins the
// same ones (`glyph_q8` tests, `text_q14` tests).

/** A little-endian ALTB bake: one placeholder-font outline, glyphs `[m, t, color]`. */
function bake(commands, upem, glyphs) {
  const out = [];
  const u8 = (v) => out.push(v & 255);
  const u16 = (v) => { u8(v); u8(v >> 8); };
  const u32 = (v) => { u16(v & 0xffff); u16(v >>> 16); };
  const i32 = (v) => u32(v >>> 0);
  const i64 = (v) => { const b = BigInt.asUintN(64, BigInt(v)); u32(Number(b & 0xffffffffn)); u32(Number(b >> 32n)); };
  "ALTB".split("").forEach((c) => u8(c.charCodeAt(0)));
  u16(1); u16(1); u32(0); u32(1);
  u32(0xffffffff); u16(0); u16(upem); u32(commands.length);
  for (const [tag, ...points] of commands) { u8(tag); points.flat().forEach(i32); }
  u32(glyphs.length);
  for (const [m, t, color] of glyphs) { u32(0); m.forEach(i32); t.forEach(i64); color.forEach(u16); }
  return Uint8Array.from(out);
}
const q6 = (v) => v * 64;
const square = (x0, y0, x1, y1) => [[0, [q6(x0), q6(y0)]], [1, [q6(x1), q6(y0)]], [1, [q6(x1), q6(y1)]], [1, [q6(x0), q6(y1)]], [4]];
const q24 = (v) => Math.round(v * 2 ** 24);
const q30 = (v) => BigInt(Math.round(v * 2 ** 30));
const alpha = (tile, x, y) => tile[(y * 256 + x) * 4 + 3];

test("v3 text reference: exact area, 3/4 coverage is 191", () => {
  // v2's case: a 1000-unit square at 12 px/em shifted by a quarter pixel.
  const m = [q24(0.012), 0, 0, q24(-0.012)];
  const small = bake(square(0, 0, 1000, 1000), 1000, [[m, [q30(2.25), q30(14.5)], [16384, 16384, 16384, 16384]]]);
  const tile = renderTextV3(small);
  assert.equal(alpha(tile, 2, 8), 191);
  assert.equal(alpha(tile, 5, 8), 255);
  let ink = 0;
  for (let i = 3; i < tile.length; i += 4) ink += tile[i];
  assert.ok(Math.abs(ink / 255 - 144) < 0.2, `ink ${ink / 255}`);
});

test("v3 text reference: place_q rounds negative positions away from zero", () => {
  const outline = decodeTextBake(bake(square(0, 0, 500, 500), 1000, [])).outlines[0];
  for (const [x, origin, phase] of [[-0.125, -1n, 3n], [-0.375, -1n, 2n], [0.125, 0n, 1n], [0, 0n, 0n]]) {
    const C = glyphMapQ(FIXED_IDENTITY, 0, [q24(0.012), 0, 0, q24(-0.012)].map(BigInt), [q30(x), q30(10)]);
    const p = placeQ(C, outline);
    assert.deepEqual([p.origin[0], p.phase[0]], [origin, phase], `x ${x}`);
  }
  // A degenerate band has no atlas placement.
  const flat = glyphMapQ(FIXED_IDENTITY, 0, [q24(0.012), 0, q24(0.024), 0].map(BigInt), [0n, 0n]);
  assert.equal(placeQ(flat, outline), null);
});

test("v3 text reference: the atlas band boundary is strict", () => {
  const diag = (v) => ({ m: [BigInt(v), 0n, 0n, BigInt(-v)], t: [0n, 0n] });
  assert.equal(atlasBandQ(diag(393216), 1024n), false);
  assert.equal(atlasBandQ(diag(393215), 1024n), true);
});

test("v3 text reference: decoding refuses every out-of-bound field", () => {
  const good = bake(square(0, 0, 10, 10), 1000, [[[q24(0.02), 0, 0, q24(0.02)], [0n, 0n], [0, 0, 0, 16384]]]);
  assert.ok(decodeTextBake(good));
  const corrupt = (at, bytes) => { const b = Uint8Array.from(good); b.set(bytes, at); return decodeTextBake(b); };
  assert.equal(corrupt(0, [0x58]), null, "magic");
  assert.equal(corrupt(4, [2, 0]), null, "version");
  assert.equal(decodeTextBake(Uint8Array.from([...good, 0])), null, "trailing byte");
  assert.equal(decodeTextBake(good.subarray(0, good.length - 1)), null, "truncated");
  const outline = 4 + 2 + 2 + 4 + 4;
  assert.equal(corrupt(outline, [7, 0, 0, 0]), null, "font index");
  assert.equal(corrupt(outline + 6, [0, 0]), null, "upem 0");
  assert.equal(corrupt(outline + 8, [0xa1, 0x0f, 0, 0]), null, "command count 4001");
  assert.equal(corrupt(outline + 13, [1, 0, 0x40, 0]), null, "coordinate 2^22 + 1");
  const color = good.length - 2;
  assert.equal(corrupt(color, [1, 0x40]), null, "colour 16385");
  // An undecodable bake draws nothing.
  assert.ok(renderTextV3(good.subarray(0, 8)).every((v) => v === 0));
});

test("v3 text reference: paint order and the direct band", () => {
  // Two 40 px squares (direct band), the second on top.
  const m = [671089, 0, 0, 671089];
  const two = bake(square(0, 0, 1000, 1000), 1000, [
    [m, [0n, 0n], [16384, 0, 0, 16384]],
    [m, [q30(10), 0n], [0, 0, 16384, 16384]],
  ]);
  const tile = renderTextV3(two);
  const at = (x, y) => Array.from(tile.subarray((y * 256 + x) * 4, (y * 256 + x) * 4 + 4));
  assert.deepEqual(at(5, 20), [255, 0, 0, 255]);
  assert.deepEqual(at(20, 20), [0, 0, 255, 255]);
  // The far tile of a 300 px square: the direct window is the glyph's box in it.
  const big = bake(square(0, 0, 1000, 1000), 1000, [[[q24(0.3), 0, 0, q24(0.3)], [q30(100), 0n], [0, 16384, 0, 16384]]]);
  const right = renderTextV3(big, undefined, [1, 0], [512, 512]);
  assert.equal(alpha(right, 0, 10), 255);
  assert.equal(alpha(right, 143, 10), 255);
  assert.equal(alpha(right, 144, 10), 0);
});
