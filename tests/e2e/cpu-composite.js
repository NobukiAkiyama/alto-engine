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
function quantizeQ16(v) {
  if (!Number.isFinite(v)) return null;
  const scaled = v * 65536;
  if (!Number.isFinite(scaled) || Math.abs(scaled) >= F64_EXACT_INT) return null;
  return scaled >= 0 ? Math.floor(scaled + 0.5) : Math.ceil(scaled - 0.5);
}
function quantizeInverse(inv) {
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
  const q = quantizeInverse(inv);
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
  const world = multiply(parent, node.transform);
  if (node.type === "group") {
    const isolate = node.isolated || opacity < 1 || node.mode !== 0;
    if (isolate) ops.push({ t: "push" });
    for (const child of node.children) walk(child, world, ops, version);
    if (isolate) ops.push({ t: "pop", opacity: f(node.opacity), mode: node.mode });
  } else {
    const inv = inverseF64(world);
    if (!inv) return;
    ops.push({ t: "draw", inv, opacity: f(node.opacity), mode: node.mode, tiles: splitTiles(node.image) });
  }
}
function composite(ops, width, height, origin, scale, originPx, scaleI, version) {
  if (version === 3) return compositeInteger(ops, width, height, origin, scale, originPx, scaleI);
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
  for (const node of testCase.nodes) walk(node, IDENT, ops, version);
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

/** Q14 of a color-adjust setting. 1 is 16384. Half away from zero. */
function colorAdjustQ14(value) {
  if (!Number.isFinite(value)) return 0;
  const clamped = f(Math.min(4, Math.max(-1, value)));
  const x = f(clamped * 16384);
  const adj = x < 0 || Object.is(x, -0) ? -0.5 : 0.5;
  return Math.trunc(f(x + adj));
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

/**
 * Render version 3 M5 reference for a flat stack at the identity transform, as
 * `renderer/m5.rs` dispatches it. `layers` is bottom first. A raster is
 * `{ image, opacity, mode, effects }` (color-adjust settings only) and composites
 * with mode 19; `{ adjust, opacity }` is a color-adjust adjustment layer (mode 17,
 * then mode 20 when its opacity is below 1).
 */
export function renderM5V3(layers, w = 256, h = 256) {
  const out = new Uint8Array(w * h * 4);
  const Q = 16384n;
  const big = (q) => q.map(BigInt);
  for (let i = 0; i < out.length; i += 4) {
    let accum = [0n, 0n, 0n, 0n];
    for (const layer of layers) {
      const opacity = cqOpacity(layer.opacity ?? 1);
      if (layer.adjust) {
        const adjusted = big(colorAdjustStageQ14(accum.map(Number), layer.adjust));
        accum = opacity === Q ? adjusted : accum.map((v, c) => cqDiv(v * (Q - opacity) + adjusted[c] * opacity));
        continue;
      }
      let q = ingressQ14(Array.from(layer.image.pixels.subarray(i, i + 4)));
      for (const effect of layer.effects || []) q = colorAdjustStageQ14(q, effect);
      const src = big(q);
      const mode = layer.mode ?? 0;
      const straight = cqStraight(src);
      if (opacity === Q) accum = compositeQ14Blend(accum, straight, src.slice(0, 3), src[3], mode);
      else {
        const sa = cqMul(src[3], opacity);
        accum = compositeQ14Blend(accum, straight, straight.map((v) => cqMul(v, sa)), sa, mode);
      }
    }
    out.set(resolveQ14(accum), i);
  }
  return out;
}

export function fnv64(bytes) {
  let h = 0xcbf29ce484222325n;
  const prime = 0x100000001b3n;
  const mask = 0xffffffffffffffffn;
  for (const b of bytes) h = ((h ^ BigInt(b)) * prime) & mask;
  return h.toString(16).padStart(16, "0");
}

const GAP_ORDER = ["mask", "vector mask", "clip path", "clip", "effects", "vector", "text", "adjustment", "unsupported"];

const isColorAdjust = (effect) => effect?.type === "color_adjust" && !effect.selection;

/**
 * Why a document is outside the plain composite.wgsl path, or null when it is on it.
 * Render version 3 has exactly three classes, mirroring Rust `renderer::v3_gap`:
 * null (integer tile compositor), "v3-m5-integer" (integer M5 stages: color-adjust
 * effects and color-adjust adjustment layers) and "v3-unsupported" (the host
 * refuses the scene; no tile is drawn).
 */
export function sceneGap(snapshot) {
  const found = new Set();
  let unsupported = false;
  for (const node of snapshot.nodes.values()) {
    const props = node.props || {};
    if (props.mask) found.add("mask");
    if (props.vector_mask) found.add("vector mask");
    if (props.clip_path) found.add("clip path");
    if (props.clip_to_below) found.add("clip");
    if (props.effects?.length) found.add("effects");
    if (node.type === "vector") found.add("vector");
    if (node.type === "text") found.add("text");
    if (node.type === "adjustment") found.add("adjustment");
    if (node.type === "unsupported") found.add("unsupported");
    if (props.mask || props.vector_mask || props.clip_path || props.clip_to_below ||
        node.type === "vector" || node.type === "text" ||
        (props.effects || []).some((effect) => !isColorAdjust(effect)) ||
        (node.type === "adjustment" && (!isColorAdjust(node.adjustment) || (props.blend ?? "normal") !== "normal"))) {
      unsupported = true;
    }
  }
  if (snapshot.meta?.render_version === 3) {
    if (unsupported) return "v3-unsupported";
    if (found.has("effects") || found.has("adjustment")) return "v3-m5-integer";
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
const cqOpacity = (v) => BigInt(Math.trunc(f(f(f(Math.min(1, Math.max(0, f(v)))) * 16384) + 0.5)));
function compositeInteger(ops, width, height, origin, scale, originPx, scaleI) {
  for (const op of ops) if (op.mode >= 17) throw new Error(`unspecified blend mode ${op.mode}`);
  const out = new Uint8Array(256 * 256 * 4);
  // Quantize once on the host, including the inverse, before evaluating pixels.
  const prepared = ops.map((op) => ({ ...op, opacityQ: op.opacity === undefined ? 0n : cqOpacity(op.opacity), q: op.inv ? quantizeInverse(op.inv) : null }));
  for (let y = 0; y < 256; y++) for (let x = 0; x < 256; x++) {
    const p = [add(origin[0], mul(add(f(x), f(0.5)), scale)), add(origin[1], mul(add(f(y), f(0.5)), scale))];
    const stack = []; let accum = [0n, 0n, 0n, 0n];
    if (p[0] >= 0 && p[1] >= 0 && p[0] < width && p[1] < height) for (const op of prepared) {
      if (op.t === "push") { stack.push(accum); accum = [0n, 0n, 0n, 0n]; }
      else if (op.t === "pop") {
        const s = cqStraight(accum), sa = cqMul(accum[3], op.opacityQ);
        accum = compositeQ14Blend(stack.pop(), s, s.map((v) => cqMul(v, sa)), sa, op.mode);
      } else {
        const inv = op.inv.map(f);
        let bytes;
        if (axisUnit(inv)) {
          const q = [add(add(mul(inv[0], p[0]), mul(inv[2], p[1])), inv[4]), add(add(mul(inv[1], p[0]), mul(inv[3], p[1])), inv[5])];
          bytes = sampleU8(op.tiles, Math.floor(q[0]), Math.floor(q[1]));
        } else {
          if (!op.q) throw new Error("v3 filtered f32 fallback is unspecified");
          bytes = fetchInteger(op.tiles, op.q, originPx, scaleI, x, y, true);
        }
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
