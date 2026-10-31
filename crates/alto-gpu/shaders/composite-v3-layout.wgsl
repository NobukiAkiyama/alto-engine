struct Command { inv0: vec4i, unused: vec2u, opacity_q: u32, mode: u32, info: vec4u, trans: vec4u };
struct Tile { xy: vec2i, slot: u32, pad: u32 };
struct Config { origin_scale: vec4i, info: vec4u, span: vec4u };
struct Wide { lo: u32, hi: u32 };
struct WideSum { v: Wide, ok: u32 };
// `ok == 0` means the Q8 value does not fit in i32. Callers return transparent.
struct Q8Fit { v: i32, ok: u32 };
@group(0) @binding(0) var atlas: texture_2d_array<f32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<storage, read> commands: array<Command>;
@group(0) @binding(3) var<storage, read> tiles: array<Tile>;
@group(0) @binding(4) var<uniform> config: Config;

// Render version 2 integer 4-tap. See crates/alto-gpu/src/sample.rs.
// Q16 matrix * Q8 position is Q24. Translation is i64 Q16 in `trans`
// (lo, hi, lo, hi), shifted to Q24. Round half away from zero into Q8
// (add or subtract 2^15, arithmetic >> 16). The low 8 bits are the weights.
// A rounded Q8 coordinate, or a Q8 pixel center, that does not fit in i32
// is out of range: the sample is transparent. It is not wrapped.
// Channel bytes use round half up: (acc + 32768) >> 16.
fn wide_neg(a: Wide) -> Wide {
  let lo = ~a.lo + 1u;
  let carry = select(0u, 1u, lo == 0u);
  return Wide(lo, ~a.hi + carry);
}
fn wide_add(a: Wide, b: Wide) -> Wide {
  let lo = a.lo + b.lo;
  let carry = select(0u, 1u, lo < a.lo);
  return Wide(lo, a.hi + b.hi + carry);
}
fn wide_sub(a: Wide, b: Wide) -> Wide { return wide_add(a, wide_neg(b)); }
fn wide_mul_u32(a: u32, b: u32) -> Wide {
  let a0 = a & 0xffffu; let a1 = a >> 16u;
  let b0 = b & 0xffffu; let b1 = b >> 16u;
  let p00 = a0 * b0; let p01 = a0 * b1; let p10 = a1 * b0; let p11 = a1 * b1;
  let mid = (p00 >> 16u) + (p01 & 0xffffu) + (p10 & 0xffffu);
  let lo = (p00 & 0xffffu) | (mid << 16u);
  let hi = p11 + (p01 >> 16u) + (p10 >> 16u) + (mid >> 16u);
  return Wide(lo, hi);
}
fn wide_mul_i32(a: i32, b: i32) -> Wide {
  let neg = (a < 0) != (b < 0);
  var ua = bitcast<u32>(a);
  var ub = bitcast<u32>(b);
  if a < 0 { ua = bitcast<u32>(-a); }
  if b < 0 { ub = bitcast<u32>(-b); }
  let p = wide_mul_u32(ua, ub);
  if neg { return wide_neg(p); }
  return p;
}
fn wide_shl8(a: Wide) -> Wide {
  return Wide(a.lo << 8u, (a.hi << 8u) | (a.lo >> 24u));
}
fn wide_negative(a: Wide) -> bool { return (a.hi & 0x80000000u) != 0u; }
// Signed 64-bit add. `ok == 0` when the sum does not fit in i64.
fn wide_add_checked(a: Wide, b: Wide) -> WideSum {
  let sum = wide_add(a, b);
  let same = wide_negative(a) == wide_negative(b);
  var ok = 1u;
  if same && wide_negative(a) != wide_negative(sum) { ok = 0u; }
  return WideSum(sum, ok);
}
// Rounded Q8. `ok == 0` when the i64 sum overflows or the result does not fit in i32.
fn wide_round_shr16(a: Wide) -> Q8Fit {
  let neg = wide_negative(a);
  let half = Wide(32768u, 0u);
  // Naga rejects select() on a user struct, so the half-away branch is explicit.
  var adj: WideSum;
  if neg { adj = wide_add_checked(a, wide_neg(half)); } else { adj = wide_add_checked(a, half); }
  if adj.ok == 0u { return Q8Fit(0, 0u); }
  // Bits above the i32 sign must be a copy of that sign. Otherwise the Q8
  // coordinate is outside ±2^31 and wrapping the low 32 bits would sample
  // an unrelated texel.
  let top = adj.v.hi >> 16u;
  let sign = (adj.v.hi & 0x8000u) != 0u;
  var ok = 0u;
  if sign {
    if top == 0xffffu { ok = 1u; }
  } else {
    if top == 0u { ok = 1u; }
  }
  return Q8Fit(bitcast<i32>((adj.v.lo >> 16u) | (adj.v.hi << 16u)), ok);
}
fn coord_q8(m0: i32, m1: i32, t: Wide, px: i32, py: i32) -> Q8Fit {
  let a = wide_add_checked(wide_mul_i32(m0, px), wide_mul_i32(m1, py));
  if a.ok == 0u { return Q8Fit(0, 0u); }
  let sum = wide_add_checked(a.v, wide_shl8(t));
  if sum.ok == 0u { return Q8Fit(0, 0u); }
  return wide_round_shr16(sum.v);
}
// Q8 pixel center. `origin * 256 + (index * 256 + 128) * scale` overflows i32
// once the tile origin passes ±2^23 px. That sample is transparent.
fn pixel_q8(origin: i32, index: i32, scale: i32) -> Q8Fit {
  let term = index * 256 + 128;
  let sum = wide_add_checked(wide_mul_i32(origin, 256), wide_mul_i32(term, scale));
  if sum.ok == 0u { return Q8Fit(0, 0u); }
  // The low 32 bits are an i32 only when the high half is their sign extension.
  let lo_neg = (sum.v.lo & 0x80000000u) != 0u;
  var ok = 0u;
  if lo_neg {
    if sum.v.hi == 0xffffffffu { ok = 1u; }
  } else {
    if sum.v.hi == 0u { ok = 1u; }
  }
  return Q8Fit(bitcast<i32>(sum.v.lo), ok);
}
