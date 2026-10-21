struct Command { inv0: vec4f, inv1: vec4f, info: vec4u };
struct Tile { xy: vec2i, slot: u32, pad: u32 };
struct Config { origin_scale: vec4f, info: vec4u };
@group(0) @binding(0) var atlas: texture_2d_array<f32>;
@group(0) @binding(1) var output: texture_storage_2d_array<rgba8unorm, write>;
@group(0) @binding(2) var<storage, read> commands: array<Command>;
@group(0) @binding(3) var<storage, read> tiles: array<Tile>;
@group(0) @binding(4) var<uniform> config: Config;
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
