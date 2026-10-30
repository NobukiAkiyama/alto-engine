/**
 * Port of composite.wgsl and the Rust CPU reference in
 * crates/alto-gpu/src/cpu_composite.rs.
 *
 * Render version 1 keeps the f32 path: world matrices stay f64, each inverse
 * component is Math.fround, and bilinear is the f32 mix. Render version 2
 * (the default) uses the integer 4-tap from crates/alto-gpu/src/sample.rs:
 * Q16 coefficients, round half away from zero; Q8 positions; bilinear weights
 * in 0..255; channel bytes round half up with `(acc + 32768) >> 16`.
 * The Q24 sum is BigInt, the same checked i64 arithmetic as Rust. A term past
 * 2^53 that cancels to a small sum stays exact, and a rounded Q8 coordinate
 * outside i32 is transparent.
 * The browser suite compares version 2 bytes with production readTile output.
 */
import { FIXED_IDENTITY, f64Bits, fixedAffine, fixedConcat, fixedInverse, qFromF64Bits } from "./fixed-affine.js";

export const RENDER_VERSION_F32 = 1;
export const RENDER_VERSION = 2;
const f = Math.fround;
const EPS = f(0.000001);
const AXIS = f(0.0001);
const add = (a, b) => f(f(a) + f(b));
const sub = (a, b) => f(f(a) - f(b));
const mul = (a, b) => f(f(a) * f(b));
const div = (a, b) => f(f(a) / f(b));
const add3 = (a, b) => [add(a[0], b[0]), add(a[1], b[1]), add(a[2], b[2])];
const sub3 = (a, b) => [sub(a[0], b[0]), sub(a[1], b[1]), sub(a[2], b[2])];
const mul3 = (a, b) => [mul(a[0], b[0]), mul(a[1], b[1]), mul(a[2], b[2])];
const scale3 = (a, s) => [mul(a[0], s), mul(a[1], s), mul(a[2], s)];
const splat = (v) => [f(v), f(v), f(v)];
const clamp01 = (c) => c.map((v) => f(Math.min(f(1), Math.max(f(0), f(v)))));
const min3 = (c) => Math.min(c[0], c[1], c[2]);
const max3 = (c) => Math.max(c[0], c[1], c[2]);

function lum3(c) {
  return add(add(mul(c[0], f(0.2126)), mul(c[1], f(0.7152))), mul(c[2], f(0.0722)));
}
function clipColor(c) {
  const l = lum3(c);
  const n = min3(c);
  const x = max3(c);
  let o = c;
  if (n < 0) o = add3(splat(l), scale3(sub3(c, splat(l)), div(l, Math.max(sub(l, n), EPS))));
  if (x > 1) o = add3(splat(l), scale3(sub3(o, splat(l)), div(sub(f(1), l), Math.max(sub(x, l), EPS))));
  return clamp01(o);
}
function setLum(c, l) {
  return clipColor(add3(c, splat(sub(l, lum3(c)))));
}
function setSat(c, s) {
  const mn = min3(c);
  const mx = max3(c);
  if (mx <= mn) return [f(0), f(0), f(0)];
  return scale3(sub3(c, splat(mn)), div(s, Math.max(sub(mx, mn), EPS)));
}
function softChannel(ss, dd) {
  const d2 = dd <= f(0.25) ? mul(add(mul(sub(mul(f(16), dd), f(12)), dd), f(4)), dd) : f(Math.sqrt(dd));
  if (ss <= f(0.5)) return sub(dd, mul(mul(sub(f(1), mul(f(2), ss)), dd), sub(f(1), dd)));
  return add(dd, mul(sub(mul(f(2), ss), f(1)), sub(d2, dd)));
}
function dodge(ss, dd) {
  if (ss >= f(1)) return f(1);
  return Math.min(f(1), div(dd, Math.max(sub(f(1), ss), EPS)));
}
function burn(ss, dd) {
  if (ss <= f(0)) return f(0);
  return sub(f(1), Math.min(f(1), div(sub(f(1), dd), Math.max(ss, EPS))));
}
function select3(whenFalse, whenTrue, cond) {
  return [0, 1, 2].map((i) => (cond[i] ? whenTrue[i] : whenFalse[i]));
}
function mixColor(s, d, mode) {
  if (mode === 1) return mul3(d, s);
  if (mode === 2) return sub3(splat(1), mul3(sub3(splat(1), d), sub3(splat(1), s)));
  if (mode === 3) {
    return select3(
      sub3(splat(1), scale3(mul3(sub3(splat(1), s), sub3(splat(1), d)), f(2))),
      scale3(mul3(s, d), f(2)),
      [d[0] < f(0.5), d[1] < f(0.5), d[2] < f(0.5)],
    );
  }
  if (mode === 4) return [Math.min(s[0], d[0]), Math.min(s[1], d[1]), Math.min(s[2], d[2])];
  if (mode === 5) return [Math.max(s[0], d[0]), Math.max(s[1], d[1]), Math.max(s[2], d[2])];
  if (mode === 6) return [dodge(s[0], d[0]), dodge(s[1], d[1]), dodge(s[2], d[2])];
  if (mode === 7) return [burn(s[0], d[0]), burn(s[1], d[1]), burn(s[2], d[2])];
  if (mode === 8) {
    return select3(
      sub3(splat(1), scale3(mul3(sub3(splat(1), d), sub3(splat(1), s)), f(2))),
      scale3(mul3(d, s), f(2)),
      [s[0] < f(0.5), s[1] < f(0.5), s[2] < f(0.5)],
    );
  }
  if (mode === 9) return [softChannel(s[0], d[0]), softChannel(s[1], d[1]), softChannel(s[2], d[2])];
  if (mode === 10) return [f(Math.abs(sub(d[0], s[0]))), f(Math.abs(sub(d[1], s[1]))), f(Math.abs(sub(d[2], s[2])))];
  if (mode === 11) return sub3(add3(d, s), scale3(mul3(d, s), f(2)));
  if (mode === 12) return setLum(setSat(s, sub(max3(d), min3(d))), lum3(d));
  if (mode === 13) return setLum(setSat(d, sub(max3(s), min3(s))), lum3(d));
  if (mode === 14) return setLum(s, lum3(d));
  if (mode === 15) return setLum(d, lum3(s));
  if (mode === 16) return [Math.min(f(1), add(s[0], d[0])), Math.min(f(1), add(s[1], d[1])), Math.min(f(1), add(s[2], d[2]))];
  return s;
}
function blend(dst, src, mode) {
  const d = dst[3] > 0
    ? [div(dst[0], Math.max(dst[3], EPS)), div(dst[1], Math.max(dst[3], EPS)), div(dst[2], Math.max(dst[3], EPS))]
    : [f(0), f(0), f(0)];
  const b = mixColor([src[0], src[1], src[2]], d, mode);
  const rgb = [0, 1, 2].map((i) =>
    add(add(mul(sub(f(1), src[3]), dst[i]), mul(mul(sub(f(1), dst[3]), src[i]), src[3])), mul(mul(dst[3], src[3]), b[i])),
  );
  return [rgb[0], rgb[1], rgb[2], add(src[3], mul(dst[3], sub(f(1), src[3])))];
}

const IDENT = [1, 0, 0, 1, 0, 0];
const BLENDS = [
  "normal", "multiply", "screen", "overlay", "darken", "lighten", "color_dodge", "color_burn",
  "hard_light", "soft_light", "difference", "exclusion", "hue", "saturation", "color", "luminosity", "add",
];

export function solid(w, h, rgba) {
  const pixels = new Uint8Array(w * h * 4);
  for (let i = 0; i < pixels.length; i += 4) pixels.set(rgba, i);
  return { w, h, pixels };
}
/** Straight RGBA. R is x, G is y, B is (x*3+y*5)&255. Alpha is 0, 1, 128, 254 on the first four rows and x after that. */
export function colorAdjustGradient(w = 256, h = 256) {
  const pixels = new Uint8Array(w * h * 4);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const i = (y * w + x) * 4;
    pixels[i] = x & 255;
    pixels[i + 1] = y & 255;
    pixels[i + 2] = (x * 3 + y * 5) & 255;
    pixels[i + 3] = y === 0 ? 0 : y === 1 ? 1 : y === 2 ? 128 : y === 3 ? 254 : (x & 255);
  }
  return { w, h, pixels };
}

/**
 * Contrast, saturation, brightness. `Math.fround` is the JS form of the
 * document path's `f64 as f32`.
 */
function colorAdjustSettings(contrastValues, saturationValues, brightnessValues) {
  const grid = [];
  for (const contrastValue of contrastValues) {
    for (const saturationValue of saturationValues) {
      for (const brightnessValue of brightnessValues) {
        grid.push({
          contrast: f(contrastValue),
          saturation: f(saturationValue),
          brightness: f(brightnessValue),
          contrastLabel: String(contrastValue),
          saturationLabel: String(saturationValue),
          brightnessLabel: String(brightnessValue),
        });
      }
    }
  }
  return grid;
}

/**
 * Contrast, saturation, brightness. `Math.fround` is the JS form of the
 * document path's `f64 as f32`. The first 12 are the interior cross product.
 * The last 8 are the document-range endpoints.
 */
export function colorAdjustGrid() {
  return [
    ...colorAdjustSettings([0.5, 1.7], [0, 0.35, 1.6], [-0.35, 0.45]),
    ...colorAdjustSettings([0, 4], [0, 4], [-1, 1]),
  ];
}

export function ramp(w, h) {
  const pixels = new Uint8Array(w * h * 4);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const i = (y * w + x) * 4;
    pixels[i] = x & 255;
    pixels[i + 1] = y & 255;
    pixels[i + 2] = (x * 3 + y * 5) & 255;
    pixels[i + 3] = 255;
  }
  return { w, h, pixels };
}
function checker(w, h, a, b) {
  const pixels = new Uint8Array(w * h * 4);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    pixels.set((x + y) % 2 === 0 ? a : b, (y * w + x) * 4);
  }
  return { w, h, pixels };
}
function raster(image, mode = 0, opacity = 1, visible = true, transform = IDENT) {
  return { type: "raster", image, mode, opacity, visible, transform };
}
function group(isolated, opacity, children) {
  return { type: "group", isolated, opacity, mode: 0, visible: true, transform: IDENT, children };
}

export function parityCases() {
  const blue = solid(256, 256, [0, 0, 255, 255]);
  const redHalf = solid(256, 256, [255, 0, 0, 128]);
  const cases = BLENDS.map((name, mode) => ({
    name: `blend-${name}`, w: 256, h: 256, level: 0, tx: 0, ty: 0, lock: true,
    nodes: [raster(blue), raster(redHalf, mode)],
  }));
  const base = (name, nodes, extra = {}) => cases.push({
    name, w: 256, h: 256, level: 0, tx: 0, ty: 0, lock: true, nodes, ...extra,
  });
  base("opacity-half", [raster(blue), raster(solid(256, 256, [255, 0, 0, 255]), 0, 0.5)]);
  base("opacity-zero", [raster(blue), raster(solid(256, 256, [255, 0, 0, 255]), 0, 0)]);
  base("hidden-layer", [raster(blue), raster(solid(256, 256, [255, 0, 0, 255]), 0, 1, false)]);
  const stacked = [raster(solid(256, 256, [0, 255, 0, 128])), raster(redHalf)];
  base("isolated-group", [raster(blue), group(true, 1, stacked)]);
  base("pass-through-group", [raster(blue), group(false, 1, stacked)]);
  base("nested-groups", [raster(blue), group(true, 0.5, [group(false, 1, [raster(solid(256, 256, [255, 0, 0, 255]))])])]);
  const moved = [raster(ramp(256, 256), 0, 1, true, [1, 0, 0, 1, 200, 0])];
  base("translate-across-tile", moved, { w: 512, h: 256 });
  base("translate-across-tile-1", moved, { w: 512, h: 256, tx: 1 });
  base("half-pixel-translate", [raster(ramp(256, 256), 0, 1, true, [1, 0, 0, 1, 0.5, 0])]);
  base("scale-bilinear", [raster(ramp(256, 256), 0, 1, true, [2, 0, 0, 2, 0, 0])], { w: 512, h: 512 });
  base("extremes", [raster(checker(256, 256, [0, 0, 0, 255], [255, 255, 255, 255]))]);
  base("alpha-extremes", [raster(blue), raster(checker(256, 256, [255, 0, 0, 0], [255, 0, 0, 255]))]);
  base("two-partial", [raster(solid(256, 256, [0, 0, 255, 200])), raster(solid(256, 256, [255, 0, 0, 100]))]);
  const layers = [];
  for (let i = 0; i < 8; i++) layers.push(raster(solid(256, 256, [(i * 40) & 255, (i * 70) & 255, (i * 110) & 255, 80])));
  base("eight-layers", layers);
  const odd = [raster(solid(300, 100, [255, 0, 0, 255]))];
  base("odd-canvas-0", odd, { w: 300, h: 100 });
  base("odd-canvas-1", odd, { w: 300, h: 100, tx: 1 });
  base("level1-centers", [raster(ramp(512, 512))], { w: 512, h: 512, level: 1 });
  const angle = 0.3;
  const c = Math.cos(angle);
  const s = Math.sin(angle);
  // Q16 rounding absorbs an ulp between Math.cos and f64::cos, so rotation is locked.
  base("rotation", [raster(ramp(256, 256), 0, 1, true, [c, s, -s, c, 32, 24])], { w: 512, h: 512 });
  // Inverse translation is -40000px, which does not fit in i32 Q16.
  base("large-translation", [raster(ramp(256, 256), 0, 1, true, [0.1, 0, 0, 0.1, 4000, 0])], { w: 4096, h: 256, tx: 15 });
  // Inverse translation is -8400000px. The origin tile's left pixels have a
  // source Q8 past -2^23 px, so the whole 256px canvas is transparent.
  // Every source Q8 of the tile is past 2^23 px and reaches none of the layer's
  // tiles: transparent in every version (v3 skips the draw, DQ3-4).
  base("large-translation-out-of-range", [raster(ramp(256, 256), 0, 1, true, [0.01, 0, 0, 0.01, 84000, 0])]);
  return cases;
}

const ALPHA_SWEEP = [0, 1, 128, 254];

/**
 * Straight RGBA for one layer in paint order. Matches `alpha_sweep` in
 * crates/alto-gpu/src/cpu_composite.rs. Layer 0 is R = x&255, G = y&255,
 * B = (3x+5y)&255. Layer i rotates those channels by i%3 and adds 17*i.
 * Even layers step alpha with x+y; odd layers step with x. Both add the
 * layer index. Layers 0 and 1 cover all 16 pairs of {0, 1, 128, 254}.
 */
export function alphaSweep(w, h, layer = 0) {
  const pixels = new Uint8Array(w * h * 4);
  const rot = layer % 3;
  const shift = (layer * 17) & 255;
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const base = [x & 255, y & 255, (x * 3 + y * 5) & 255];
    const i = (y * w + x) * 4;
    pixels[i] = (base[rot % 3] + shift) & 255;
    pixels[i + 1] = (base[(1 + rot) % 3] + shift) & 255;
    pixels[i + 2] = (base[(2 + rot) % 3] + shift) & 255;
    const phase = layer % 2 === 0 ? (x + y + layer) : (x + layer);
    pixels[i + 3] = ALPHA_SWEEP[phase % 4];
  }
  return { w, h, pixels };
}

function paintAlphaSweep(nodes) {
  let layer = 0;
  const walk = (node) => {
    if (node.type === "group") return { ...node, children: node.children.map(walk) };
    const image = alphaSweep(node.image.w, node.image.h, layer);
    layer += 1;
    return { ...node, image };
  };
  return nodes.map(walk);
}

/** Blend stacks must cover all 16 alpha pairs and differ in RGB at every pixel. */
export function assertDistinctAlphaLayers(cases = alphaMeasureCases()) {
  const alphas = [0, 1, 128, 254];
  const want = alphas.flatMap((dst) => alphas.map((src) => `${dst},${src}`));
  let checked = 0;
  const walk = (nodes, images) => {
    for (const node of nodes) {
      if (node.type === "group") walk(node.children, images);
      else images.push(node.image);
    }
  };
  for (const testCase of cases) {
    const stacked = ["blend-", "two-partial", "eight-layers", "alpha-extremes"]
      .some((prefix) => testCase.name.startsWith(prefix));
    if (!stacked) continue;
    const images = [];
    walk(testCase.nodes, images);
    if (images.length < 2) throw new Error(`${testCase.name} should stack at least two layers`);
    for (let n = 0; n < images.length - 1; n++) {
      const dst = images[n];
      const src = images[n + 1];
      if (dst.w !== src.w || dst.h !== src.h) throw new Error(`${testCase.name} layer size differs`);
      const pairs = new Set();
      let same = 0;
      for (let i = 0; i < dst.pixels.length; i += 4) {
        if (dst.pixels[i] === src.pixels[i]
          && dst.pixels[i + 1] === src.pixels[i + 1]
          && dst.pixels[i + 2] === src.pixels[i + 2]) same += 1;
        pairs.add(`${dst.pixels[i + 3]},${src.pixels[i + 3]}`);
      }
      if (same !== 0) throw new Error(`${testCase.name} has pixels where src RGB equals dst`);
      for (const key of want) {
        if (!pairs.has(key)) throw new Error(`${testCase.name} missing dst,src alpha ${key}`);
      }
    }
    checked += 1;
  }
  if (checked < 16) throw new Error("blend sweeps were not checked");
}

/** Same geometry as parityCases. Measured, not enforced, and not a locked hash. */
export function alphaMeasureCases() {
  return parityCases().map((testCase) => ({
    ...testCase,
    name: `${testCase.name}-alpha-measure`,
    lock: false,
    measure: true,
    nodes: paintAlphaSweep(testCase.nodes),
  }));
}

function splitTiles(image) {
  const tiles = [];
  const nx = Math.ceil(image.w / 256);
  const ny = Math.ceil(image.h / 256);
  for (let ty = 0; ty < ny; ty++) for (let tx = 0; tx < nx; tx++) {
    const pixels = new Uint8Array(256 * 256 * 4);
    let ink = false;
    for (let y = 0; y < 256 && ty * 256 + y < image.h; y++) {
      for (let x = 0; x < 256 && tx * 256 + x < image.w; x++) {
        const s = ((ty * 256 + y) * image.w + (tx * 256 + x)) * 4;
        const d = (y * 256 + x) * 4;
        pixels.set(image.pixels.subarray(s, s + 4), d);
        ink ||= image.pixels[s + 3] !== 0;
      }
    }
    if (ink) tiles.push({ tx, ty, pixels });
  }
  tiles.sort((a, b) => a.tx - b.tx || a.ty - b.ty);
  return tiles;
}
function lookup(tiles, key) {
  let lo = 0;
  let hi = tiles.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    const t = tiles[mid];
    if (t.tx < key[0] || (t.tx === key[0] && t.ty < key[1])) lo = mid + 1;
    else hi = mid;
  }
  const t = tiles[lo];
  return t && t.tx === key[0] && t.ty === key[1] ? t.pixels : null;
}
function texel(tiles, q) {
  const key = [Math.floor(q[0] / 256), Math.floor(q[1] / 256)];
  const pixels = lookup(tiles, key);
  if (!pixels) return [f(0), f(0), f(0), f(0)];
  const localX = Math.min(255, Math.max(0, Math.floor(q[0]) - key[0] * 256));
  const localY = Math.min(255, Math.max(0, Math.floor(q[1]) - key[1] * 256));
  const i = (localY * 256 + localX) * 4;
  return [div(pixels[i], f(255)), div(pixels[i + 1], f(255)), div(pixels[i + 2], f(255)), div(pixels[i + 3], f(255))];
}
function fetchFloat(tiles, inv, p) {
  const q = [
    add(add(mul(inv[0], p[0]), mul(inv[2], p[1])), inv[4]),
    add(add(mul(inv[1], p[0]), mul(inv[3], p[1])), inv[5]),
  ];
  const axis = Math.abs(inv[1]) < AXIS && Math.abs(inv[2]) < AXIS;
  const unit = Math.abs(Math.abs(inv[0]) - 1) < AXIS && Math.abs(Math.abs(inv[3]) - 1) < AXIS;
  if (axis && unit) return texel(tiles, q);
  const base = [f(Math.floor(q[0])), f(Math.floor(q[1]))];
  const fx = sub(q[0], base[0]);
  const fy = sub(q[1], base[1]);
  const a = texel(tiles, base);
  const b = texel(tiles, [add(base[0], f(1)), base[1]]);
  const d = texel(tiles, [base[0], add(base[1], f(1))]);
  const e = texel(tiles, [add(base[0], f(1)), add(base[1], f(1))]);
  return [0, 1, 2, 3].map((i) => {
    const ab = add(mul(a[i], sub(f(1), fx)), mul(b[i], fx));
    const de = add(mul(d[i], sub(f(1), fx)), mul(e[i], fx));
    return add(mul(ab, sub(f(1), fy)), mul(de, fy));
  });
}
const F64_EXACT_INT = 9007199254740992;
/**
 * `x` rounded exactly half away from zero (Rust `f64::round`): the fraction
 * `a - floor(a)` is exact for `|a| < 2^53` (Sterbenz), so no sum is rounded
 * first. `sign * floor(|x| + 0.5)` is not exact: `0.5 - 2^-54` plus `0.5` is 1.
 */
export function roundHalfAway(x) {
  const a = Math.abs(x);
  const fl = Math.floor(a);
  const r = a - fl >= 0.5 ? fl + 1 : fl;
  // Q16 values are integers: no -0.
  return x < 0 && r !== 0 ? -r : r;
}
/** Q16 of render version 3 (Rust `sample::integer_inverse`), exact. */
export function quantizeQ16(v) {
  return quantizeQ16With(v, roundHalfAway);
}
/**
 * Q16 of render version 2 (Rust `sample::integer_inverse_v2`), its frozen
 * `floor(x + 0.5)` rounding, so v2 quantization and hashes never move.
 */
export function quantizeQ16V2(v) {
  return quantizeQ16With(v, (scaled) => (scaled >= 0 ? Math.floor(scaled + 0.5) : Math.ceil(scaled - 0.5)));
}
function quantizeQ16With(v, round) {
  if (!Number.isFinite(v)) return null;
  const scaled = v * 65536;
  if (!Number.isFinite(scaled) || Math.abs(scaled) >= F64_EXACT_INT) return null;
  return round(scaled);
}
function quantizeInverseV2(inv) {
  return quantizeInverseWith(inv, quantizeQ16V2);
}
function quantizeInverse(inv) {
  return quantizeInverseWith(inv, quantizeQ16);
}
function quantizeInverseWith(inv, quantizeQ16) {
  const m = inv.slice(0, 4).map(quantizeQ16);
  if (m.some((v) => v === null)) return null;
  for (const v of m) {
    if (v > 2147483647 || v < -2147483647) return null;
  }
  const tx = quantizeQ16(inv[4]);
  const ty = quantizeQ16(inv[5]);
  if (tx === null || ty === null) return null;
  return { m, tx, ty };
}
const I32_MIN = -2147483648;
const I32_MAX = 2147483647;
function fitsI32(v) {
  return Number.isSafeInteger(v) && v >= I32_MIN && v <= I32_MAX;
}
function pixelQ8(origin, index, scale) {
  const term = index * 256 + 128;
  const q = origin * 256 + term * scale;
  return fitsI32(q) ? q : null;
}
const I64_MIN = -(1n << 63n);
const I64_MAX = (1n << 63n) - 1n;
function asBig(v) {
  return typeof v === "bigint" ? v : BigInt(v);
}
function fitsI64(v) {
  return v >= I64_MIN && v <= I64_MAX;
}
function roundShr16(v) {
  if (!fitsI64(v)) return null;
  const half = 1n << 15n;
  const adjusted = v >= 0n ? v + half : v - half;
  if (!fitsI64(adjusted)) return null;
  return adjusted >> 16n;
}
/**
 * Q8 source coordinate for one axis. `m0`, `px`, `m1`, `py` are i32; `translation`
 * is i64 Q16. The sum is exact integer arithmetic. `null` when an i64 step
 * overflows or the rounded Q8 value does not fit in i32.
 */
export function sourceAxis(m0, px, m1, py, translation) {
  const a = asBig(m0) * asBig(px);
  const b = asBig(m1) * asBig(py);
  const t = asBig(translation) * 256n;
  if (!fitsI64(a) || !fitsI64(b) || !fitsI64(t)) return null;
  const ab = a + b;
  if (!fitsI64(ab)) return null;
  const sum = ab + t;
  if (!fitsI64(sum)) return null;
  const q = roundShr16(sum);
  if (q === null || q < BigInt(I32_MIN) || q > BigInt(I32_MAX)) return null;
  return Number(q);
}
function sourceQ8(q, px, py) {
  const x = sourceAxis(q.m[0], px, q.m[2], py, q.tx);
  const y = sourceAxis(q.m[1], px, q.m[3], py, q.ty);
  if (x === null || y === null) return null;
  return [x, y];
}
function floorFracQ8(q) {
  const base = Math.floor(q / 256);
  return [base, q - base * 256];
}
function sampleU8(tiles, x, y) {
  const key = [Math.floor(x / 256), Math.floor(y / 256)];
  const pixels = lookup(tiles, key);
  if (!pixels) return [0, 0, 0, 0];
  const localX = Math.min(255, Math.max(0, x - key[0] * 256));
  const localY = Math.min(255, Math.max(0, y - key[1] * 256));
  const i = (localY * 256 + localX) * 4;
  return [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
}
function bilinearByte(a, b, d, e, fx, fy) {
  const ab = a * (256 - fx) + b * fx;
  const de = d * (256 - fx) + e * fx;
  return (ab * (256 - fy) + de * fy + 32768) >> 16;
}
function fetchInteger(tiles, q, originPx, scale, x, y, bytes = false) {
  const clear = [f(0), f(0), f(0), f(0)];
  const px = pixelQ8(originPx[0], x, scale);
  const py = pixelQ8(originPx[1], y, scale);
  if (px === null || py === null) return clear;
  const pos = sourceQ8(q, px, py);
  if (!pos) return clear;
  const [ix, fx] = floorFracQ8(pos[0]);
  const [iy, fy] = floorFracQ8(pos[1]);
  const a = sampleU8(tiles, ix, iy);
  const b = sampleU8(tiles, ix + 1, iy);
  const d = sampleU8(tiles, ix, iy + 1);
  const e = sampleU8(tiles, ix + 1, iy + 1);
  return [0, 1, 2, 3].map((i) => { const value = bilinearByte(a[i], b[i], d[i], e[i], fx, fy); return bytes ? value : div(f(value), f(255)); });
}
function axisUnit(inv) {
  return Math.abs(inv[1]) < AXIS && Math.abs(inv[2]) < AXIS
    && Math.abs(Math.abs(inv[0]) - 1) < AXIS && Math.abs(Math.abs(inv[3]) - 1) < AXIS;
}
function fetch(tiles, inv, p, originPx, scale, x, y, version) {
  const inv32 = inv.map(f);
  const q = quantizeInverseV2(inv);
  if (version < RENDER_VERSION || axisUnit(inv32) || !q) return fetchFloat(tiles, inv32, p);
  return fetchInteger(tiles, q, originPx, scale, x, y);
}
function popSrc(accum, opacity) {
  const rgb = accum[3] > 0
    ? [div(accum[0], Math.max(accum[3], EPS)), div(accum[1], Math.max(accum[3], EPS)), div(accum[2], Math.max(accum[3], EPS))]
    : [f(0), f(0), f(0)];
  return [rgb[0], rgb[1], rgb[2], mul(accum[3], opacity)];
}
function quantize(v) {
  const x = mul(f(Math.min(f(1), Math.max(f(0), f(v)))), f(255));
  const floor = Math.floor(x);
  const diff = x - floor;
  if (diff > 0.5) return floor + 1;
  if (diff < 0.5) return floor;
  return floor % 2 === 0 ? floor : floor + 1;
}
function multiply(a, b) {
  return [
    a[0] * b[0] + a[2] * b[1],
    a[1] * b[0] + a[3] * b[1],
    a[0] * b[2] + a[2] * b[3],
    a[1] * b[2] + a[3] * b[3],
    a[0] * b[4] + a[2] * b[5] + a[4],
    a[1] * b[4] + a[3] * b[5] + a[5],
  ];
}
function inverseF64(m) {
  const det = m[0] * m[3] - m[1] * m[2];
  if (!Number.isFinite(det) || Math.abs(det) < 1e-12) return null;
  return [m[3] / det, -m[1] / det, -m[2] / det, m[0] / det, (m[2] * m[5] - m[3] * m[4]) / det, (m[1] * m[4] - m[0] * m[5]) / det];
}
function walk(node, parent, ops, version = RENDER_VERSION) {
  const opacity = version === 3 ? f(node.opacity) : node.opacity;
  if (!node.visible || opacity <= 0) return;
  const local = version === 3 ? fixedAffine(node.transform) : null;
  if (version === 3 && !local) throw Error("v3 transform out of range");
  const world = version === 3 ? fixedConcat(parent, local) : multiply(parent, node.transform);
  if (!world) throw Error("v3 composed transform out of range");
  if (node.type === "group") {
    const isolate = node.isolated || opacity < 1 || node.mode !== 0;
    if (isolate) ops.push({ t: "push" });
    for (const child of node.children) walk(child, world, ops, version);
    if (isolate) ops.push({ t: "pop", opacity: f(node.opacity), mode: node.mode });
  } else {
    const inv = version === 3 ? fixedInverse(world) : inverseF64(world);
    if (inv === undefined) throw Error("v3 transform has no Q16 inverse");
    if (inv === null) return;
    ops.push({ t: "draw", ...(version === 3 ? { q: inv } : { inv }), opacity: f(node.opacity), mode: node.mode, tiles: splitTiles(node.image) });
  }
}
function composite(ops, width, height, origin, scale, originPx, scaleI, version) {
  if (version === 3) return compositeInteger(ops, width, height, originPx, scaleI);
  const out = new Uint8Array(256 * 256 * 4);
  const w = f(width);
  const h = f(height);
  for (let y = 0; y < 256; y++) for (let x = 0; x < 256; x++) {
    const p = [add(origin[0], mul(add(f(x), f(0.5)), scale)), add(origin[1], mul(add(f(y), f(0.5)), scale))];
    const stack = [];
    let accum = [f(0), f(0), f(0), f(0)];
    if (p[0] >= 0 && p[1] >= 0 && p[0] < w && p[1] < h) {
      for (const op of ops) {
        if (op.t === "push") {
          stack.push(accum);
          accum = [f(0), f(0), f(0), f(0)];
        } else if (op.t === "pop") accum = blend(stack.pop(), popSrc(accum, op.opacity), op.mode);
        else {
          const s = fetch(op.tiles, op.inv, p, originPx, scaleI, x, y, version);
          accum = blend(accum, [s[0], s[1], s[2], mul(s[3], op.opacity)], op.mode);
        }
      }
    }
    const i = (y * 256 + x) * 4;
    for (let c = 0; c < 4; c++) out[i + c] = quantize(accum[c]);
  }
  return out;
}

export function renderCase(testCase, version = RENDER_VERSION) {
  if (version === 3) {
    const validate = (nodes) => { for (const node of nodes) {
      if (node.mode >= 17) throw new Error(`unspecified blend mode ${node.mode}`);
      if (node.children) validate(node.children);
    } };
    validate(testCase.nodes);
  }
  const ops = [];
  for (const node of testCase.nodes) walk(node, version === 3 ? FIXED_IDENTITY : IDENT, ops, version);
  const scaleI = 2 ** testCase.level;
  const scale = f(scaleI);
  const origin = [f(testCase.tx * 256 * scale), f(testCase.ty * 256 * scale)];
  const originPx = [testCase.tx * 256 * scaleI, testCase.ty * 256 * scaleI];
  return composite(ops, testCase.w, testCase.h, origin, scale, originPx, scaleI, version);
}

function f32ToF16Bits(value) {
  const buf = new ArrayBuffer(4);
  const f32 = new Float32Array(buf);
  const u32 = new Uint32Array(buf);
  f32[0] = value;
  const x = u32[0];
  const sign = (x >>> 16) & 0x8000;
  const mantissa = x & 0x007fffff;
  const exp = (x & 0x7f800000) >>> 23;
  if (exp === 255) {
    if (mantissa === 0) return sign | 0x7c00;
    const payload = mantissa >>> 13;
    return sign | 0x7c00 | payload | (payload === 0 ? 1 : 0);
  }
  const e = exp - 127 + 15;
  if (e <= 0) {
    if (e < -10) return sign;
    const m = mantissa | 0x00800000;
    const shift = 14 - e;
    let half = m >>> shift;
    const remMask = (1 << shift) - 1;
    const rem = m & remMask;
    const halfway = 1 << (shift - 1);
    if (rem > halfway || (rem === halfway && (half & 1) === 1)) half += 1;
    return (sign | half) & 0xffff;
  }
  if (e >= 31) return sign | 0x7c00;
  let half = (mantissa >>> 13) | (e << 10);
  const rem = mantissa & 0x1fff;
  if (rem > 0x1000 || (rem === 0x1000 && (half & 1) === 1)) half += 1;
  return (sign | half) & 0xffff;
}
function f16BitsToF32(half) {
  const sign = (half & 0x8000) << 16;
  const exp = (half >> 10) & 0x1f;
  const mant = half & 0x3ff;
  let bits;
  if (exp === 0) {
    if (mant === 0) bits = sign;
    else {
      let m = mant;
      let e = 113;
      while ((m & 0x400) === 0) {
        m <<= 1;
        e -= 1;
      }
      m &= 0x3ff;
      bits = (sign | (e << 23) | (m << 13)) >>> 0;
    }
  } else if (exp === 31) bits = (sign | 0x7f800000 | (mant << 13)) >>> 0;
  else bits = (sign | ((exp + (127 - 15)) << 23) | (mant << 13)) >>> 0;
  const buf = new ArrayBuffer(4);
  new Uint32Array(buf)[0] = bits >>> 0;
  return new Float32Array(buf)[0];
}
function f16(value) {
  return f16BitsToF32(f32ToF16Bits(value));
}
function unorm(byte) {
  return div(f(byte), f(255));
}
function colorAdjustTexel(rgba, brightness, contrast, saturation) {
  const straight = rgba.map(unorm);
  const premul = [0, 1, 2].map((i) => f16(mul(straight[i], straight[3]))).concat([f16(straight[3])]);
  const straightSrc = premul[3] > 0
    ? [div(premul[0], Math.max(premul[3], EPS)), div(premul[1], Math.max(premul[3], EPS)), div(premul[2], Math.max(premul[3], EPS))]
    : [f(0), f(0), f(0)];
  const blended = blend([f(0), f(0), f(0), f(0)], [straightSrc[0], straightSrc[1], straightSrc[2], premul[3]], 0).map(f16);
  const alpha = blended[3];
  const scale = Math.max(alpha, EPS);
  let rgb = [div(blended[0], scale), div(blended[1], scale), div(blended[2], scale)];
  rgb = rgb.map((channel) => add(add(mul(sub(channel, f(0.5)), contrast), f(0.5)), brightness));
  const luma = add(add(mul(rgb[0], f(0.2126)), mul(rgb[1], f(0.7152))), mul(rgb[2], f(0.0722)));
  rgb = rgb.map((channel) => add(mul(luma, sub(f(1), saturation)), mul(channel, saturation)));
  const stored = rgb.map((channel) => f16(mul(f(Math.min(f(1), Math.max(f(0), channel))), alpha)));
  return [...stored, f16(alpha)].map(quantize);
}

/** Mode 0, normal blend onto clear, mode 3, unorm resolve, one texel at a time. */
export function renderColorAdjust(image, settings = {}) {
  const brightness = f(settings.brightness ?? 0);
  const contrast = f(settings.contrast ?? 1);
  const saturation = f(settings.saturation ?? 1);
  const out = new Uint8Array(image.w * image.h * 4);
  for (let i = 0; i < out.length; i += 4) {
    out.set(colorAdjustTexel(
      [image.pixels[i], image.pixels[i + 1], image.pixels[i + 2], image.pixels[i + 3]],
      brightness,
      contrast,
      saturation,
    ), i);
  }
  return out;
}

const Q14_ONE = 16384;
const Q14_HALF = 8192;
const Q14_LUMA = [3483, 11718, 1183];

const F32_VALUE = new Float32Array(1);
const F32_BITS = new Uint32Array(F32_VALUE.buffer);
/**
 * `v * 16384` rounded half away from zero, exactly, for an f32 `v` with
 * `|v| <= 4` (Rust `q14_round`). Evaluated on the f32 bits in BigInt: the f32
 * sum with one half that a float rounding needs is not exact.
 */
export function q14Round(v) {
  F32_VALUE[0] = v;
  const bits = F32_BITS[0];
  const exponent = (bits >>> 23) & 0xff;
  const fraction = BigInt(bits & 0x7fffff);
  const [m, e] = exponent === 0 ? [fraction, -149] : [fraction | 0x800000n, exponent - 150];
  const shift = e + 14;
  let magnitude;
  if (shift >= 0) magnitude = m << BigInt(shift);
  else if (shift < -40) magnitude = 0n;
  else magnitude = (m + (1n << BigInt(-shift - 1))) >> BigInt(-shift);
  const q = Number(magnitude);
  return bits >>> 31 && q ? -q : q;
}

/** Q14 of a color-adjust setting. 1 is 16384. Clamped to [-1, 4], then `q14Round`. */
export function colorAdjustQ14(value) {
  if (!Number.isFinite(value)) return 0;
  return q14Round(f(Math.min(4, Math.max(-1, f(value)))));
}

/** Q14 of a layer opacity (Rust `composite_opacity_q14`): clamped to 0..1, then `q14Round`; NaN is 0. */
export function opacityQ14(value) {
  const v = f(value);
  if (Number.isNaN(v)) return 0;
  return q14Round(Math.min(1, Math.max(0, v)));
}

function byteQ14(byte) {
  return Math.trunc((byte * 16384 + 127) / 255);
}

function premulByteQ14(color, alpha) {
  return Math.trunc((color * alpha * 16384 + 32512) / 65025);
}

/** `(n + d/2) / d` in BigInt. Division truncates toward zero, matching `divQ`. */
function divQBig(n, d) {
  const half = d / 2n;
  if (n >= 0n) return (n + half) / d;
  return -(((-n) + half) / d);
}

function colorAdjustExactQ14(q, brightness, contrast, saturation) {
  const one = BigInt(Q14_ONE);
  const half = BigInt(Q14_HALF);
  const alpha = BigInt(q[3]);
  const contrastQ = BigInt(contrast);
  const brightnessQ = BigInt(brightness);
  const saturationQ = BigInt(saturation);
  const straight = [0n, 0n, 0n];
  if (alpha !== 0n) {
    for (let i = 0; i < 3; i++) {
      const s = (BigInt(q[i]) * one) / alpha;
      straight[i] = s < 0n ? 0n : s > one ? one : s;
    }
  }
  // Contrast is not clamped. Straight 0..=16384, contrast Q14 -16384..=65536
  // and brightness -16384..=16384 land in -40960..=57344. The saturation sum
  // luma*(16384-saturation)+channel*saturation is -5502926848..=5771362304
  // over that box. It fits BigInt and does not fit i32. The only clamp is
  // 0..=16384, after the mix.
  const rgb = straight.map((channel) => divQBig((channel - half) * contrastQ, one) + half + brightnessQ);
  const luma = divQBig(
    rgb[0] * BigInt(Q14_LUMA[0]) + rgb[1] * BigInt(Q14_LUMA[1]) + rgb[2] * BigInt(Q14_LUMA[2]),
    one,
  );
  const gap = one - saturationQ;
  return rgb.map((channel) => {
    const mixed = divQBig(luma * gap + channel * saturationQ, one);
    const clamped = mixed < 0n ? 0n : mixed > one ? one : mixed;
    return Number(divQBig(clamped * alpha, one));
  });
}

/** Q14 premultiplied ingress of one stored byte texel (`effects-q14.wgsl` mode 18). */
function ingressQ14(rgba) {
  const alpha = rgba[3];
  return [premulByteQ14(rgba[0], alpha), premulByteQ14(rgba[1], alpha), premulByteQ14(rgba[2], alpha), byteQ14(alpha)];
}

/** Mode 17 on premultiplied Q14; alpha passes through. */
function colorAdjustStageQ14(q, settings) {
  const rgb = colorAdjustExactQ14(
    q,
    colorAdjustQ14(f(settings.brightness ?? 0)),
    colorAdjustQ14(f(settings.contrast ?? 1)),
    colorAdjustQ14(f(settings.saturation ?? 1)),
  );
  return [...rgb, q[3]];
}

/** Resolve: `div_q(q * 255, 16384)` bytes, as the integer tile compositor. */
const resolveQ14 = (q) => q.map((v) => Math.min(255, Math.max(0, Number(divQBig(BigInt(v) * 255n, 16384n)))));

function colorAdjustTexelV3(rgba, brightness, contrast, saturation) {
  // Mode 18, then mode 19 at opacity 1 onto the empty backdrop (a Q14 copy), then 17.
  return resolveQ14(colorAdjustStageQ14(ingressQ14(rgba), { brightness, contrast, saturation }));
}

/** Render version 3, integer M5 stages: Q14 premultiply, Q14 color-adjust, byte resolve. */
export function renderColorAdjustV3(image, settings = {}) {
  const brightness = settings.brightness ?? 0;
  const contrast = settings.contrast ?? 1;
  const saturation = settings.saturation ?? 1;
  const out = new Uint8Array(image.w * image.h * 4);
  for (let i = 0; i < out.length; i += 4) {
    out.set(colorAdjustTexelV3(
      [image.pixels[i], image.pixels[i + 1], image.pixels[i + 2], image.pixels[i + 3]],
      brightness,
      contrast,
      saturation,
    ), i);
  }
  return out;
}

// v3 pixel masks (doc/design/2026-10-28-v3-integer-mask.md), `effects-q14.wgsl`
// modes 21 to 23. Every term is non-negative BigInt; `F = m * o` is Q28.
const Q28 = 1n << 28n;

/** Mode 21, layer mask and opacity: `div28(v * F)` on all four channels. */
export function maskLayerQ14(v, f) {
  return v.map((c) => divQBig(c * f, Q28));
}

/** Mode 22, effect selection: `div_q(in * (16384 - s) + f * s, 16384)`. */
export function maskSelectQ14(input, filtered, s) {
  return input.map((c, i) => divQBig(c * (CQ - s) + filtered[i] * s, CQ));
}

/**
 * Mode 23, adjustment-layer mask, opacity and blend: `div28(orig * (2^28 - F) + g * F)`.
 * `g` is the adjusted copy for normal; otherwise the straight product (screen for 2,
 * multiply for every other mode, as v2) premultiplied by the backdrop alpha.
 */
export function maskAdjustQ14(orig, filt, f, blend) {
  let g = filt;
  if (blend !== 0) {
    const d = cqStraight(orig);
    const t = cqStraight(filt);
    const color = d.map((v, i) => (blend === 2 ? CQ - cqDiv((CQ - v) * (CQ - t[i])) : cqDiv(v * t[i])));
    g = [...color.map((v) => cqDiv(v * orig[3])), orig[3]];
  }
  return orig.map((c, i) => divQBig(c * (Q28 - f) + g[i] * f, Q28));
}

/**
 * The stored texel a v3 stage reads at pixel (x, y) of a tile whose ROI has
 * origin `origin` and level scale `scale` (default: level 0 at the origin): the
 * raster of mode 18, or the mask of modes 21 to 23. `texelAt(x, y)` is the
 * stored texel, transparent outside its tiles. As the host
 * (`renderer::mask_sampler`), every lookup takes the Q16 inverse and Q8 pixel
 * centers, axis-aligned unit inverses included; there is no f32 lookup. A
 * transform without a Q16 inverse, or a ROI whose origin is not an i32 or whose
 * scale is not an i32 of at least 1, is refused (throws), as the host refuses
 * the render. A pixel without an i32 Q8 position is `null` (transparent).
 * Nearest; the texel is the floor.
 */
export function lookupV3(texelAt, transform, x, y, origin = [0, 0], scale = 1) {
  const world = Array.isArray(transform) ? fixedAffine(transform) : transform;
  const q = world && fixedInverse(world);
  if (!q) throw new Error("v3 refuses a lookup without a Q16 inverse");
  if (!origin.every(fitsI32) || !fitsI32(scale) || scale < 1) {
    throw new Error("v3 refuses a lookup at a ROI without i32 integer origin and scale");
  }
  const px = pixelQ8(origin[0], x, scale);
  const py = pixelQ8(origin[1], y, scale);
  const pos = px === null || py === null ? null : sourceQ8(q, px, py);
  if (!pos) return null;
  return texelAt(floorFracQ8(pos[0])[0], floorFracQ8(pos[1])[0]);
}

/** The mask byte of `lookupV3`; `byteAt(x, y)` is the stored byte, 0 outside its tiles. */
export function maskByteV3(byteAt, transform, x, y, origin = [0, 0], scale = 1) {
  return lookupV3(byteAt, transform, x, y, origin, scale) ?? 0;
}

/**
 * A texel source over `tiles` (`[{ key: [tx, ty], bytes }]`, 256² tiles of
 * `channels` bytes per texel), transparent outside them.
 */
function tileTexels(tiles, channels) {
  const byKey = new Map(tiles.map((t) => [`${t.key[0]},${t.key[1]}`, t.bytes]));
  return (x, y) => {
    const kx = Math.floor(x / 256), ky = Math.floor(y / 256);
    const bytes = byKey.get(`${kx},${ky}`);
    if (!bytes) return channels === 1 ? 0 : [0, 0, 0, 0];
    const i = ((y - ky * 256) * 256 + x - kx * 256) * channels;
    return channels === 1 ? bytes[i] : Array.from(bytes.subarray(i, i + 4));
  };
}

/**
 * Q14 coverage of `mask` at pixel (x, y) of a tile with ROI `frame`
 * (`{ origin, scale }`). A mask is `{ bytes, transform }` (one 256² R8 tile at
 * key 0,0) or `{ tiles, transform }` (R8 tiles by key, `tileTexels`); its
 * transform is the document transform, after linking.
 */
function maskCoverage(mask, x, y, frame) {
  if (!mask.at) mask.at = tileTexels(mask.tiles ?? [{ key: [0, 0], bytes: mask.bytes }], 1);
  return BigInt(byteQ14(maskByteV3(mask.at, mask.transform ?? IDENT, x, y, frame.origin, frame.scale)));
}

/**
 * The stored bytes of a raster layer at pixel (x, y): `image` is the tile's own
 * pixels (mode 18 of a raster drawn as it is); `{ tiles, transform }` are RGBA
 * tiles by key under the layer's document transform (the Q16 nearest lookup).
 */
function rasterBytes(layer, i, x, y, frame) {
  if (!layer.tiles) return Array.from(layer.image.pixels.subarray(i, i + 4));
  if (!layer.at) layer.at = tileTexels(layer.tiles, 4);
  return lookupV3(layer.at, layer.transform ?? IDENT, x, y, frame.origin, frame.scale) ?? [0, 0, 0, 0];
}

/**
 * Render version 3 M5 reference for a stack at the identity transform, as
 * `renderer/m5.rs` dispatches it. `layers` is bottom first. A raster is
 * `{ image, opacity, mode, effects, mask }` and composites with mode 19; an effect
 * is color-adjust settings, with an optional `selection` mask (mode 22). A group is
 * `{ group: layers, opacity, mode, mask }`, composited onto an empty target first.
 * A `mask` is mode 21: the mask and the opacity once, then mode 19 at opacity 1.
 * `{ adjust, opacity, mode, mask }` is a color-adjust adjustment layer: mode 17,
 * then mode 23 with a mask or a non-normal blend, else mode 20 below opacity 1.
 * A mask is `{ bytes, transform }` or `{ tiles, transform }` (see `maskCoverage`).
 * A raster's pixels are `image` (the tile's own pixels) or `{ tiles, transform }`
 * (see `rasterBytes`). The tile's ROI has origin `origin` and level scale `scale`.
 */
export function renderM5V3(layers, w = 256, h = 256, origin = [0, 0], scale = 1) {
  const out = new Uint8Array(w * h * 4);
  const frame = { origin, scale };
  for (let i = 0; i < out.length; i += 4) {
    out.set(resolveQ14(stackM5V3(layers, i, (i / 4) % w, Math.floor(i / 4 / w), frame)), i);
  }
  return out;
}

function stackM5V3(layers, i, x, y, frame) {
  const Q = 16384n;
  const big = (q) => q.map(BigInt);
  let accum = [0n, 0n, 0n, 0n];
  for (const layer of layers) {
    const opacity = cqOpacity(layer.opacity ?? 1);
    const mode = layer.mode ?? 0;
    if (layer.adjust) {
      const adjusted = big(colorAdjustStageQ14(accum.map(Number), layer.adjust));
      if (layer.mask || mode !== 0) {
        const m = layer.mask ? maskCoverage(layer.mask, x, y, frame) : Q;
        accum = maskAdjustQ14(accum, adjusted, m * opacity, mode);
      } else {
        accum = opacity === Q ? adjusted : accum.map((v, c) => cqDiv(v * (Q - opacity) + adjusted[c] * opacity));
      }
      continue;
    }
    let src;
    if (layer.group) src = stackM5V3(layer.group, i, x, y, frame);
    else {
      let q = big(ingressQ14(rasterBytes(layer, i, x, y, frame)));
      for (const effect of layer.effects || []) {
        const adjusted = big(colorAdjustStageQ14(q.map(Number), effect));
        q = effect.selection ? maskSelectQ14(q, adjusted, maskCoverage(effect.selection, x, y, frame)) : adjusted;
      }
      src = q;
    }
    let op = opacity;
    if (layer.mask) {
      src = maskLayerQ14(src, maskCoverage(layer.mask, x, y, frame) * opacity);
      op = Q;
    }
    const straight = cqStraight(src);
    if (op === Q) accum = compositeQ14Blend(accum, straight, src.slice(0, 3), src[3], mode);
    else {
      const sa = cqMul(src[3], op);
      accum = compositeQ14Blend(accum, straight, straight.map((v) => cqMul(v, sa)), sa, mode);
    }
  }
  return accum;
}

// Render version 3 Gaussian blur (doc/design/2026-10-28-v3-integer-blur.md),
// written independently of `alto_core::blur` and `renderer.rs`. Bits and the
// kernel are BigInt; pixel sums stay below 2^31 and use Number.
const f32Bits = (x) => {
  const view = new DataView(new ArrayBuffer(4));
  view.setFloat32(0, x);
  return view.getUint32(0);
};

/** `floor(radius * 256 + 1/2)` of the stored f64, from its bits (Rust `blur_radius_q8_f64`). */
export function blurRadiusQ8(radius) {
  // The document validates 0..=64; outside it Rust refuses rather than decodes.
  if (!(radius >= 0 && radius <= 64)) throw new RangeError(`v3 blur radius out of range: ${radius}`);
  const bits = f64Bits(radius);
  const e = Number((bits >> 52n) & 0x7ffn);
  if (e === 0) return 0;
  const m = (bits & ((1n << 52n) - 1n)) | (1n << 52n);
  const s = 1067 - e;
  if (s <= 0) return Number(m << BigInt(-s));
  if (s >= 54) return 0;
  return Number((m + (1n << BigInt(s - 1))) >> BigInt(s));
}

/** The same for an f32 feather (Rust `blur_radius_q8_f32`). */
export function blurRadiusQ8F32(feather) {
  if (!(feather >= 0 && Math.fround(feather) <= 64)) throw new RangeError(`v3 feather out of range: ${feather}`);
  const bits = f32Bits(feather);
  const e = (bits >>> 23) & 0xff;
  if (e === 0) return 0;
  const m = BigInt((bits & 0x7fffff) | 0x800000);
  const s = 142 - e;
  if (s <= 0) return Number(m << BigInt(-s));
  if (s >= 25) return 0;
  return Number((m + (1n << BigInt(s - 1))) >> BigInt(s));
}

/** The least k with `64 * 2^k >= rf`, from the f32 bits; null for 0 (Rust `blur_coarsen_k`). */
export function blurCoarsenK(rf) {
  // Defined on non-negative finite f32 bits (Rust returns None otherwise).
  if (!(rf >= 0 && Number.isFinite(rf))) return null;
  const bits = f32Bits(rf);
  const e = (bits >>> 23) & 0xff;
  const frac = bits & 0x7fffff;
  if (e === 0 && frac === 0) return null;
  const [m, power] = e === 0 ? [frac, -149] : [frac | 0x800000, e - 150];
  const clog2 = m <= 1 ? 0 : (m - 1).toString(2).length;
  return clog2 + power - 6;
}

/** Half kernel `w[0..=r]` in Q16, `w[0] + 2 * sum(w[1..])` exactly 65536 (memo "重みの生成"). */
export function blurKernelQ16(r, rho8) {
  if (!Number.isInteger(r) || r < 1 || r > 64 || !Number.isInteger(rho8) || rho8 < 0 || rho8 > 16384) {
    throw new RangeError(`v3 blur kernel out of range: (${r}, ${rho8})`);
  }
  const ONE = 1n << 32n;
  const divR = (n, d) => (n + d / 2n) / d;
  const rho = BigInt(Math.max(rho8, 256));
  const g = [];
  for (let i = 0n; i <= BigInt(r); i++) {
    const y = divR(4608n * i * i * ONE, rho * rho);
    let h = ONE;
    for (let k = 6n; k >= 1n; k--) h = ONE - divR(y * h, k * ONE);
    for (let n = 0; n < 6; n++) h = (h * h + (1n << 31n)) >> 32n;
    g.push(h);
  }
  const tails = new Array(r + 2).fill(0n);
  for (let k = r; k >= 1; k--) tails[k] = tails[k + 1] + g[k];
  const total = g[0] + 2n * tails[1];
  const p = new Array(r + 2).fill(0n);
  for (let k = 1; k <= r; k++) p[k] = divR(65536n * tails[k], total);
  const w = [Number(65536n - 2n * p[1])];
  for (let k = 1; k <= r; k++) w.push(Number(p[k] - p[k + 1]));
  return w;
}

/**
 * One pass over a `w x h` plane of Q14 RGBA (`Array` of 4-arrays), the same size
 * out; outside the plane is transparent. `vertical` picks the axis.
 */
export function blurPassQ14(plane, w, h, weights, vertical) {
  const r = weights.length - 1;
  const out = new Array(w * h);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const acc = [0, 0, 0, 0];
    for (let i = -r; i <= r; i++) {
      const sx = vertical ? x : x + i, sy = vertical ? y + i : y;
      if (sx < 0 || sy < 0 || sx >= w || sy >= h) continue;
      const q = plane[sy * w + sx], k = weights[Math.abs(i)];
      for (let c = 0; c < 4; c++) acc[c] += q[c] * k;
    }
    out[y * w + x] = acc.map((v) => Math.floor((v + 32768) / 65536));
  }
  return out;
}

/**
 * A level-0 tile of one raster layer (`image`, straight RGBA at the document
 * origin) with Gaussian blurs `radii` (document f64 values, in effect order) on
 * a `canvas` of `[w, h]`. The layer is blurred as an infinite transparent plane
 * and cut, as the GPU's ROI chain gives (memo "端"); level 0 never coarsens
 * (`blurCoarsenK(rf) <= 0` for every valid radius).
 */
export function renderBlurV3(image, radii, tile = [0, 0], canvas = [256, 256]) {
  const taps = radii.map(blurRadiusQ8).filter((q) => q > 0).map((q) => [Math.ceil(q / 256), q]);
  const pad = taps.reduce((sum, [r]) => sum + r, 0);
  const w = image.w + 2 * pad, h = image.h + 2 * pad;
  let plane = new Array(w * h);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const ix = x - pad, iy = y - pad;
    const inside = ix >= 0 && iy >= 0 && ix < image.w && iy < image.h;
    const i = (iy * image.w + ix) * 4;
    plane[y * w + x] = inside ? ingressQ14(Array.from(image.pixels.subarray(i, i + 4))) : [0, 0, 0, 0];
  }
  for (const [r, rho8] of taps) {
    const weights = blurKernelQ16(r, rho8);
    plane = blurPassQ14(blurPassQ14(plane, w, h, weights, false), w, h, weights, true);
  }
  const out = new Uint8Array(256 * 256 * 4);
  for (let y = 0; y < 256; y++) for (let x = 0; x < 256; x++) {
    const dx = tile[0] * 256 + x, dy = tile[1] * 256 + y;
    // Outside the canvas `[0, w) x [0, h)` the resolve writes transparent.
    if (dx < 0 || dy < 0 || dx >= canvas[0] || dy >= canvas[1]) continue;
    const px = dx + pad, py = dy + pad;
    if (px >= w || py >= h) continue;
    out.set(resolveQ14(plane[py * w + px]), (y * 256 + x) * 4);
  }
  return out;
}

// Render version 3 vector fill (doc/design/2026-10-28-v3-integer-vector-fill.md),
// written independently of `raster_q8.rs`: Q8 points from the f64 bits through
// the Q32 world, a sparse-cell nonzero integration, 16 even-odd sub-scanlines,
// and the Q14 source-over of `vector-q14.wgsl`. Exact integers throughout.
const VECTOR_NMAX = 4096n;

/** Half-away `n / d` for BigInt (Rust `raster_q8::div_q`). */
const vDiv = (n, d) => (n < 0n ? -((-n + d / 2n) / d) : (n + d / 2n) / d);

/** A path's commands as Q8 level-0 points under `world` (Rust `map_path`). */
function vectorQ8(commands, world) {
  const pt = (x, y) => {
    const X = qFromF64Bits(f64Bits(x), 16), Y = qFromF64Bits(f64Bits(y), 16);
    const axis = (a, b, t) => vDiv(a * X + b * Y + (t << 32n), 1n << 40n);
    return [axis(world.m[0], world.m[2], world.t[0]), axis(world.m[1], world.m[3], world.t[1])];
  };
  return commands.map((c) => {
    switch (c.op) {
      case "move": return ["M", pt(c.x, c.y)];
      case "line": return ["L", pt(c.x, c.y)];
      case "quad": return ["Q", pt(c.cx, c.cy), pt(c.x, c.y)];
      case "cubic": return ["C", pt(c.c1x, c.c1y), pt(c.c2x, c.c2y), pt(c.x, c.y)];
      case "close": return ["Z"];
      default: throw new Error(`unknown path op ${c.op}`);
    }
  });
}

/** Glyph curves cap at this many segments (v2's 1..=64). */
const GLYPH_NMAX = 64n;
/** A glyph's Q8 extent from its own pixel origin (Rust `GLYPH_EXTENT`). */
const GLYPH_EXTENT = 1n << 24n;

/**
 * Smallest n in 1..=4096 with 65536 n^4 >= K^2 D, or null (VECTOR_CURVE_RANGE);
 * for a glyph, the smallest in 1..=64, or 64 when none is enough.
 */
function vectorDivisions(d, k, glyph = false) {
  const need = k * k * d;
  const limit = glyph ? GLYPH_NMAX : VECTOR_NMAX;
  for (let n = 1n; n <= limit; n++) if (65536n * n ** 4n >= need) return n;
  return glyph ? GLYPH_NMAX : null;
}
const secondDifference = (p, c, q) => {
  const x = p[0] - 2n * c[0] + q[0], y = p[1] - 2n * c[1] + q[1];
  return x * x + y * y;
};

/** Closed polygons (segments) of Q8 commands, or null when a curve is out of range. */
function vectorSegments(commands, glyph = false) {
  const segments = [];
  let start = null, current = null;
  const edge = (a, b) => { if (a[0] !== b[0] || a[1] !== b[1]) segments.push([a, b]); };
  const close = () => { if (current && start) { edge(current, start); current = start; } };
  for (const [op, ...p] of commands) {
    if (op === "M") { close(); start = current = p[0]; continue; }
    if (op === "L") { if (current) edge(current, p[0]); else start = p[0]; current = p[0]; continue; }
    if (op === "Z") { close(); continue; }
    // A curve with no current point is out of range, as in Rust (`Range`).
    if (!current) return null;
    const a = current;
    const ctrl = op === "Q" ? [a, p[0], p[1]] : [a, p[0], p[1], p[2]];
    const d = op === "Q" ? secondDifference(a, p[0], p[1])
      : [secondDifference(a, p[0], p[1]), secondDifference(p[0], p[1], p[2])].reduce((x, y) => (x > y ? x : y));
    const n = vectorDivisions(d, op === "Q" ? 10n : 30n, glyph);
    if (n === null) return null;
    const binomial = op === "Q" ? [1n, 2n, 1n] : [1n, 3n, 3n, 1n];
    const degree = BigInt(binomial.length - 1);
    for (let i = 1n; i <= n; i++) {
      const j = n - i;
      const next = [0, 1].map((axis) => vDiv(
        ctrl.reduce((sum, q, k) => sum + binomial[k] * j ** (degree - BigInt(k)) * i ** BigInt(k) * q[axis], 0n),
        n ** degree,
      ));
      edge(current, next);
      current = next;
    }
  }
  close();
  return segments;
}

/**
 * Coverage bytes of one path over the `[ox, oy, w, h]` window (level pixels;
 * `w` and `h` default to a tile), or null when a curve is out of range (the
 * path is skipped). `glyph` selects the glyph rule (Rust `rasterize_glyph`):
 * curves cap at 64 segments instead of being refused, and a glyph wider than
 * 2^24 Q8 from its own pixel origin is refused (null) whatever the window.
 */
function vectorCoverage(commands, rule, [ox, oy, w = 256, h = 256], glyph = false) {
  const W = BigInt(w), H = BigInt(h);
  const points = commands.flatMap(([, ...p]) => p);
  const bytes = new Uint8Array(w * h);
  if (!points.length) return bytes;
  // Shift to non-negative Q8 (the origin A): the half-away rounding below is
  // not translation invariant across zero.
  const min = [0, 1].map((a) => points.reduce((m, p) => (p[a] < m ? p[a] : m), points[0][a]));
  const origin = min.map((v) => (v >= 0n ? v / 256n : -((-v + 255n) / 256n)));
  const shifted = commands.map(([op, ...p]) => [op, ...p.map((q) => [q[0] - origin[0] * 256n, q[1] - origin[1] * 256n])]);
  if (glyph && shifted.some(([, ...p]) => p.some((q) => q[0] > GLYPH_EXTENT || q[1] > GLYPH_EXTENT))) return null;
  const segments = vectorSegments(shifted, glyph);
  if (segments === null) return null;
  const lx = BigInt(ox) - origin[0], ly = BigInt(oy) - origin[1];
  const XL = lx * 256n, XR = XL + 256n * W, YT = ly * 256n, YB = YT + 256n * H;
  const floorDiv = (v, d) => (v >= 0n ? v / d : -((-v + d - 1n) / d));
  const floor256 = (v) => floorDiv(v, 256n);
  const lerpY = (a, b, y) => vDiv(a[0] * (b[1] - a[1]) + (b[0] - a[0]) * (y - a[1]), b[1] - a[1]);
  const coverageByte = (e) => Number(((e > 131072n ? 131072n : e) * 255n + 65536n) >> 17n);
  if (rule === "even_odd") {
    for (let row = 0n; row < H; row++) {
      const tally = new Array(w).fill(0n);
      for (let sub = 0n; sub < 16n; sub++) {
        const y = YT + row * 256n + sub * 16n + 8n;
        const xs = [];
        for (const [p, q] of segments) {
          const [a, b] = p[1] < q[1] ? [p, q] : [q, p];
          if (a[1] <= y && y < b[1]) xs.push(lerpY(a, b, y));
        }
        xs.sort((u, v) => (u < v ? -1 : u > v ? 1 : 0));
        for (let k = 0; k + 1 < xs.length; k += 2) {
          const left = xs[k] > XL ? xs[k] : XL, right = xs[k + 1] < XR ? xs[k + 1] : XR;
          for (let cx = 0n; cx < W; cx++) {
            const c0 = XL + cx * 256n, c1 = c0 + 256n;
            const lo = left > c0 ? left : c0, hi = right < c1 ? right : c1;
            if (hi > lo) tally[cx] += hi - lo;
          }
        }
      }
      for (let cx = 0; cx < w; cx++) bytes[Number(row) * w + cx] = coverageByte(32n * tally[cx]);
    }
    return bytes;
  }
  // Nonzero: cover and area per touched cell, kept in a sparse map.
  const cells = new Map();
  const cell = (cx, cy) => {
    const key = `${cx},${cy}`;
    if (!cells.has(key)) cells.set(key, { cover: 0n, area: 0n });
    return cells.get(key);
  };
  for (const [p, q] of segments) {
    if (p[1] === q[1]) continue;
    const [a, b, dir] = p[1] < q[1] ? [p, q, 1n] : [q, p, -1n];
    for (let row = floor256(a[1]) > ly ? floor256(a[1]) : ly; row <= floor256(b[1]) && row < ly + H; row++) {
      let top = a[1] > row * 256n ? a[1] : row * 256n; if (top < YT) top = YT;
      let bottom = b[1] < row * 256n + 256n ? b[1] : row * 256n + 256n; if (bottom > YB) bottom = YB;
      if (bottom <= top) continue;
      const xa = top === a[1] ? a[0] : lerpY(a, b, top);
      const xb = bottom === b[1] ? b[0] : lerpY(a, b, bottom);
      // Split the row piece at every column boundary inside it (window columns only).
      const pts = [[xa, top]];
      const lo = xa < xb ? xa : xb, hi = xa < xb ? xb : xa;
      const span = bottom - top, run = hi - lo;
      const cuts = [];
      for (let col = floor256(lo) > lx ? floor256(lo) : lx; col <= floor256(hi) && col <= lx + W; col++) {
        const x = col * 256n;
        if (lo < x && x < hi) {
          const dist = x > xa ? x - xa : xa - x;
          cuts.push([x, vDiv(top * run + span * dist, run)]);
        }
      }
      cuts.sort((u, v) => (xa < xb ? (u[0] < v[0] ? -1 : 1) : (u[0] > v[0] ? -1 : 1)));
      pts.push(...cuts, [xb, bottom]);
      for (let k = 0; k + 1 < pts.length; k++) {
        let [ux, va] = pts[k], [vx, vb] = pts[k + 1];
        const dy = vb - va;
        if (dy === 0n || (ux >= XR && vx >= XR)) continue;
        if (ux <= XL && vx <= XL) ux = vx = XL;
        const cx = floorDiv(ux + vx, 512n) - lx;
        const cy = floorDiv(va + vb, 512n) - ly;
        if (cx < 0n || cy < 0n || cx >= W || cy >= H) continue;
        const c = cell(cx, cy);
        c.cover += dir * dy;
        c.area += dir * (ux + vx - 2n * (lx + cx) * 256n) * dy;
      }
    }
  }
  for (let cy = 0n; cy < H; cy++) {
    let sum = 0n;
    for (let cx = 0n; cx < W; cx++) {
      const c = cells.get(`${cx},${cy}`);
      if (c) sum += c.cover;
      const n = sum * 512n - (c ? c.area : 0n);
      bytes[Number(cy * W + cx)] = coverageByte(n < 0n ? -n : n);
    }
  }
  return bytes;
}

/** Q14 of a stored f32 colour channel (`Math.fround`), half away; exact. */
const vectorColorQ14 = (v) => Math.floor(Math.fround(v) * 16384 + 0.5);

/** One Q14 source-over of `color` (straight Q14) at coverage byte `b`. */
function vectorPaintQ14(dst, color, b) {
  const f = byteQ14(b) * color[3];
  const sa = Math.floor((f + 8192) / 16384);
  return dst.map((d, c) => {
    const s = c === 3 ? sa : Number((BigInt(color[c]) * BigInt(f) + (1n << 27n)) >> 28n);
    return Math.floor((s * 16384 + d * (16384 - sa) + 8192) / 16384);
  });
}

/** The Q14 coverage of a white mask/clip path at each pixel (`vector_q14` with white). */
function vectorMaskQ14(path, world, tile) {
  const bytes = vectorCoverage(vectorQ8(path.commands, world), path.fill_rule ?? "non_zero", tile) ?? new Uint8Array(65536);
  return Array.from(bytes, byteQ14);
}

/**
 * A level-0 tile of one layer at node transform `transform`: a vector node
 * (`paths`, styled paths in the document JSON) or a raster (`image` at the
 * origin), with an optional `vectorMask` and `clipPath` (feather 0), on a
 * `canvas`. Their coverages are multiplied once and applied once (mode 26).
 */
export function renderVectorV3({ paths, image, transform = IDENT, vectorMask, clipPath }, tile = [0, 0], canvas = [256, 256]) {
  // Rust refuses a world outside the FixedAffine range (GpuUnsupported); only a
  // point the Q8 grid cannot hold makes the node transparent (`map_path`).
  const world = fixedAffine(transform);
  if (!world) throw new Error("v3 refuses a transform outside the fixed affine range");
  const origin = [tile[0] * 256, tile[1] * 256];
  let q;
  if (paths) {
    q = Array.from({ length: 65536 }, () => [0, 0, 0, 0]);
    for (const path of paths) {
      if (!path.fill) continue;
      const color = path.fill.map(vectorColorQ14);
      if (color[3] === 0) continue;
      const bytes = vectorCoverage(vectorQ8(path.data.commands, world), path.fill_rule ?? "non_zero", origin);
      if (!bytes) continue;
      for (let i = 0; i < 65536; i++) q[i] = vectorPaintQ14(q[i], color, bytes[i]);
    }
  } else {
    // A raster reads its texel through the v3 Q16 nearest lookup of `transform`.
    const texelAt = (tx, ty) => {
      if (tx < 0 || ty < 0 || tx >= image.w || ty >= image.h) return [0, 0, 0, 0];
      const k = (ty * image.w + tx) * 4;
      return Array.from(image.pixels.subarray(k, k + 4));
    };
    q = Array.from({ length: 65536 }, (_, i) =>
      ingressQ14(lookupV3(texelAt, transform, i % 256, Math.floor(i / 256), origin, 1) ?? [0, 0, 0, 0]));
  }
  // A disabled mask or clip path is none (Rust filters on `enabled`).
  const masks = [vectorMask, clipPath].filter((m) => m && m.enabled !== false).map((m) => vectorMaskQ14(m.path, fixedConcat(world, fixedAffine(m.transform ?? IDENT)), origin));
  if (masks.length) {
    const k = masks.length === 2 ? masks[0].map((a, i) => Number(divQBig(BigInt(a * masks[1][i]), 16384n))) : masks[0];
    q = q.map((v, i) => v.map((c) => Number(divQBig(BigInt(c * k[i]), 16384n))));
  }
  const out = new Uint8Array(65536 * 4);
  for (let i = 0; i < 65536; i++) {
    const x = origin[0] + (i % 256), y = origin[1] + Math.floor(i / 256);
    if (x >= 0 && y >= 0 && x < canvas[0] && y < canvas[1]) out.set(resolveQ14(q[i]), i * 4);
  }
  return out;
}

// Render version 3 text (doc/design/2026-10-28-v3-integer-text.md), written
// independently of `text_bake.rs`, `glyph_q8.rs` and the host: the ALTB bake is
// the only input; the world is the v3 FixedAffine of the node transform.

/** The ALTB bake (little endian) with every bound checked, or null (TEXT_BAKE_INVALID). */
export function decodeTextBake(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let at = 0;
  const need = (n) => { if (at + n > bytes.length) throw new Error("truncated"); };
  const u16 = () => { need(2); const v = view.getUint16(at, true); at += 2; return v; };
  const u32 = () => { need(4); const v = view.getUint32(at, true); at += 4; return v; };
  const i32 = () => { need(4); const v = view.getInt32(at, true); at += 4; return BigInt(v); };
  const i64 = () => { need(8); const v = view.getBigInt64(at, true); at += 8; return v; };
  const MAX_COORD = 1n << 22n, MAX_T = 1n << 60n, ABS = (v) => (v < 0n ? -v : v);
  try {
    need(4);
    if (String.fromCharCode(...bytes.subarray(0, 4)) !== "ALTB") return null;
    at = 4;
    if (u16() !== 1) return null;
    u16(); // baker, diagnostic only
    const fonts = [];
    for (let n = u32(), i = 0; i < n; i++) { need(32); at += 32; fonts.push(u32()); }
    const outlines = [];
    for (let n = u32(), i = 0; i < n; i++) {
      const font = u32();
      if (font !== 0xffffffff && font >= fonts.length) return null;
      u16(); // glyph id
      const upem = u16();
      if (upem < 1 || upem > 16384) return null;
      const count = u32();
      if (count > 4000) return null;
      const commands = [];
      const point = () => {
        const p = [i32(), i32()];
        if (ABS(p[0]) > MAX_COORD || ABS(p[1]) > MAX_COORD) throw new Error("coordinate");
        return p;
      };
      for (let k = 0; k < count; k++) {
        need(1);
        const tag = bytes[at++];
        if (tag === 0) commands.push(["M", point()]);
        else if (tag === 1) commands.push(["L", point()]);
        else if (tag === 2) commands.push(["Q", point(), point()]);
        else if (tag === 3) commands.push(["C", point(), point(), point()]);
        else if (tag === 4) commands.push(["Z"]);
        else return null;
      }
      outlines.push({ upem: BigInt(upem), commands });
    }
    const glyphs = [];
    for (let n = u32(), i = 0; i < n; i++) {
      const outline = u32();
      if (outline >= outlines.length) return null;
      const m = [i32(), i32(), i32(), i32()];
      const t = [i64(), i64()];
      if (t.some((v) => ABS(v) > MAX_T)) return null;
      const color = [u16(), u16(), u16(), u16()];
      if (color.some((c) => c > 16384)) return null;
      glyphs.push({ outline, m, t, color });
    }
    return at === bytes.length ? { outlines, glyphs } : null;
  } catch {
    return null;
  }
}

const tAbs = (v) => (v < 0n ? -v : v);
const tFloorDiv = (a, b) => (a >= 0n ? a / b : -((-a + b - 1n) / b));
const tCeilDiv = (a, b) => -tFloorDiv(-a, b);

/** C: font units to level pixels, Q24 linear and Q30 translation; null out of range. */
export function glyphMapQ(world, level, m, t) {
  if (level < -16 || level > 12) return null;
  const d = 1n << BigInt(32 + level);
  const [a, b, c, e] = world.m, wt = world.t.map((v) => v << 46n);
  const C = {
    m: [vDiv(a * m[0] + c * m[1], d), vDiv(b * m[0] + e * m[1], d), vDiv(a * m[2] + c * m[3], d), vDiv(b * m[2] + e * m[3], d)],
    t: [vDiv(a * t[0] + c * t[1] + wt[0], d), vDiv(b * t[0] + e * t[1] + wt[1], d)],
  };
  return C.m.every((v) => tAbs(v) < 1n << 31n) && C.t.every((v) => tAbs(v) < 1n << 60n) ? C : null;
}

/** The atlas band: Frobenius / sqrt 2 under 24 px/em, strictly. */
export function atlasBandQ(C, upem) {
  const frob = C.m.reduce((sum, v) => sum + v * v, 0n);
  return frob * upem * upem < 2n * 576n * (1n << 48n);
}

const outlineBounds = (commands) => {
  const pts = commands.flatMap(([, ...p]) => p);
  if (!pts.length) return null;
  const min = (a) => pts.reduce((m, p) => (p[a] < m ? p[a] : m), pts[0][a]);
  const max = (a) => pts.reduce((m, p) => (p[a] > m ? p[a] : m), pts[0][a]);
  return [min(0), min(1), max(0), max(1)];
};

/** The memo's place_q: bands, origin and phase, and the bitmap box; null for the direct band. */
export function placeQ(C, outline) {
  const u = outline.upem;
  const band = C.m.map((v) => vDiv(v * u * 4n, 1n << 24n));
  if (band[0] * band[3] === band[1] * band[2]) return null;
  const q = C.t.map((v) => vDiv(v, 1n << 28n));
  const origin = q.map((v) => tFloorDiv(v, 4n));
  const phase = q.map((v, k) => v - origin[k] * 4n);
  const point = (x, y) => [
    vDiv(band[0] * x + band[2] * y + 64n * phase[0] * u, u),
    vDiv(band[1] * x + band[3] * y + 64n * phase[1] * u, u),
  ];
  const b = outlineBounds(outline.commands);
  if (!b) return null;
  const corners = [point(b[0], b[1]), point(b[2], b[1]), point(b[0], b[3]), point(b[2], b[3])];
  const lo = (a) => corners.reduce((m, c) => (c[a] < m ? c[a] : m), corners[0][a]);
  const hi = (a) => corners.reduce((m, c) => (c[a] > m ? c[a] : m), corners[0][a]);
  const x0 = tFloorDiv(lo(0), 256n) - 1n, y0 = tFloorDiv(lo(1), 256n) - 1n;
  const w = tCeilDiv(hi(0), 256n) + 1n - x0, h = tCeilDiv(hi(1), 256n) + 1n - y0;
  if (w < 1n || h < 1n || w > 96n || h > 96n) return null;
  return { band, origin, phase, x0, y0, w: Number(w), h: Number(h), point };
}

const mapGlyph = (commands, point) => commands.map(([op, ...p]) => [op, ...p.map(([x, y]) => point(x, y))]);
const directPoint = (C) => (x, y) => [
  vDiv(C.m[0] * x + C.m[2] * y + C.t[0], 1n << 22n),
  vDiv(C.m[1] * x + C.m[3] * y + C.t[1], 1n << 22n),
];

/** A direct-band glyph's window: its mapped box in pixels within the ROI, or null. */
function directWindowQ(C, outline, [ox, oy, w, h]) {
  const b = outlineBounds(outline.commands);
  if (!b) return null;
  const p = directPoint(C);
  const corners = [p(b[0], b[1]), p(b[2], b[1]), p(b[0], b[3]), p(b[2], b[3])];
  const lo = (a) => tFloorDiv(corners.reduce((m, c) => (c[a] < m ? c[a] : m), corners[0][a]), 256n);
  const hi = (a) => tCeilDiv(corners.reduce((m, c) => (c[a] > m ? c[a] : m), corners[0][a]), 256n);
  const left = lo(0) > BigInt(ox) ? lo(0) : BigInt(ox), top = lo(1) > BigInt(oy) ? lo(1) : BigInt(oy);
  const right = hi(0) < BigInt(ox + w) ? hi(0) : BigInt(ox + w), bottom = hi(1) < BigInt(oy + h) ? hi(1) : BigInt(oy + h);
  return right > left && bottom > top ? [Number(left), Number(top), Number(right - left), Number(bottom - top)] : null;
}

/**
 * A level-`level` tile of one v3 text node from its ALTB `bake` bytes, at node
 * `transform` on a `canvas`: per glyph in bake order, C, the atlas band
 * (place_q's bitmap) or the direct band (its window in the tile), and the Q14
 * normal over the glyph's straight colour. An undecodable bake is transparent.
 */
export function renderTextV3(bake, transform = IDENT, tile = [0, 0], canvas = [256, 256], level = 0) {
  // The renderer's ROI takes levels -6..=12 only (glyphMapQ itself goes to -16).
  if (level < -6 || level > 12) throw new Error("v3 refuses a level outside -6..=12");
  const world = fixedAffine(transform);
  if (!world) throw new Error("v3 refuses a transform outside the fixed affine range");
  const roi = [tile[0] * 256, tile[1] * 256, 256, 256];
  const q = Array.from({ length: 65536 }, () => [0, 0, 0, 0]);
  const decoded = bake && decodeTextBake(bake);
  for (const g of decoded?.glyphs ?? []) {
    const outline = decoded.outlines[g.outline];
    const C = glyphMapQ(world, level, g.m, g.t);
    if (!C) continue;
    const paint = (bytes, [bx, by, bw]) => {
      for (let i = 0; i < bytes.length; i++) {
        const x = bx + (i % bw) - roi[0], y = by + Math.floor(i / bw) - roi[1];
        if (bytes[i] && x >= 0 && y >= 0 && x < 256 && y < 256) q[y * 256 + x] = vectorPaintQ14(q[y * 256 + x], g.color, bytes[i]);
      }
    };
    const placed = atlasBandQ(C, outline.upem) ? placeQ(C, outline) : null;
    if (placed) {
      const points = mapGlyph(outline.commands, (x, y) => {
        const b = placed.point(x, y);
        return [b[0] - 256n * placed.x0, b[1] - 256n * placed.y0];
      });
      const bytes = vectorCoverage(points, "non_zero", [0, 0, placed.w, placed.h], true);
      if (bytes) paint(bytes, [Number(placed.origin[0] + placed.x0), Number(placed.origin[1] + placed.y0), placed.w]);
    } else {
      const window = directWindowQ(C, outline, roi);
      if (!window) continue;
      const bytes = vectorCoverage(mapGlyph(outline.commands, directPoint(C)), "non_zero", window, true);
      if (bytes) paint(bytes, window);
    }
  }
  // The resolve keeps a level pixel whose centre, in document pixels, lies in
  // the canvas (Rust `resolve_span_q`): (2x + 1) 2^L in [0, 2 canvas) at
  // L >= 0, and x in [0, canvas 2^-L) below.
  const inCanvas = (x, end) => (level >= 0
    ? x >= 0 && (2 * x + 1) * 2 ** level < 2 * end
    : x >= 0 && x < end * 2 ** -level);
  const out = new Uint8Array(65536 * 4);
  for (let i = 0; i < 65536; i++) {
    const x = roi[0] + (i % 256), y = roi[1] + Math.floor(i / 256);
    if (inCanvas(x, canvas[0]) && inCanvas(y, canvas[1])) out.set(resolveQ14(q[i]), i * 4);
  }
  return out;
}

/**
 * The bytes `selection_ops` writes for a feathered rectangle `[x0, y0, x1, y1]` on a
 * 256² document: f64 distance to the nearest edge, `(d / feather + 0.5)` clamped to
 * 0..1, then `round(coverage * 255)`. Non-negative, so `Math.round` is Rust's `round`.
 */
export function featheredRectangle(rect, feather) {
  const bytes = new Uint8Array(256 * 256);
  for (let y = 0; y < 256; y++) for (let x = 0; x < 256; x++) {
    const px = x + 0.5;
    const py = y + 0.5;
    const d = Math.min(px - rect[0], rect[2] - px, py - rect[1], rect[3] - py);
    bytes[y * 256 + x] = Math.round(Math.min(1, Math.max(0, d / feather + 0.5)) * 255);
  }
  return bytes;
}

export function fnv64(bytes) {
  let h = 0xcbf29ce484222325n;
  const prime = 0x100000001b3n;
  const mask = 0xffffffffffffffffn;
  for (const b of bytes) h = ((h ^ BigInt(b)) * prime) & mask;
  return h.toString(16).padStart(16, "0");
}

const GAP_ORDER = ["mask", "vector mask", "clip path", "clip", "effects", "vector", "text", "adjustment", "unsupported"];

// Selection-limited color-adjust and Gaussian blur are v3 mode 22.
const isColorAdjust = (effect) => effect?.type === "color_adjust";
const isIntegerEffect = (effect) => isColorAdjust(effect) || effect?.type === "gaussian_blur";
// Rust `raster_q8::stroke_visible`: a nonzero width (the default is 1) and an
// alpha whose half-away Q14 is nonzero, i.e. at least 2^-15.
const strokeVisible = (stroke) => Boolean(stroke) && (stroke.width ?? 1) !== 0 &&
  !(Math.abs(stroke.color?.[3] ?? 1) < 2 ** -15);

/**
 * Why a document is outside the plain composite.wgsl path, or null when it is on it.
 * Render version 3 has exactly three classes, mirroring Rust `renderer::v3_gap`:
 * null (integer tile compositor), "v3-m5-integer" (integer M5 stages: color-adjust
 * and Gaussian blur effects, with or without a selection, color-adjust adjustment
 * layers (any blend with an enabled pixel mask, normal blend without), enabled
 * pixel masks, filled vectors, enabled vector masks and clip paths, and text
 * drawn from its bake) and
 * "v3-unsupported" (the host refuses the scene; no tile is drawn). A visibly
 * stroked vector keeps the v2 tessellator and is refused, and so are vector masks and
 * clip paths on adjustment layers.
 */
export function sceneGap(snapshot) {
  const found = new Set();
  let unsupported = false;
  let integer = false;
  for (const node of snapshot.nodes.values()) {
    const props = node.props || {};
    if (props.mask) found.add("mask");
    const enabledMask = Boolean(props.mask && props.mask_enabled !== false);
    const enabledVectorMask = Boolean(props.vector_mask && props.vector_mask.enabled !== false);
    const enabledClipPath = Boolean(props.clip_path && props.clip_path.enabled !== false);
    if (enabledMask || enabledVectorMask || enabledClipPath) integer = true;
    if (props.vector_mask) found.add("vector mask");
    if (props.clip_path) found.add("clip path");
    if (props.clip_to_below) found.add("clip");
    if (props.effects?.length) found.add("effects");
    if (node.type === "vector") found.add("vector");
    if (node.type === "text") found.add("text");
    if (node.type === "adjustment") found.add("adjustment");
    if (node.type === "unsupported") found.add("unsupported");
    if (props.clip_to_below ||
        (node.type === "vector" && (node.paths || []).some((path) => strokeVisible(path?.stroke))) ||
        (props.effects || []).some((effect) => !isIntegerEffect(effect)) ||
        (node.type === "adjustment" && (!isIntegerEffect(node.adjustment) ||
          // Unmasked non-normal blends reach f32 stages upstream of mode 23.
          (!enabledMask && (props.blend ?? "normal") !== "normal") ||
          // No integer coverage mix of backdrop and adjusted copy is defined.
          enabledVectorMask || enabledClipPath))) {
      unsupported = true;
    }
  }
  if (snapshot.meta?.render_version === 3) {
    if (unsupported) return "v3-unsupported";
    if (found.has("effects") || found.has("adjustment") || found.has("vector") || found.has("text") || integer) return "v3-m5-integer";
    return null;
  }
  return GAP_ORDER.find((gap) => found.has(gap)) || null;
}

// v3: all color arithmetic is BigInt Q14; coordinates retain the v2 sampler.
const CQ = 16384n;
const cqDiv = (n, d = CQ) => divQBig(n, d);
const cqClamp = (v) => v < 0n ? 0n : v > CQ ? CQ : v;
const cqMul = (a, b) => cqDiv(a * b);
const cqLum = (c) => cqDiv(c[0] * 3483n + c[1] * 11718n + c[2] * 1183n);
const cqMin = (c) => c.reduce((a, b) => a < b ? a : b);
const cqMax = (c) => c.reduce((a, b) => a > b ? a : b);
const cqSat = (c) => cqMax(c) - cqMin(c);
function cqClip(c) {
  const l = cqLum(c), n = cqMin(c), x = cqMax(c);
  let o = c;
  if (n < 0n) o = l - n <= 0n ? [l, l, l] : c.map((v) => l + cqDiv((v - l) * l, l - n));
  if (x > CQ) o = x - l <= 0n ? [l, l, l] : o.map((v) => l + cqDiv((v - l) * (CQ - l), x - l));
  return o.map(cqClamp);
}
function cqSetLum(c, l) { const delta = l - cqLum(c); return cqClip(c.map((v) => v + delta)); }
function cqSetSat(c, s) {
  const n = cqMin(c), x = cqMax(c);
  return c.map((v) => x === n ? 0n : cqDiv((v - n) * s, x - n));
}
function floorIsqrt(n) {
  let lo = 0, hi = 16384;
  while (lo < hi) { const mid = Math.floor((lo + hi + 1) / 2); if (mid * mid <= n) lo = mid; else hi = mid - 1; }
  return lo;
}
function cqChannel(d, s, mode) {
  const overlay = (s, d) => d < 8192n ? cqDiv(2n * s * d) : CQ - cqDiv(2n * (CQ - s) * (CQ - d));
  let b;
  switch (mode) {
    case 0: b = s; break;
    case 1: b = cqMul(d, s); break;
    case 2: b = CQ - cqMul(CQ - d, CQ - s); break;
    case 3: b = overlay(s, d); break;
    case 4: b = d < s ? d : s; break;
    case 5: b = d > s ? d : s; break;
    case 6: b = s >= CQ ? CQ : cqClamp(cqDiv(d * CQ, CQ - s)); break;
    case 7: b = s === 0n ? 0n : CQ - cqClamp(cqDiv((CQ - d) * CQ, s)); break;
    case 8: b = overlay(d, s); break;
    case 9:
      if (s <= 8192n) b = d - cqDiv((CQ - 2n * s) * d * (CQ - d), CQ * CQ);
      else {
        let d2;
        if (d <= 4096n) d2 = cqDiv(16n * d * d * d - 12n * d * d * CQ + 4n * d * CQ * CQ, CQ * CQ);
        else {
          const n = Number(d * CQ), r = floorIsqrt(n);
          d2 = BigInt(r + (n - r * r >= r + 1 ? 1 : 0));
        }
        b = d + cqDiv((2n * s - CQ) * (d2 - d));
      }
      break;
    case 10: b = d > s ? d - s : s - d; break;
    case 11: b = d + s - cqDiv(2n * d * s); break;
    case 16: b = d + s; break;
    default: throw new Error(`unspecified blend mode ${mode}`);
  }
  return cqClamp(b);
}
function cqMix(d, s, mode) {
  switch (mode) {
    case 12: return cqSetLum(cqSetSat(s, cqSat(d)), cqLum(d));
    case 13: return cqSetLum(cqSetSat(d, cqSat(s)), cqLum(d));
    case 14: return cqSetLum(s, cqLum(d));
    case 15: return cqSetLum(d, cqLum(s));
    default: return s.map((v, i) => cqChannel(d[i], v, mode));
  }
}
const cqStraight = (p) => p.slice(0, 3).map((v) => p[3] === 0n ? 0n : cqClamp(cqDiv(v * CQ, p[3])));
export function compositeQ14Blend(dst, src, pre, sa, mode) {
  const da = dst[3], oa = cqDiv(sa * CQ + da * (CQ - sa));
  const b = mode === 0 ? src : cqMix(cqStraight(dst), src, mode);
  const out = src.map((s, i) => mode === 0
    ? cqDiv(pre[i] * CQ + dst[i] * (CQ - sa))
    : cqDiv((CQ - sa) * dst[i] * CQ + (CQ - da) * s * sa + da * sa * b[i], CQ * CQ));
  if (out.some((v) => v > oa)) throw new Error("Q14 RGB exceeds alpha");
  return [...out, oa];
}
const cqOpacity = (v) => BigInt(opacityQ14(v));
/**
 * Render version 3 integer 4-tap or nearest: Rust `sample::axis_unit_q16` on the
 * Q16 coefficients (`m00, m10, m01, m11`), within 6 of 0 off the axes and of
 * ±65536 on them.
 */
export function axisUnitQ16(m) {
  const near = (v, to) => Math.abs(Math.abs(v) - to) <= 6;
  return near(m[1], 0) && near(m[2], 0) && near(m[0], 65536) && near(m[3], 65536);
}
/** v3 nearest (`SAMPLE_NEAREST`): the texel under the Q8 position. */
function fetchNearest(tiles, q, originPx, scale, x, y) {
  const px = pixelQ8(originPx[0], x, scale);
  const py = pixelQ8(originPx[1], y, scale);
  const pos = px === null || py === null ? null : sourceQ8(q, px, py);
  if (!pos) return [0, 0, 0, 0];
  return sampleU8(tiles, floorFracQ8(pos[0])[0], floorFracQ8(pos[1])[0]);
}
/**
 * Render version 3 integer tile compositor (Rust `composite_integer`). A pixel
 * is on the canvas when its doubled center `2 origin + (2 i + 1) scale` is in
 * `[0, 2 end)`; every draw is the Q16 lookup (nearest or the integer 4-tap).
 * There is no f32 fetch and no f32 canvas test.
 */
function compositeInteger(ops, width, height, originPx, scaleI) {
  for (const op of ops) if (op.mode >= 17) throw new Error(`unspecified blend mode ${op.mode}`);
  const out = new Uint8Array(256 * 256 * 4);
  // Quantize once on the host, including the inverse, before evaluating pixels.
  const prepared = ops.map((op) => ({ ...op, opacityQ: op.opacity === undefined ? 0n : cqOpacity(op.opacity), q: op.q ?? null }));
  const on = (o, i, end) => { const c = 2 * o + (2 * i + 1) * scaleI; return c >= 0 && c < 2 * end; };
  for (let y = 0; y < 256; y++) for (let x = 0; x < 256; x++) {
    const stack = []; let accum = [0n, 0n, 0n, 0n];
    if (on(originPx[0], x, width) && on(originPx[1], y, height)) for (const op of prepared) {
      if (op.t === "push") { stack.push(accum); accum = [0n, 0n, 0n, 0n]; }
      else if (op.t === "pop") {
        const s = cqStraight(accum), sa = cqMul(accum[3], op.opacityQ);
        accum = compositeQ14Blend(stack.pop(), s, s.map((v) => cqMul(v, sa)), sa, op.mode);
      } else {
        if (!op.q) throw new Error("v3 refuses a draw without a Q16 inverse");
        const bytes = axisUnitQ16(op.q.m)
          ? fetchNearest(op.tiles, op.q, originPx, scaleI, x, y)
          : fetchInteger(op.tiles, op.q, originPx, scaleI, x, y, true);
        const s = bytes.slice(0, 3).map((v) => BigInt(byteQ14(v)));
        const pre = bytes.slice(0, 3).map((v) => cqMul(BigInt(premulByteQ14(v, bytes[3])), op.opacityQ));
        const sa = cqMul(BigInt(byteQ14(bytes[3])), op.opacityQ);
        accum = compositeQ14Blend(accum, s, pre, sa, op.mode);
      }
    }
    const i = (y * 256 + x) * 4;
    for (let c = 0; c < 4; c++) out[i + c] = Math.min(255, Math.max(0, Number(cqDiv(accum[c] * 255n))));
  }
  return out;
}

/**
 * Render version 3 plain tile (the integer tile compositor) for the real-renderer
 * cases: `layers` are `{ tiles: [{ tx, ty, pixels }], transform, opacity, mode }`,
 * bottom first, under a root group that does not isolate; the tile has origin
 * `originPx` at level 0 and the canvas is `[0, width) x [0, height)`.
 */
export function renderPlainV3(layers, width, height, originPx) {
  const ops = [];
  for (const l of layers) {
    const world = fixedAffine(l.transform);
    if (!world) throw Error("v3 transform out of range");
    const q = fixedInverse(world);
    if (q === undefined) throw Error("v3 transform has no Q16 inverse");
    if (q === null) continue;
    const tiles = [...l.tiles].sort((a, b) => a.tx - b.tx || a.ty - b.ty);
    ops.push({ t: "draw", q, opacity: f(l.opacity), mode: l.mode, tiles });
  }
  return compositeInteger(ops, width, height, originPx, 1);
}

/** Every v3 scene the host draws is integer-exact and enforced; refused scenes are not. */
export function isV3IntegerCase(row) {
  return row.renderPath !== "v3-unsupported";
}

/** Integer cached-pyramid reference. Children are TL, TR, BL, BR. */
export function downsampleQ14(children) {
  const out = new Uint8Array(256*256*4);
  for (let y=0;y<256;y++) for(let x=0;x<256;x++) for(let c=0;c<4;c++) {
    const child=children[Math.floor(y/128)*2+Math.floor(x/128)];
    const i=((y%128*2)*256+x%128*2)*4+c;
    out[(y*256+x)*4+c]=Math.floor((child[i]+child[i+4]+child[i+1024]+child[i+1028]+2)/4);
  }
  return out;
}
export function renderPyramidCase(testCase) {
  if (testCase.level <= 0) return renderCase(testCase,3);
  return downsampleQ14(Array.from({length:4},(_,i)=>renderPyramidCase({...testCase,level:testCase.level-1,tx:testCase.tx*2+i%2,ty:testCase.ty*2+Math.floor(i/2)})));
}
