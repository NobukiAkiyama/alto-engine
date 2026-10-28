// Q14 helpers for render version 3. 1.0 = 16384. Shared by the integer tile
// compositor and the integer M5 stages; v1 and v2 shaders do not include this.
fn div_q(n: i32, d: i32) -> i32 {
  let half = d / 2;
  if n >= 0 { return (n + half) / d; }
  return -(((-n) + half) / d);
}
// Same 64-bit pair as composite.wgsl `Wide`. The integer tile compositor takes
// that copy and only the functions from `wide_div_q14` on. Factors here are far
// from i32::MIN, which `wide_mul_i32` cannot negate.
struct Wide { lo: u32, hi: u32 };
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
fn wide_negative(a: Wide) -> bool { return (a.hi & 0x80000000u) != 0u; }
// Divide by 16384, half away from zero. The magnitude is shifted; an arithmetic
// shift of a negative value is not `div_q`. The quotient fits in i32.
fn wide_div_q14(n: Wide) -> i32 {
  let neg = wide_negative(n);
  var mag = n;
  if neg { mag = wide_neg(n); }
  let adj = wide_add(mag, Wide(8192u, 0u));
  let lo = (adj.lo >> 14u) | (adj.hi << 18u);
  var q = i32(lo);
  if neg { q = -q; }
  return q;
}
// The unorm-to-float conversion is a division, so it can miss by 2.5 ULP.
// Half a byte is about 0.002, far outside that error, so the byte comes back.
fn unorm_byte(v: f32) -> u32 {
  return u32(clamp(v, 0.0, 1.0) * 255.0 + 0.5);
}
fn byte_q14(b: u32) -> u32 {
  return min((b * 16384u + 127u) / 255u, 16384u);
}
fn premul_byte_q14(color: u32, alpha: u32) -> u32 {
  return min((color * alpha * 16384u + 32512u) / 65025u, 16384u);
}
