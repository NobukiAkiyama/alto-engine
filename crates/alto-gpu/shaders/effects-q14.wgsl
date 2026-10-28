// Render version 3 M5 stages. Appended after q14.wgsl and blend-q14.wgsl.
// Every intermediate is premultiplied Q14 (0..=16384) in rgba16uint. No float
// texture is read or written; the atlas is read with textureLoad + unorm_byte.
// Bindings match effects.wgsl except that binding 2 (the clip base) is unused, so
// the derived layout omits it and the host does not bind it. The host
// rejects every v3 scene that needs a stage not listed here (renderer.rs
// `v3_gap`), so unknown modes are never dispatched and write zero.
// Modes 21, 22 and 23 are the pixel-mask stages; v2 modes 4, 5 and 7 in
// effects.wgsl are not changed. They take everything from the integer
// `grid` (renderer.rs `mask_grid`) and the integer pixel index: no float
// position, region or uniform. The only float is the atlas `.r`, which
// `unorm_byte` turns back into its byte.
struct Tile { xy: vec2i, slot: u32, pad: u32 };
// `grid` is the `curve` slot of effects.wgsl, which no v3 stage reads as a curve.
struct Config { out_region: vec4f, region_a: vec4f, region_b: vec4f, inv0: vec4f, inv1: vec4f, settings: vec4f, info: vec4u, extra: vec4f, grid: array<vec4i, 16>, sizes: vec4u, output_clip: vec4u };
@group(0) @binding(0) var a: texture_2d<u32>;
@group(0) @binding(1) var b: texture_2d<u32>;
@group(0) @binding(3) var atlas: texture_2d_array<f32>;
@group(0) @binding(4) var output: texture_storage_2d<rgba16uint,write>;
@group(0) @binding(5) var<storage,read> tiles: array<Tile>;
@group(0) @binding(6) var<uniform> c: Config;
const Q14_ONE: i32 = 16384;
const Q14_HALF: i32 = 8192;
fn load_a(p:vec2f)->vec4i { let q=vec2i(floor((p-c.region_a.xy)/c.region_a.z));if any(q<vec2i(0))||any(q>=vec2i(c.sizes.xy)){return vec4i(0);}return vec4i(textureLoad(a,q,0)); }
fn load_b(p:vec2f)->vec4i { let q=vec2i(floor((p-c.region_b.xy)/c.region_b.z));if any(q<vec2i(0))||any(q>=vec2i(c.sizes.zw)){return vec4i(0);}return vec4i(textureLoad(b,q,0)); }
// The same nearest lookup as effects.wgsl `sparse`, returning the stored bytes.
fn sparse_bytes(p:vec2f)->vec4u {
 let q=vec2f(c.inv0.x*p.x+c.inv0.z*p.y+c.inv1.x,c.inv0.y*p.x+c.inv0.w*p.y+c.inv1.y);
 let key=vec2i(floor(q/256.0));var lo=0u;var hi=c.info.z;
 loop {if lo>=hi{break;}let mid=(lo+hi)/2u;let t=tiles[mid];if t.xy.x<key.x||(t.xy.x==key.x&&t.xy.y<key.y){lo=mid+1u;}else{hi=mid;}}
 if lo>=c.info.z{return vec4u(0u);}let t=tiles[lo];if any(t.xy!=key){return vec4u(0u);}
 if t.slot==0xffffffffu{return vec4u(255u,0u,51u,255u);}
 let s=textureLoad(atlas,vec2i(floor(q))-key*256,i32(t.slot),0);
 return vec4u(unorm_byte(s.r),unorm_byte(s.g),unorm_byte(s.b),unorm_byte(s.a));
}
// Floor of n / d for d > 0. WGSL `/` truncates toward zero, so a negative
// quotient with a remainder is one less.
fn floor_div(n: i32, d: i32) -> i32 {
 let q=n/d;
 if n<0&&q*d!=n{return q-1;}
 return q;
}
// The input texel of output pixel `ix` in a mask stage, from the map `m`
// (renderer.rs `input_map`). `m.w == 0`: the input region is the output
// region, and the texel is `ix`. Otherwise it is `floor((m.xy + ix * m.z) / m.w)`,
// the pixel center over the input scale with every term doubled. The host
// refuses a region whose numerators leave i32.
fn input_texel(ix: vec2i, m: vec4i) -> vec2i {
 if m.w==0{return ix;}
 return vec2i(floor_div(m.x+ix.x*m.z,m.w),floor_div(m.y+ix.y*m.z,m.w));
}
fn texel_a(ix: vec2i)->vec4i { let q=input_texel(ix,c.grid[1]);if any(q<vec2i(0))||any(q>=vec2i(c.sizes.xy)){return vec4i(0);}return vec4i(textureLoad(a,q,0)); }
fn texel_b(ix: vec2i)->vec4i { let q=input_texel(ix,c.grid[2]);if any(q<vec2i(0))||any(q>=vec2i(c.sizes.zw)){return vec4i(0);}return vec4i(textureLoad(b,q,0)); }
// The mask byte at output pixel `ix`, nearest only (doc/design/2026-10-28-v3-integer-mask.md).
// Always the Q16 lookup (sample.rs SAMPLE_INTEGER); there is no f32 route, and
// the host refuses a mask it cannot express this way. `grid[3]` is the output
// ROI origin and scale, `grid[4]` the Q16 inverse and `grid[5]` the i64 Q16
// translation (lo, hi, lo, hi). The texel is the arithmetic `>> 8` of the Q8
// position; there are no weights. Out of range is byte 0.
fn mask_texel(x: i32, y: i32) -> u32 {
 let key=vec2i(x>>8,y>>8);var lo=0u;var hi=c.info.z;
 loop {if lo>=hi{break;}let mid=(lo+hi)/2u;let t=tiles[mid];if t.xy.x<key.x||(t.xy.x==key.x&&t.xy.y<key.y){lo=mid+1u;}else{hi=mid;}}
 if lo>=c.info.z{return 0u;}let t=tiles[lo];if any(t.xy!=key){return 0u;}
 if t.slot==0xffffffffu{return 255u;}
 return unorm_byte(textureLoad(atlas,vec2i(x,y)-key*256,i32(t.slot),0).r);
}
fn mask_byte(ix: vec2i) -> u32 {
 let o=c.grid[3];
 let px=pixel_q8(o.x,ix.x,o.z);let py=pixel_q8(o.y,ix.y,o.z);
 if px.ok==0u||py.ok==0u{return 0u;}
 let m=c.grid[4];let t=bitcast<vec4u>(c.grid[5]);
 let qx=coord_q8(m.x,m.z,Wide(t.x,t.y),px.v,py.v);let qy=coord_q8(m.y,m.w,Wide(t.z,t.w),px.v,py.v);
 if qx.ok==0u||qy.ok==0u{return 0u;}
 return mask_texel(qx.v>>8,qy.v>>8);
}
// Mask coverage in Q14; 16384 when the stage has no mask (`settings.y == 0`).
fn mask_q(ix: vec2i, has: i32) -> i32 {
 if has==0{return Q14_ONE;}
 return i32(byte_q14(mask_byte(ix)));
}
// `(n + 2^27) >> 28` for n >= 0: the one rounding of the Q28 mask factor.
// Every mask term is non-negative and below 2^43, so no sign branch is needed.
fn div28(n: Wide) -> i32 {
 let adj=wide_add(n,Wide(1u<<27u,0u));
 return i32((adj.lo>>28u)|(adj.hi<<4u));
}
// Color-adjust on premultiplied Q14. The input is already exact Q14, so no
// float store sits between stages. The straightening is the color-adjust
// truncation `(q << 14) / alpha`; alpha passes through unchanged.
// No clamp between contrast and saturation. Straight 0..=16384, contrast
// Q14 -16384..=65536, brightness -16384..=16384: the contrast result is
// -40960..=57344, and luma stays in that span. The mix sum
// luma*(16384-saturation)+channel*saturation is -5502926848..=5771362304
// when luma and the channel are independent inside that span. It fits i64
// and does not fit i32, so those two products use Wide.
fn color_adjust_q14(v: vec4i, settings: vec4i) -> vec4i {
  let brightness = settings.x;
  let contrast = settings.y;
  let saturation = settings.z;
  let qa = v.a;
  var sr = 0;
  var sg = 0;
  var sb = 0;
  if qa != 0 {
    sr = clamp(i32((u32(v.r) << 14u) / u32(qa)), 0, Q14_ONE);
    sg = clamp(i32((u32(v.g) << 14u) / u32(qa)), 0, Q14_ONE);
    sb = clamp(i32((u32(v.b) << 14u) / u32(qa)), 0, Q14_ONE);
  }
  var r = div_q((sr - Q14_HALF) * contrast, Q14_ONE) + Q14_HALF + brightness;
  var g = div_q((sg - Q14_HALF) * contrast, Q14_ONE) + Q14_HALF + brightness;
  var bl = div_q((sb - Q14_HALF) * contrast, Q14_ONE) + Q14_HALF + brightness;
  let luma = div_q(r * 3483 + g * 11718 + bl * 1183, Q14_ONE);
  let gap = Q14_ONE - saturation;
  r = wide_div_q14(wide_add(wide_mul_i32(luma, gap), wide_mul_i32(r, saturation)));
  g = wide_div_q14(wide_add(wide_mul_i32(luma, gap), wide_mul_i32(g, saturation)));
  bl = wide_div_q14(wide_add(wide_mul_i32(luma, gap), wide_mul_i32(bl, saturation)));
  r = div_q(clamp(r, 0, Q14_ONE) * qa, Q14_ONE);
  g = div_q(clamp(g, 0, Q14_ONE) * qa, Q14_ONE);
  bl = div_q(clamp(bl, 0, Q14_ONE) * qa, Q14_ONE);
  return vec4i(r, g, bl, qa);
}
// A layer target onto the accumulator. At opacity 1 the premultiplied target is
// the source as stored (normal onto an empty backdrop is a Q14 copy). Below 1 it
// is the group pop: straighten, scale alpha once, premultiply again.
fn layer_q14(dst: vec4i, src: vec4i, op: i32, mode: u32) -> vec4i {
  let s = straight_q(src);
  if op == Q14_ONE { return blend_q(dst, s, src.rgb, src.a, mode); }
  let sa = mul_q(src.a, op);
  return blend_q(dst, s, vec3i(mul_q(s.r, sa), mul_q(s.g, sa), mul_q(s.b, sa)), sa, mode);
}
// Modes 21 to 23. Settings are `grid[0]`.
fn mask_stage(ix: vec2i) -> vec4i {
 let settings=c.grid[0];var out=vec4i(0);
 if c.info.x==21u{
  // Layer mask and opacity (v2 mode 4): F = m * o in Q28, rounded once.
  // settings: x opacity Q14, y has mask.
  let v=texel_a(ix);let f=u32(mask_q(ix,settings.y)*settings.x);
  for(var i=0u;i<4u;i++){out[i]=div28(wide_mul_u32(u32(v[i]),f));}
 }
 else if c.info.x==22u{
  // Effect selection (v2 mode 5): a is before the effect, b after it.
  let s=mask_q(ix,1);let a0=texel_a(ix);let f=texel_b(ix);
  for(var i=0u;i<4u;i++){out[i]=div_q(a0[i]*(Q14_ONE-s)+f[i]*s,Q14_ONE);}
 }
 else if c.info.x==23u{
  // Adjustment-layer mask, opacity and blend (v2 mode 7). As v2, screen (2) is
  // screen and every other non-normal blend is multiply, on straight Q14.
  // settings: x opacity Q14, y has mask, z blend.
  let o=texel_a(ix);let f=texel_b(ix);let w=u32(mask_q(ix,settings.y)*settings.x);
  var g=f;
  if settings.z!=0{
   let d=straight_q(o);let t=straight_q(f);var col=vec3i(0);
   for(var i=0u;i<3u;i++){
    if settings.z==2{col[i]=Q14_ONE-div_q((Q14_ONE-d[i])*(Q14_ONE-t[i]),Q14_ONE);}
    else{col[i]=div_q(d[i]*t[i],Q14_ONE);}
   }
   g=vec4i(div_q(col.r*o.a,Q14_ONE),div_q(col.g*o.a,Q14_ONE),div_q(col.b*o.a,Q14_ONE),o.a);
  }
  for(var i=0u;i<4u;i++){out[i]=div28(wide_add(wide_mul_u32(u32(o[i]),(1u<<28u)-w),wide_mul_u32(u32(g[i]),w)));}
 }
 return out;
}
@compute @workgroup_size(16,16) fn main(@builtin(global_invocation_id) id:vec3u){
 if any(id.xy>=c.output_clip.xy){return;}
 // The mask stages never form the float center `p`.
 if c.info.x>=21u&&c.info.x<=23u{
  textureStore(output,vec2i(id.xy),vec4u(clamp(mask_stage(vec2i(id.xy)),vec4i(0),vec4i(Q14_ONE))));
  return;
 }
 let p=c.out_region.xy+(vec2f(id.xy)+0.5)*c.out_region.z;var out=vec4i(0);
 let settings=bitcast<vec4i>(c.settings);
 if c.info.x==8u{out=load_a(p);}
 else if c.info.x==17u{out=color_adjust_q14(load_a(p),settings);}
 else if c.info.x==18u{
  let v=sparse_bytes(p);
  out=vec4i(i32(premul_byte_q14(v.r,v.a)),i32(premul_byte_q14(v.g,v.a)),i32(premul_byte_q14(v.b,v.a)),i32(byte_q14(v.a)));
 }else if c.info.x==19u{out=layer_q14(load_a(p),load_b(p),settings.x,u32(settings.y));}
 else if c.info.x==20u{
  // Adjustment-layer opacity: a premultiplied mix of the backdrop and the
  // adjusted copy, every channel with the same weight.
  let o=load_a(p);let f=load_b(p);let w=settings.x;
  for(var i=0u;i<4u;i++){out[i]=div_q(o[i]*(Q14_ONE-w)+f[i]*w,Q14_ONE);}
 }
 textureStore(output,vec2i(id.xy),vec4u(clamp(out,vec4i(0),vec4i(Q14_ONE))));
}
