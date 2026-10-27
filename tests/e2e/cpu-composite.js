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
export function colorAdjustGrid() {
  const contrast = [0.5, 1.7];
  const saturation = [0, 0.35, 1.6];
  const brightness = [-0.35, 0.45];
  const grid = [];
  for (const contrastValue of contrast) {
    for (const saturationValue of saturation) {
      for (const brightnessValue of brightness) {
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
function fetchInteger(tiles, q, originPx, scale, x, y) {
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
  return [0, 1, 2, 3].map((i) => div(f(bilinearByte(a[i], b[i], d[i], e[i], fx, fy)), f(255)));
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
function walk(node, parent, ops) {
  if (!node.visible || node.opacity <= 0) return;
  const world = multiply(parent, node.transform);
  if (node.type === "group") {
    const isolate = node.isolated || node.opacity < 1 || node.mode !== 0;
    if (isolate) ops.push({ t: "push" });
    for (const child of node.children) walk(child, world, ops);
    if (isolate) ops.push({ t: "pop", opacity: f(node.opacity), mode: node.mode });
  } else {
    const inv = inverseF64(world);
    if (!inv) return;
    ops.push({ t: "draw", inv, opacity: f(node.opacity), mode: node.mode, tiles: splitTiles(node.image) });
  }
}
function composite(ops, width, height, origin, scale, originPx, scaleI, version) {
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
  const ops = [];
  for (const node of testCase.nodes) walk(node, IDENT, ops);
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

/** `(n + d/2) / d` for `d > 0`, away from zero on a tie. */
function divQ(n, d) {
  const half = Math.trunc(d / 2);
  if (n >= 0) return Math.trunc((n + half) / d);
  return -Math.trunc(((-n) + half) / d);
}

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

function floorLog2(value) {
  let n = value >>> 0;
  let p = 0;
  if (n >= 256) { p += 8; n >>>= 8; }
  if (n >= 16) { p += 4; n >>>= 4; }
  if (n >= 4) { p += 2; n >>>= 2; }
  if (n >= 2) p += 1;
  return p;
}

function q14ToF16Bits(qIn) {
  if (qIn === 0) return 0;
  const q = Math.min(qIn >>> 0, Q14_ONE);
  if (q === Q14_ONE) return 0x3c00;
  const p = floorLog2(q);
  let sig = q;
  let expP = p;
  if (p >= 10) {
    const cut = p - 10;
    if (cut > 0) {
      const half = 1 << (cut - 1);
      const rem = sig & ((1 << cut) - 1);
      sig >>>= cut;
      if (rem > half || (rem === half && (sig & 1) === 1)) sig += 1;
    }
    if (sig >= 2048) { sig = 1024; expP = p + 1; }
  } else sig = q << (10 - p);
  return (((expP + 1) << 10) | (sig & 1023)) & 0xffff;
}

function f16BitsToQ14(half) {
  const h = half & 0xffff;
  if ((h & 0x8000) !== 0) return 0;
  const exp = (h >> 10) & 0x1f;
  const mant = h & 0x3ff;
  if (exp === 31) return mant === 0 ? Q14_ONE : 0;
  if (exp === 0) return 0;
  const sig = 1024 + mant;
  const q = exp >= 11 ? (sig << (exp - 11)) >>> 0 : sig >>> (11 - exp);
  return Math.min(q, Q14_ONE);
}

function colorAdjustExactQ14(q, brightness, contrast, saturation) {
  const alpha = q[3];
  const straight = [0, 0, 0];
  if (alpha !== 0) {
    for (let i = 0; i < 3; i++) {
      const s = Math.trunc((q[i] * Q14_ONE) / alpha);
      straight[i] = Math.min(Q14_ONE, Math.max(0, s));
    }
  }
  const rgb = straight.map((channel) => {
    const v = divQ((channel - Q14_HALF) * contrast, Q14_ONE) + Q14_HALF + brightness;
    return Math.min(32767, Math.max(-32768, v));
  });
  const luma = divQ(rgb[0] * Q14_LUMA[0] + rgb[1] * Q14_LUMA[1] + rgb[2] * Q14_LUMA[2], Q14_ONE);
  return rgb.map((channel) => {
    const mixed = divQ(luma * (Q14_ONE - saturation) + channel * saturation, Q14_ONE);
    return divQ(Math.min(Q14_ONE, Math.max(0, mixed)) * alpha, Q14_ONE);
  });
}

function colorAdjustTexelV3(rgba, brightness, contrast, saturation) {
  const alpha = rgba[3];
  const stored = [
    q14ToF16Bits(premulByteQ14(rgba[0], alpha)),
    q14ToF16Bits(premulByteQ14(rgba[1], alpha)),
    q14ToF16Bits(premulByteQ14(rgba[2], alpha)),
    q14ToF16Bits(byteQ14(alpha)),
  ];
  const rgb = colorAdjustExactQ14(
    stored.map(f16BitsToQ14),
    colorAdjustQ14(brightness),
    colorAdjustQ14(contrast),
    colorAdjustQ14(saturation),
  );
  return [
    quantize(f16BitsToF32(q14ToF16Bits(rgb[0]))),
    quantize(f16BitsToF32(q14ToF16Bits(rgb[1]))),
    quantize(f16BitsToF32(q14ToF16Bits(rgb[2]))),
    quantize(f16BitsToF32(stored[3])),
  ];
}

/** Render version 3: integer premultiply, Q14 color-adjust, unorm resolve. */
export function renderColorAdjustV3(image, settings = {}) {
  const brightness = f(settings.brightness ?? 0);
  const contrast = f(settings.contrast ?? 1);
  const saturation = f(settings.saturation ?? 1);
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

export function fnv64(bytes) {
  let h = 0xcbf29ce484222325n;
  const prime = 0x100000001b3n;
  const mask = 0xffffffffffffffffn;
  for (const b of bytes) h = ((h ^ BigInt(b)) * prime) & mask;
  return h.toString(16).padStart(16, "0");
}

const GAP_ORDER = ["mask", "vector mask", "clip path", "clip", "effects", "vector", "text", "adjustment", "unsupported"];

/** Why a document is outside the plain composite.wgsl path, or null when it is on it. */
export function sceneGap(snapshot) {
  const found = new Set();
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
  }
  return GAP_ORDER.find((gap) => found.has(gap)) || null;
}
