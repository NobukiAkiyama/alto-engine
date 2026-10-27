import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { RENDER_VERSION_F32, fnv64, parityCases, renderCase, renderColorAdjust, solid, sourceAxis } from "../../../tests/e2e/cpu-composite.js";

test("render version 2 matches the locked CPU reference hashes", () => {
  const lines = readFileSync(new URL("../../../tests/parity/cpu-reference.txt", import.meta.url), "utf8")
    .split("\n")
    .filter((line) => line && !line.startsWith("#"));
  const cases = new Map(parityCases().filter((item) => item.lock).map((item) => [item.name, item]));
  assert.equal(lines.length, cases.size);
  for (const line of lines) {
    const [name, , fnv] = line.split(" ");
    const tile = renderCase(cases.get(name));
    assert.equal(fnv64(tile), fnv, name);
  }
});

test("render version 1 matches the hashes recorded on main", () => {
  const lines = readFileSync(new URL("../../../tests/parity/render-v1.txt", import.meta.url), "utf8")
    .split("\n")
    .filter((line) => line && !line.startsWith("#"));
  const cases = new Map(parityCases().map((item) => [item.name, item]));
  assert.equal(lines.length, 34);
  for (const line of lines) {
    const [name, , fnv] = line.split(" ");
    const tile = renderCase(cases.get(name), RENDER_VERSION_F32);
    assert.equal(fnv64(tile), fnv, name);
  }
});

test("a cancelling Q24 sum past 2^53 matches the integer coordinate", () => {
  // m0*px is about 2^62. f64 rounds that product, so the cancelled sum is 32768
  // and the Q8 coordinate is 1. The exact sum is 32735 and the coordinate is 0.
  const m0 = 2147483647;
  const px = 2000000033;
  const translation = -16777216269011436n;
  const f64Sum = m0 * px + Number(translation) * 256;
  assert.equal(f64Sum, 32768);
  assert.equal(Math.floor((f64Sum + 32768) / 65536), 1);
  assert.equal(sourceAxis(m0, px, 0, 0, translation), 0);
  // The rounded coordinate is 2^31, which does not fit in i32: transparent.
  assert.equal(sourceAxis(0, 0, 0, 0, 1n << 39n), null);
  // translation*256 does not fit in i64.
  assert.equal(sourceAxis(0, 0, 0, 0, (1n << 63n) - 1n), null);
});

test("color-adjust of opaque 40,80,120 at brightness 0.6 is the known pixel", () => {
  const tile = renderColorAdjust(solid(256, 256, [40, 80, 120, 255]), { brightness: 0.6 });
  assert.equal(tile.length, 262144);
  assert.deepEqual([tile[0], tile[1], tile[2], tile[3]], [193, 233, 255, 255]);
  assert.ok(tile.every((value, index) => value === tile[index % 4]));
});

test("opaque blue under half-red normal blend is the known pixel", () => {
  const tile = renderCase(parityCases().find((item) => item.name === "blend-normal"));
  assert.deepEqual([tile[0], tile[1], tile[2], tile[3]], [128, 0, 127, 255]);
  const level = renderCase(parityCases().find((item) => item.name === "level1-centers"));
  assert.deepEqual([level[0], level[1], level[2], level[3]], [1, 1, 8, 255]);
});
