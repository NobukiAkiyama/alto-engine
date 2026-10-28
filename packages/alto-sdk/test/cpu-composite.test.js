import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { RENDER_VERSION_F32, colorAdjustGradient, colorAdjustGrid, fnv64, parityCases, renderCase, renderColorAdjust, renderColorAdjustV3, solid, sourceAxis, downsampleQ14 } from "../../../tests/e2e/cpu-composite.js";

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

test("color-adjust gradient covers every channel and the alpha anchors", () => {
  const image = colorAdjustGradient();
  const hit = [new Set(), new Set(), new Set(), new Set()];
  for (let i = 0; i < image.pixels.length; i += 4) {
    for (let channel = 0; channel < 4; channel++) hit[channel].add(image.pixels[i + channel]);
  }
  for (const channel of hit) assert.equal(channel.size, 256);
  assert.equal(image.pixels[3], 0);
  assert.equal(image.pixels[256 * 4 + 3], 1);
  assert.equal(image.pixels[2 * 256 * 4 + 3], 128);
  assert.equal(image.pixels[3 * 256 * 4 + 3], 254);
  assert.equal(image.pixels[(4 * 256 + 255) * 4 + 3], 255);
  const anchor = image.pixels.slice(3 * 256 * 4, 3 * 256 * 4 + 4);
  const flat = renderColorAdjust({ w: 1, h: 1, pixels: anchor }, { brightness: 0, contrast: 1, saturation: 1 });
  const gray = renderColorAdjust({ w: 1, h: 1, pixels: anchor }, { brightness: -0.35, contrast: 1.7, saturation: 0 });
  const vivid = renderColorAdjust({ w: 1, h: 1, pixels: anchor }, { brightness: 0.45, contrast: 0.5, saturation: 1.6 });
  assert.notDeepEqual([...flat], [...gray]);
  assert.notDeepEqual([...flat], [...vivid]);
  assert.notDeepEqual([...gray], [...vivid]);
  const clear = renderColorAdjust({ w: 1, h: 1, pixels: Uint8Array.of(40, 80, 120, 0) }, { brightness: 0.45, contrast: 1.7, saturation: 1.6 });
  assert.deepEqual([...clear], [0, 0, 0, 0]);
  assert.equal(colorAdjustGrid().length, 20);
  const imageFull = colorAdjustGradient();
  const interior = colorAdjustGrid().slice(0, 12);
  const expected = [
    "09de5f49e7068fbe",
    "d296346794223ecf",
    "0a0b6c96d748999c",
    "e80976ac9139b99d",
    "5926c4243a96f79f",
    "6e6fc79ca84727ab",
    "beb375818b446960",
    "8dd8cd53b2a57140",
    "54e47915c5a04b18",
    "9be25cf48c61da5a",
    "c0cc9de8851d97d2",
    "eaae1596e5e99a2b",
  ];
  interior.forEach((settings, index) => {
    assert.equal(fnv64(renderColorAdjust(imageFull, settings)), expected[index], `${settings.contrastLabel} ${settings.saturationLabel} ${settings.brightnessLabel}`);
  });
  const exact = [
    "85427e72aa6975f4",
    "d34d9def32069e30",
    "6fabe3fd4a1d755d",
    "03bdc46a2a94022a",
    "e2631093ebe7c14b",
    "6614a1c612c0eb35",
    "35828221f6e46782",
    "f2d63e24deb38bb5",
    "e21500eb040c8e1d",
    "54f48aae0e383c44",
    "8e735cc19d43f13b",
    "2edfc3149add3c41",
    // Contrast 0 flattens every channel, so saturation does not move a pixel.
    "204b8da3774c1b25",
    "455fe98699531725",
    "204b8da3774c1b25",
    "455fe98699531725",
    "3696fac022da0187",
    "a719b6b1ac5d2588",
    "e3e29d265599c372",
    "db4228c4143ca9ff",
  ];
  colorAdjustGrid().forEach((settings, index) => {
    assert.equal(fnv64(renderColorAdjustV3(imageFull, settings)), exact[index], `v3 ${settings.contrastLabel} ${settings.saturationLabel} ${settings.brightnessLabel}`);
  });
});

test("opaque blue under half-red normal blend is the known pixel", () => {
  const tile = renderCase(parityCases().find((item) => item.name === "blend-normal"));
  assert.deepEqual([tile[0], tile[1], tile[2], tile[3]], [128, 0, 127, 255]);
  const level = renderCase(parityCases().find((item) => item.name === "level1-centers"));
  assert.deepEqual([level[0], level[1], level[2], level[3]], [1, 1, 8, 255]);
});

test("v3 compositing resolves the former blend-color and half-opacity boundaries", async () => {
  const { alphaMeasureCases, compositeQ14Blend } = await import("../../../tests/e2e/cpu-composite.js");
  const cases = new Map(alphaMeasureCases().map((c) => [c.name, c]));
  for (const name of ["blend-color", "opacity-half", "nested-groups"]) {
    const tile = renderCase(cases.get(`${name}-alpha-measure`), 3);
    for (let i = 0; i < tile.length; i += 4) {
      assert.ok(tile[i] <= tile[i + 3] && tile[i + 1] <= tile[i + 3] && tile[i + 2] <= tile[i + 3], `${name} premultiplied at ${i / 4}`);
    }
    if (name === "blend-color") {
      const i = (82 * 256 + 181) * 4;
      assert.deepEqual([...tile.slice(i, i + 4)], [105, 107, 156, 255]);
    }
  }
  // Half-away division, including the premultiplied normal path.
  assert.deepEqual(compositeQ14Blend([0n, 0n, 0n, 0n], [16384n, 0n, 0n], [8192n, 0n, 0n], 8192n, 0), [8192n, 0n, 0n, 8192n]);
});

test("v3 isolates from document f32 opacity before Q14 quantization", () => {
  const raster = (rgba, mode = 0) => ({ type: "raster", image: solid(1, 1, rgba), visible: true, opacity: 1, mode, transform: [1, 0, 0, 1, 0, 0] });
  const scene = (opacity, isolated = false) => ({ w: 1, h: 1, tx: 0, ty: 0, level: 0, nodes: [
    raster([100, 150, 200, 255]),
    { type: "group", opacity, isolated, visible: true, mode: 0, transform: [1, 0, 0, 1, 0, 0], children: [raster([200, 100, 50, 255], 1)] },
  ] });
  const pixel = (s) => [...renderCase(s, 3).slice(0, 4)];
  assert.deepEqual(pixel(scene(1 - 2 ** -24)), pixel(scene(1, true)));
  assert.deepEqual(pixel(scene(1 - 1e-9)), pixel(scene(1)));
  assert.notDeepEqual(pixel(scene(1)), pixel(scene(1, true)));
});

test("v3 rejects unspecified modes before drawing, including hidden nodes", () => {
  const base=parityCases().find(c=>c.name==="blend-normal");
  for(const mode of [17,18,255]) {
    const nodes=[...base.nodes,{...base.nodes[0],mode,visible:false}];
    assert.throws(()=>renderCase({...base,nodes},3),/unspecified blend mode/);
  }
});

test("v3 integer pyramid rounds ties upward and preserves premultiplication across levels", () => {
  const child=new Uint8Array(256*256*4);
  for(let y=0;y<256;y++) for(let x=0;x<256;x++) {
    const i=(y*256+x)*4;
    child.set([x%2,y%2,(x+y)%2,1],i);
  }
  const level1=downsampleQ14([child,child,child,child]);
  const level2=downsampleQ14([level1,level1,level1,level1]);
  assert(level1.every(v=>v===1));
  assert.deepEqual(level2,level1);
});
