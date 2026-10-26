/**
 * f32 port of composite.wgsl and the Rust CPU reference in
 * crates/alto-gpu/src/cpu_composite.rs. Every arithmetic result is Math.fround.
 * World matrices stay f64, then each inverse component is rounded to f32.
 * The browser suite compares these bytes with production readTile output.
 */
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
  base("scale-bilinear", [raster(ramp(256, 256), 0, 1, true, [2, 0, 0, 2, 0, 0])], { w: 512, h: 512, rounding: true });
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
  // SwiftShader matches this JS matrix bit-exactly. Native Metal and lavapipe still
  // miss the Rust reference by at most 1/255, so only the Rust xfail keeps rotation.
  // The hash file omits it because Math.cos can differ from f64::cos by an ulp.
  base("rotation", [raster(ramp(256, 256), 0, 1, true, [c, s, -s, c, 32, 24])], { w: 512, h: 512, lock: false });
  return cases;
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
function fetch(tiles, inv, p) {
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
function inverse(m) {
  const det = m[0] * m[3] - m[1] * m[2];
  if (!Number.isFinite(det) || Math.abs(det) < 1e-12) return null;
  return [m[3] / det, -m[1] / det, -m[2] / det, m[0] / det, (m[2] * m[5] - m[3] * m[4]) / det, (m[1] * m[4] - m[0] * m[5]) / det].map(f);
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
    const inv = inverse(world);
    if (!inv) return;
    ops.push({ t: "draw", inv, opacity: f(node.opacity), mode: node.mode, tiles: splitTiles(node.image) });
  }
}
function composite(ops, width, height, origin, scale) {
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
          const s = fetch(op.tiles, op.inv, p);
          accum = blend(accum, [s[0], s[1], s[2], mul(s[3], op.opacity)], op.mode);
        }
      }
    }
    const i = (y * 256 + x) * 4;
    for (let c = 0; c < 4; c++) out[i + c] = quantize(accum[c]);
  }
  return out;
}

export function renderCase(testCase) {
  const ops = [];
  for (const node of testCase.nodes) walk(node, IDENT, ops);
  const scale = f(2 ** testCase.level);
  const origin = [f(testCase.tx * 256 * scale), f(testCase.ty * 256 * scale)];
  return composite(ops, testCase.w, testCase.h, origin, scale);
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
