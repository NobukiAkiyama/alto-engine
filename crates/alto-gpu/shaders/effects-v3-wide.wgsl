struct WideSum { v: Wide, ok: u32 };
// `ok == 0` means the Q8 value does not fit in i32. Callers return transparent.
struct Q8Fit { v: i32, ok: u32 };
fn wide_shl8(a: Wide) -> Wide {
  return Wide(a.lo << 8u, (a.hi << 8u) | (a.lo >> 24u));
}
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
