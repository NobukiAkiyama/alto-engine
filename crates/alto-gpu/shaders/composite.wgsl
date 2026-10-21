struct Command { inv0: vec4f, inv1: vec4f, info: vec4u };
struct Tile { xy: vec2i, slot: u32, pad: u32 };
struct Config { origin_scale: vec4f, info: vec4u };
@group(0) @binding(0) var atlas: texture_2d_array<f32>;
@group(0) @binding(1) var output: texture_storage_2d_array<rgba8unorm, write>;
@group(0) @binding(2) var<storage, read> commands: array<Command>;
@group(0) @binding(3) var<storage, read> tiles: array<Tile>;
@group(0) @binding(4) var<uniform> config: Config;
fn lum3(c: vec3f) -> f32 { return dot(c, vec3f(0.2126, 0.7152, 0.0722)); }
fn clip_color(c: vec3f) -> vec3f { let l = lum3(c); let n = min(c.r, min(c.g, c.b)); let x = max(c.r, max(c.g, c.b)); var o = c; if n < 0.0 { o = vec3f(l) + (c - vec3f(l)) * l / max(l - n, 0.000001); } if x > 1.0 { o = vec3f(l) + (o - vec3f(l)) * (1.0 - l) / max(x - l, 0.000001); } return clamp(o, vec3f(0.0), vec3f(1.0)); }
fn set_lum(c: vec3f, l: f32) -> vec3f { return clip_color(c + (vec3f(l) - vec3f(lum3(c)))); }
fn set_sat(c: vec3f, s: f32) -> vec3f { let mn = min(c.r, min(c.g, c.b)); let mx = max(c.r, max(c.g, c.b)); if mx <= mn { return vec3f(0.0); } return (c - vec3f(mn)) * s / max(mx - mn, 0.000001); }
fn soft_channel(ss: f32, dd: f32) -> f32 { let d2 = select(sqrt(dd), ((16.0 * dd - 12.0) * dd + 4.0) * dd, dd <= 0.25); if ss <= 0.5 { return dd - (1.0 - 2.0 * ss) * dd * (1.0 - dd); } return dd + (2.0 * ss - 1.0) * (d2 - dd); }
fn dodge(ss: f32, dd: f32) -> f32 { if ss >= 1.0 { return 1.0; } return min(1.0, dd / max(1.0 - ss, 0.000001)); }
fn burn(ss: f32, dd: f32) -> f32 { if ss <= 0.0 { return 0.0; } return 1.0 - min(1.0, (1.0 - dd) / max(ss, 0.000001)); }
fn blend(dst: vec4f, src: vec4f, mode: u32) -> vec4f {
  var b = src.rgb;
  let d = select(vec3f(0.0), dst.rgb / max(dst.a, 0.000001), dst.a > 0.0);
  let s = src.rgb;
  if mode == 1u { b = d * s; }
  else if mode == 2u { b = 1.0 - (1.0 - d) * (1.0 - s); }
  else if mode == 3u { b = select(1.0 - 2.0 * (1.0 - s) * (1.0 - d), 2.0 * s * d, d < vec3f(0.5)); }
  else if mode == 4u { b = min(s, d); }
  else if mode == 5u { b = max(s, d); }
  else if mode == 6u { b = vec3f(dodge(s.r, d.r), dodge(s.g, d.g), dodge(s.b, d.b)); }
  else if mode == 7u { b = vec3f(burn(s.r, d.r), burn(s.g, d.g), burn(s.b, d.b)); }
  else if mode == 8u { b = select(1.0 - 2.0 * (1.0 - d) * (1.0 - s), 2.0 * d * s, s < vec3f(0.5)); }
  else if mode == 9u { b = vec3f(soft_channel(s.r, d.r), soft_channel(s.g, d.g), soft_channel(s.b, d.b)); }
  else if mode == 10u { b = abs(d - s); }
  else if mode == 11u { b = d + s - 2.0 * d * s; }
  else if mode == 12u { b = set_lum(set_sat(s, max(d.r, max(d.g, d.b)) - min(d.r, min(d.g, d.b))), lum3(d)); }
  else if mode == 13u { b = set_lum(set_sat(d, max(s.r, max(s.g, s.b)) - min(s.r, min(s.g, s.b))), lum3(d)); }
  else if mode == 14u { b = set_lum(s, lum3(d)); }
  else if mode == 15u { b = set_lum(d, lum3(s)); }
  else if mode == 16u { b = min(vec3f(1.0), s + d); }
  let rgb = (1.0-src.a)*dst.rgb + (1.0-dst.a)*src.rgb*src.a + dst.a*src.a*b;
  return vec4f(rgb, src.a + dst.a*(1.0-src.a));
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
fn fetch(c: Command, p: vec2f) -> vec4f {
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
      } else { let s=fetch(c,p); accum=blend(accum,vec4f(s.rgb,s.a*c.inv1.z),u32(c.inv1.w)); }
    }
  }
  textureStore(output,vec2i(id.xy),i32(config.info.y),accum);
}
