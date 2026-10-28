// The v3 real-renderer cases and the host's Q14 quantizers in JavaScript, for
// `cpu_composite::renderer_rust_and_js_references_agree`. stdin is JSON from
// the Rust test: `cases` (`renderer_cases` that draw, tiles as seeds, masks
// with their document transform after linking) and `values` (f32 bit patterns).
// Output: for each case its RGBA8 tile (`renderM5V3` at the tile's ROI, pixels
// whose center is off the canvas transparent, level 1 as the integer pyramid of
// its four level-0 tiles), then for each value `opacityQ14` and
// `colorAdjustQ14` as little-endian i32.
import { colorAdjustGradient, colorAdjustQ14, downsampleQ14, opacityQ14, renderM5V3 } from "./cpu-composite.js";

const gradient = colorAdjustGradient().pixels;
const wrap = (v) => ((v % 256) + 256) % 256;
// Rust `raster_tile_bytes`: the gradient moved by `seed` rows and columns.
function rasterTile(seed, opaque) {
  const out = new Uint8Array(256 * 256 * 4);
  for (let y = 0; y < 256; y++) for (let x = 0; x < 256; x++) {
    const from = (wrap(y + seed * 37) * 256 + wrap(x + seed * 11)) * 4;
    out.set(gradient.subarray(from, from + 4), (y * 256 + x) * 4);
    if (opaque) out[(y * 256 + x) * 4 + 3] = 255;
  }
  return out;
}
// Rust `mask_tile_bytes`.
function maskTile(seed) {
  const out = new Uint8Array(256 * 256);
  for (let i = 0; i < out.length; i++) out[i] = ((((i % 256) * 7 + Math.floor(i / 256) * 13 + seed * 31) % 200) + 200) % 200;
  return out;
}
function tile([kind, seed]) {
  if (kind === "raster") return rasterTile(seed, false);
  if (kind === "opaque") return rasterTile(seed, true);
  if (kind === "mask") return maskTile(seed);
  if (kind === "white") return new Uint8Array(256 * 256).fill(255);
  throw new Error(`unknown tile ${kind}`);
}
const tiles = (list) => list.map(([key, spec]) => ({ key, bytes: tile(spec) }));
const mask = (m) => m && { transform: m.transform, tiles: tiles(m.tiles) };
const settings = ([contrast, saturation, brightness]) => ({ contrast, saturation, brightness });

function layers(spec) {
  return spec.map((l) => {
    if (l.adjustment) return { adjust: settings(l.adjustment), opacity: l.opacity, mode: l.blend, mask: mask(l.mask) };
    const effects = l.adjust ? [{ ...settings(l.adjust), selection: mask(l.selection) }] : [];
    return { tiles: tiles(l.tiles), transform: l.transform, effects, mask: mask(l.mask), opacity: l.opacity, mode: l.blend };
  });
}

function levelZero(stack, key, canvas) {
  const out = renderM5V3(stack, 256, 256, [key[0] * 256, key[1] * 256], 1);
  const on = (o, i, end) => 2 * (o + i) + 1 >= 0 && 2 * (o + i) + 1 < 2 * end;
  for (let y = 0; y < 256; y++) for (let x = 0; x < 256; x++) {
    if (!on(key[0] * 256, x, canvas[0]) || !on(key[1] * 256, y, canvas[1])) out.fill(0, (y * 256 + x) * 4, (y * 256 + x) * 4 + 4);
  }
  return out;
}

const input = JSON.parse(await new Promise((resolve) => {
  const chunks = [];
  process.stdin.on("data", (c) => chunks.push(c));
  process.stdin.on("end", () => resolve(Buffer.concat(chunks).toString()));
}));
const chunks = [];
for (const c of input.cases) {
  const stack = layers(c.layers);
  if (c.level === 0) chunks.push(Buffer.from(levelZero(stack, c.key, c.canvas)));
  else {
    const children = [0, 1, 2, 3].map((i) => levelZero(stack, [c.key[0] * 2 + (i % 2), c.key[1] * 2 + Math.floor(i / 2)], c.canvas));
    chunks.push(Buffer.from(downsampleQ14(children)));
  }
}
const f32 = new Float32Array(1);
const bits = new Uint32Array(f32.buffer);
const out = new Int32Array(input.values.length * 2);
input.values.forEach((b, i) => {
  bits[0] = b;
  out[i * 2] = opacityQ14(f32[0]);
  out[i * 2 + 1] = colorAdjustQ14(f32[0]);
});
chunks.push(Buffer.from(out.buffer));
process.stdout.write(Buffer.concat(chunks));
