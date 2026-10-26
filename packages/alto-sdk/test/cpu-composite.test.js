import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { RENDER_VERSION_F32, fnv64, parityCases, renderCase } from "../../../tests/e2e/cpu-composite.js";

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

test("opaque blue under half-red normal blend is the known pixel", () => {
  const tile = renderCase(parityCases().find((item) => item.name === "blend-normal"));
  assert.deepEqual([tile[0], tile[1], tile[2], tile[3]], [128, 0, 127, 255]);
  const level = renderCase(parityCases().find((item) => item.name === "level1-centers"));
  assert.deepEqual([level[0], level[1], level[2], level[3]], [1, 1, 8, 255]);
});
