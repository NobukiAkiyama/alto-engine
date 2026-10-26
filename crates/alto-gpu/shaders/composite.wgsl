struct Command { inv0: vec4f, inv1: vec4f, info: vec4u, trans: vec4u };
struct Tile { xy: vec2i, slot: u32, pad: u32 };
struct Config { origin_scale: vec4f, info: vec4u };
struct Wide { lo: u32, hi: u32 };
@group(0) @binding(0) var atlas: texture_2d_array<f32>;
@group(0) @binding(1) var output: texture_storage_2d_array<rgba8unorm, write>;
@group(0) @binding(2) var<storage, read> commands: array<Command>;
@group(0) @binding(3) var<storage, read> tiles: array<Tile>;
@group(0) @binding(4) var<uniform> config: Config;

// Render version 2 integer 4-tap. See crates/alto-gpu/src/sample.rs.
// Q16 matrix * Q8 position is Q24. Translation is i64 Q16 in `trans`
// (lo, hi, lo, hi), shifted to Q24. Round half away from zero into Q8
// (add or subtract 2^15, arithmetic >> 16). The low 8 bits are the weights.
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
fn wide_round_shr16(a: Wide) -> i32 {
  let neg = (a.hi & 0x80000000u) != 0u;
  let half = Wide(32768u, 0u);
  // Naga rejects select() on a user struct, so the half-away branch is explicit.
  var adj: Wide;
  if neg { adj = wide_sub(a, half); } else { adj = wide_add(a, half); }
  return bitcast<i32>((adj.lo >> 16u) | (adj.hi << 16u));
}
fn coord_q8(m0: i32, m1: i32, t: Wide, px: i32, py: i32) -> i32 {
  let sum = wide_add(wide_add(wide_mul_i32(m0, px), wide_mul_i32(m1, py)), wide_shl8(t));
  return wide_round_shr16(sum);
}
fn sample_texel(c: Command, q: vec2f) -> vec4f {
  let key = vec2i(floor(q / 256.0));
  var lo = c.info.y; var hi = lo+c.info.z;
  loop {
    if lo >= hi { break; }
    let mid = (lo+hi)/2u; let t = tiles[mid];
    if t.xy.x < key.x || (t.xy.x == key.x && t.xy.y < key.y) { lo=mid+1u; } else { hi=mid; }
  }
  if lo >= c.info.y+c.info.z { return vec4f(0.0); }
  let t=tiles[lo];
  if any(t.xy != key) { return vec4f(0.0); }
  let local = clamp(vec2i(floor(q))-key*256, vec2i(0), vec2i(255));
  if t.slot == 0xffffffffu {
    return vec4f(1.0, select(0.0,0.25,((local.x/12+local.y/12)%2)==0),0.2,1.0);
  }
  return textureLoad(atlas, local, i32(t.slot), 0);
}
fn sample_byte(c: Command, x: i32, y: i32) -> vec4u {
  let key = vec2i(x >> 8, y >> 8);
  var lo = c.info.y; var hi = lo+c.info.z;
  loop {
    if lo >= hi { break; }
    let mid = (lo+hi)/2u; let t = tiles[mid];
    if t.xy.x < key.x || (t.xy.x == key.x && t.xy.y < key.y) { lo=mid+1u; } else { hi=mid; }
  }
  if lo >= c.info.y+c.info.z { return vec4u(0u); }
  let t=tiles[lo];
  if any(t.xy != key) { return vec4u(0u); }
  let local = clamp(vec2i(x, y)-key*256, vec2i(0), vec2i(255));
  if t.slot == 0xffffffffu {
    let on = ((local.x/12+local.y/12)%2)==0;
    return vec4u(255u, select(0u, 64u, on), 51u, 255u);
  }
  let s = textureLoad(atlas, local, i32(t.slot), 0);
  return vec4u(
    u32(clamp(round(s.r * 255.0), 0.0, 255.0)),
    u32(clamp(round(s.g * 255.0), 0.0, 255.0)),
    u32(clamp(round(s.b * 255.0), 0.0, 255.0)),
    u32(clamp(round(s.a * 255.0), 0.0, 255.0)),
  );
}
fn channel_byte(a: u32, b: u32, d: u32, e: u32, fx: i32, fy: i32) -> u32 {
  let ab = i32(a) * (256 - fx) + i32(b) * fx;
  let de = i32(d) * (256 - fx) + i32(e) * fx;
  let acc = ab * (256 - fy) + de * fy;
  return u32((acc + 32768) >> 16);
}
fn fetch_legacy(c: Command, p: vec2f) -> vec4f {
  let q = vec2f(c.inv0.x*p.x+c.inv0.z*p.y+c.inv1.x, c.inv0.y*p.x+c.inv0.w*p.y+c.inv1.y);
  let axis = abs(c.inv0.y) < 0.0001 && abs(c.inv0.z) < 0.0001;
  let unit = abs(abs(c.inv0.x) - 1.0) < 0.0001 && abs(abs(c.inv0.w) - 1.0) < 0.0001;
  if axis && unit { return sample_texel(c, q); }
  let base = floor(q);
  let f = fract(q);
  let a = sample_texel(c, base);
  let b = sample_texel(c, base + vec2f(1.0, 0.0));
  let d = sample_texel(c, base + vec2f(0.0, 1.0));
  let e = sample_texel(c, base + vec2f(1.0, 1.0));
  return mix(mix(a, b, f.x), mix(d, e, f.x), f.y);
}
fn fetch_integer(c: Command, ix: vec2i) -> vec4f {
  let scale = i32(config.origin_scale.z);
  let px = i32(config.origin_scale.x) * 256 + (ix.x * 256 + 128) * scale;
  let py = i32(config.origin_scale.y) * 256 + (ix.y * 256 + 128) * scale;
  let m = bitcast<vec4<i32>>(c.inv0);
  let tx = Wide(c.trans.x, c.trans.y);
  let ty = Wide(c.trans.z, c.trans.w);
  let qx = coord_q8(m.x, m.z, tx, px, py);
  let qy = coord_q8(m.y, m.w, ty, px, py);
  let x = qx >> 8;
  let y = qy >> 8;
  let fx = qx - (x << 8);
  let fy = qy - (y << 8);
  let a = sample_byte(c, x, y);
  let b = sample_byte(c, x + 1, y);
  let d = sample_byte(c, x, y + 1);
  let e = sample_byte(c, x + 1, y + 1);
  let r = channel_byte(a.x, b.x, d.x, e.x, fx, fy);
  let g = channel_byte(a.y, b.y, d.y, e.y, fx, fy);
  let bl = channel_byte(a.z, b.z, d.z, e.z, fx, fy);
  let al = channel_byte(a.w, b.w, d.w, e.w, fx, fy);
  return vec4f(f32(r), f32(g), f32(bl), f32(al)) / 255.0;
}
fn fetch(c: Command, p: vec2f, ix: vec2i) -> vec4f {
  if c.info.w == 0u { return fetch_legacy(c, p); }
  return fetch_integer(c, ix);
}
@compute @workgroup_size(16,16)
fn main(@builtin(global_invocation_id) id: vec3u) {
  if any(id.xy >= vec2u(256u)) { return; }
  let p=config.origin_scale.xy+(vec2f(id.xy)+0.5)*config.origin_scale.z;
  var stack: array<vec4f,32>; var depth=0u; var accum=vec4f(0.0);
  if p.x>=0.0 && p.y>=0.0 && p.x<config.origin_scale.w && p.y<f32(config.info.z) {
    for(var i=0u;i<config.info.x;i++) {
      let c=commands[i];
      if c.info.x==1u { stack[depth]=accum; depth++; accum=vec4f(0.0); }
      else if c.info.x==2u {
        let src=vec4f(select(vec3f(0.0),accum.rgb/max(accum.a,0.000001),accum.a>0.0),accum.a*c.inv1.z);
        depth--; accum=blend(stack[depth],src,u32(c.inv1.w));
      } else { let s=fetch(c,p,vec2i(id.xy)); accum=blend(accum,vec4f(s.rgb,s.a*c.inv1.z),u32(c.inv1.w)); }
    }
  }
  textureStore(output,vec2i(id.xy),i32(config.info.y),accum);
}
