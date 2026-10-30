// Render version 3 M5 stages. Appended after q14.wgsl and blend-q14.wgsl.
// Every intermediate is premultiplied Q14 (0..=16384) in rgba16uint, and every
// stage is integer from the pixel index on: no float position, region, inverse
// or uniform. Inputs are placed by `input_texel` (renderer.rs `input_map`), the
// atlas (raster tiles in mode 18, masks in modes 21 to 23) by `lookup` on the
// Q16 inverse and Q8 pixel centers (renderer.rs `mask_sampler`). The only float
// value is the Rgba8Unorm atlas texel, which `atlas_bytes` turns back into its
// bytes with `unorm_byte`. See doc/design/2026-10-28-v3-integer-mask.md.
// Bindings match effects.wgsl. Binding 2 carries vector coverage when an
// adjustment layer mixes its filtered copy with the backdrop. The host
// rejects every v3 scene that needs a stage not listed here (renderer.rs
// `v3_gap`), so unknown modes are never dispatched and write zero.
// Modes 21, 22 and 23 are the pixel-mask stages; v2 modes 4, 5 and 7 in
// effects.wgsl are not changed.
struct Tile { xy: vec2i, slot: u32, pad: u32 };
// The six slots before `info` are the f32 regions, inverse and settings of
// effects.wgsl. No v3 stage reads them, and the host leaves them zero. `grid` is
// the `curve` slot of effects.wgsl (renderer.rs `mask_grid`).
struct Config { unused: array<vec4u, 6>, info: vec4u, extra: vec4u, grid: array<vec4i, 16>, sizes: vec4u, output_clip: vec4u };
@group(0) @binding(0) var a: texture_2d<u32>;
@group(0) @binding(1) var b: texture_2d<u32>;
@group(0) @binding(2) var clip: texture_2d<u32>;
@group(0) @binding(3) var atlas: texture_2d_array<f32>;
@group(0) @binding(4) var output: texture_storage_2d<rgba16uint,write>;
@group(0) @binding(5) var<storage,read> tiles: array<Tile>;
@group(0) @binding(6) var<uniform> c: Config;
const Q14_ONE: i32 = 16384;
const Q14_HALF: i32 = 8192;
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
fn texel_clip(ix: vec2i)->vec4i {return vec4i(textureLoad(clip,ix,0));}
// The bytes of atlas texel `local` in layer `slot`. This is the one float read
// of a v3 stage: the atlas is the Rgba8Unorm one of versions 1 and 2, and
// `unorm_byte` returns each channel to its exact byte (`Gpu::atlas_read`).
fn atlas_bytes(local: vec2i, slot: i32) -> vec4u {
 let s=textureLoad(atlas,local,slot,0);
 return vec4u(unorm_byte(s.r),unorm_byte(s.g),unorm_byte(s.b),unorm_byte(s.a));
}
// The stored bytes of texel (x, y) of the uploaded tiles, found by the binary
// search of effects.wgsl `sparse`. No tile is transparent; a missing slot is the
// v2 debug color (1, 0, 0.2, 1), which a mask reads as byte 255.
fn tile_texel(x: i32, y: i32) -> vec4u {
 let key=vec2i(x>>8,y>>8);var lo=0u;var hi=c.info.z;
 loop {if lo>=hi{break;}let mid=(lo+hi)/2u;let t=tiles[mid];if t.xy.x<key.x||(t.xy.x==key.x&&t.xy.y<key.y){lo=mid+1u;}else{hi=mid;}}
 if lo>=c.info.z{return vec4u(0u);}let t=tiles[lo];if any(t.xy!=key){return vec4u(0u);}
 if t.slot==0xffffffffu{return vec4u(255u,0u,51u,255u);}
 return atlas_bytes(vec2i(x,y)-key*256,i32(t.slot));
}
// A negative scale encodes the reciprocal of a subpixel level: origin is then
// in grid units, so level -6 is [origin * 64, -64]. Q8 centers remain exact.
fn roi_pixel_q8(origin: i32, index: i32, scale: i32) -> Q8Fit {
 if scale>0{return pixel_q8(origin,index,scale);}
 let step=256/(-scale);
 let term=index*step+step/2;
 let sum=wide_add_checked(wide_mul_i32(origin,step),Wide(u32(term),0u));
 if sum.ok==0u{return Q8Fit(0,0u);}
 let negative=(sum.v.lo&0x80000000u)!=0u;
 let fits=select(sum.v.hi==0u,sum.v.hi==0xffffffffu,negative);
 return Q8Fit(bitcast<i32>(sum.v.lo),select(0u,1u,fits));
}
// The tile bytes under output pixel `ix`, nearest only (sample.rs
// SAMPLE_INTEGER positions; doc/design/2026-10-28-v3-integer-mask.md). Mode 18
// reads its raster and modes 21 to 23 their mask this way. `grid[3]` is the
// output ROI origin and scale, `grid[4]` the Q16 inverse and `grid[5]` the i64
// Q16 translation (lo, hi, lo, hi). The texel is the arithmetic `>> 8` of the
// Q8 position; there are no weights. A negative scale encodes a subpixel
// power-of-two level as described by `roi_pixel_q8`. Out of range is
// transparent (byte 0).
fn lookup(ix: vec2i) -> vec4u {
 let o=c.grid[3];
 let px=roi_pixel_q8(o.x,ix.x,o.z);let py=roi_pixel_q8(o.y,ix.y,o.z);
 if px.ok==0u||py.ok==0u{return vec4u(0u);}
 let m=c.grid[4];let t=bitcast<vec4u>(c.grid[5]);
 let qx=coord_q8(m.x,m.z,Wide(t.x,t.y),px.v,py.v);let qy=coord_q8(m.y,m.w,Wide(t.z,t.w),px.v,py.v);
 if qx.ok==0u||qy.ok==0u{return vec4u(0u);}
 return tile_texel(qx.v>>8,qy.v>>8);
}
// Mask coverage in Q14 from the mask byte (R); 16384 when the stage has no
// mask (`settings.y == 0`).
fn mask_q(ix: vec2i, has: i32) -> i32 {
 if has==0{return Q14_ONE;}
 return i32(byte_q14(lookup(ix).r));
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
// A clipped layer changes base color while retaining base coverage.
fn clip_layer_q14(base: vec4i, src: vec4i, op: i32, mode: u32) -> vec4i {
  let sa=mul_q(src.a,op);
  if sa==0 || base.a==0 {return base;}
  let d=straight_q(base);let s=straight_q(src);let mixed=mix_q(d,s,mode);
  var out=base;
  for(var i=0u;i<3u;i++){
    let color=div_q(d[i]*(Q14_ONE-sa)+mixed[i]*sa,Q14_ONE);
    out[i]=mul_q(color,base.a);
  }
  return out;
}
// A clipped adjustment transfers its color to the base coverage.
fn clip_adjust_q14(base: vec4i, adjusted: vec4i) -> vec4i {
  let color=straight_q(adjusted);var out=base;
  for(var i=0u;i<3u;i++){out[i]=mul_q(color[i],base.a);}
  return out;
}
fn invert_q14(v: vec4i) -> vec4i {
  let s=straight_q(v);var out=v;
  for(var i=0u;i<3u;i++){out[i]=mul_q(Q14_ONE-s[i],v.a);}
  return out;
}
fn curve_point_q14(i: u32) -> vec2i {
  let pair=c.grid[6u+i/2u];
  return select(pair.xy,pair.zw,(i&1u)==1u);
}
fn curves_q14(v: vec4i, count: u32) -> vec4i {
  let s=straight_q(v);var out=v;
  for(var channel=0u;channel<3u;channel++){
    let x=s[channel];var y=x;
    for(var i=0u;i+1u<count;i++){
      let a=curve_point_q14(i);let b=curve_point_q14(i+1u);
      if b.x>a.x && x>=a.x && x<=b.x {
        y=a.y+div_q((x-a.x)*(b.y-a.y),b.x-a.x);
      }
    }
    out[channel]=mul_q(clamp(y,0,Q14_ONE),v.a);
  }
  return out;
}
const EXP2_ROOT_Q14: array<i32,14> = array<i32,14>(23170,19484,17867,17109,16743,16562,16473,16428,16406,16395,16390,16387,16385,16385);
fn log2_q16(x: i32) -> i32 {
  var v=x;var whole=0;var frac=0;
  loop {if v>=Q14_ONE {break;}v=v*2;whole=whole-65536;}
  for(var i=0u;i<16u;i++){
    v=div_q(v*v,Q14_ONE);
    if v>=2*Q14_ONE {v=div_q(v,2);frac=frac | (1 << (15u-i));}
  }
  return whole+frac;
}
fn exp2_q16(x: i32) -> i32 {
  if x>=0 {return Q14_ONE;}
  let whole=(x-65535)/65536;
  let frac=x-whole*65536;
  var v=Q14_ONE;
  for(var i=0u;i<14u;i++){
    if (frac & (1 << (15u-i)))!=0 {v=div_q(v*EXP2_ROOT_Q14[i],Q14_ONE);}
  }
  let shift=-whole;
  if shift>=16 {return 0;}
  return div_q(v,1 << u32(shift));
}
fn pow_q14(x: i32, exponent: i32) -> i32 {
  if x<=0 {return 0;}
  if x>=Q14_ONE {return Q14_ONE;}
  let scaled=wide_div_q14(wide_mul_i32(log2_q16(x),exponent));
  return clamp(exp2_q16(scaled),0,Q14_ONE);
}
fn levels_q14(v: vec4i) -> vec4i {
  let settings=c.grid[0];let s=straight_q(v);var out=v;
  let span=max(settings.y-settings.x,1);
  for(var i=0u;i<3u;i++){
    let x=clamp(div_q((s[i]-settings.x)*Q14_ONE,span),0,Q14_ONE);
    let y=pow_q14(x,settings.z);
    let mapped=clamp(settings.w+div_q((c.grid[6].x-settings.w)*y,Q14_ONE),0,Q14_ONE);
    out[i]=mul_q(mapped,v.a);
  }
  return out;
}
const CORDIC_ATAN_DEG_Q16: array<i32,16> = array<i32,16>(2949120,1740967,919879,466945,234379,117304,58666,29335,14668,7334,3667,1833,917,458,229,115);
fn hue_sincos_q14(angle: i32) -> vec2i {
  if angle==0 {return vec2i(Q14_ONE,0);}
  if angle==5898240 {return vec2i(0,Q14_ONE);}
  if angle== -5898240 {return vec2i(0,-Q14_ONE);}
  if angle==11796480 || angle== -11796480 {return vec2i(-Q14_ONE,0);}
  var z=angle;var x=9949;var y=0;
  if z>5898240 {z=z-11796480;x= -x;y= -y;}
  if z< -5898240 {z=z+11796480;x= -x;y= -y;}
  for(var i=0u;i<16u;i++){
    let step_x=y>>i;let step_y=x>>i;
    if z>=0 {x=x-step_x;y=y+step_y;z=z-CORDIC_ATAN_DEG_Q16[i];}
    else {x=x+step_x;y=y-step_y;z=z+CORDIC_ATAN_DEG_Q16[i];}
  }
  return vec2i(clamp(x,-Q14_ONE,Q14_ONE),clamp(y,-Q14_ONE,Q14_ONE));
}
fn hue_saturation_q14(v: vec4i) -> vec4i {
  let settings=c.grid[0];let s=straight_q(v);
  let lum=div_q(s.r*3483+s.g*11718+s.b*1183,Q14_ONE);
  let cs=hue_sincos_q14(settings.x);let u=s.r-lum;let w=s.b-lum;
  let ru=div_q(u*cs.x-w*cs.y,Q14_ONE);
  let rw=div_q(u*cs.y+w*cs.x,Q14_ONE);
  var rgb=vec3i(lum+ru,lum-div_q(6466*ru+9513*rw,Q14_ONE),lum+rw);
  let rotated_lum=div_q(rgb.r*3483+rgb.g*11718+rgb.b*1183,Q14_ONE);
  let gap=Q14_ONE-settings.y;
  for(var i=0u;i<3u;i++){
    let mixed=wide_div_q14(wide_add(wide_mul_i32(rotated_lum,gap),wide_mul_i32(rgb[i],settings.y)));
    rgb[i]=clamp(mixed+settings.z,0,Q14_ONE);
  }
  return vec4i(mul_q(rgb.r,v.a),mul_q(rgb.g,v.a),mul_q(rgb.b,v.a),v.a);
}
// Modes 21 to 23 and 26. Settings are `grid[0]`.
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
 else if c.info.x==26u{
  // A vector mask (or the product of vector mask and clipping path) is a
  // premultiplied white Q14 coverage target. Combine before masking source.
  let v=texel_a(ix);let k=texel_b(ix).a;
  for(var i=0u;i<4u;i++){out[i]=div_q(v[i]*k,Q14_ONE);}
 }
 return out;
}
fn blur_weight(k: u32) -> u32 {
 let j=k/2u;
 let word=bitcast<u32>(c.grid[1u+j/4u][j%4u]);
 return (word>>(16u*(k%2u)))&0xffffu;
}
fn sample_q14(q: vec2i) -> vec4u {
 if any(q<vec2i(0))||any(q>=vec2i(c.sizes.xy)){return vec4u(0u);}
 return textureLoad(a,q,0);
}
fn blur_stage(ix: vec2i) -> vec4i {
 let settings=c.grid[0];let center=ix+settings.yz;let r=settings.x;
 var accum=vec4u(0u);
 for(var i=-r;i<=r;i=i+1){
  let delta=select(vec2i(i,0),vec2i(0,i),c.info.w==1u);
  accum=accum+sample_q14(center+delta)*blur_weight(u32(abs(i)));
 }
 return vec4i((accum+vec4u(32768u))>>vec4u(16u));
}
fn resample_stage(ix: vec2i) -> vec4i {
 let settings=c.grid[0];let m=u32(settings.z);
 let shift=i32(1u<<m);
 let u=2*(ix+settings.xy)+vec2i(1)-vec2i(shift);
 let j=u>>vec2u(m+1u);
 let mask=i32((1u<<(m+1u))-1u);
 let f=(u&vec2i(mask))<<vec2u(7u-m);
 let a0=sample_q14(j);let b0=sample_q14(j+vec2i(1,0));
 let d0=sample_q14(j+vec2i(0,1));let e0=sample_q14(j+vec2i(1,1));
 let fx=u32(f.x);let fy=u32(f.y);
 let ab=a0*(256u-fx)+b0*fx;
 let de=d0*(256u-fx)+e0*fx;
 return vec4i((ab*(256u-fy)+de*fy+vec4u(32768u))>>vec4u(16u));
}
// Every mode works on the integer pixel index; settings are `grid[0]`.
@compute @workgroup_size(16,16) fn main(@builtin(global_invocation_id) id:vec3u){
 if any(id.xy>=c.output_clip.xy){return;}
 let ix=vec2i(id.xy);let settings=c.grid[0];var out=vec4i(0);
 if c.info.x==8u{out=texel_a(ix);}
 else if c.info.x==17u{out=color_adjust_q14(texel_a(ix),settings);}
 else if c.info.x==18u{
  // Raster ingress: the stored bytes, premultiplied into Q14.
  let v=lookup(ix);
  out=vec4i(i32(premul_byte_q14(v.r,v.a)),i32(premul_byte_q14(v.g,v.a)),i32(premul_byte_q14(v.b,v.a)),i32(byte_q14(v.a)));
 }else if c.info.x==19u{out=layer_q14(texel_a(ix),texel_b(ix),settings.x,u32(settings.y));}
 else if c.info.x==20u{
  // Adjustment-layer opacity: a premultiplied mix of the backdrop and the
  // adjusted copy, every channel with the same weight.
  let o=texel_a(ix);let f=texel_b(ix);let w=settings.x;
  for(var i=0u;i<4u;i++){out[i]=div_q(o[i]*(Q14_ONE-w)+f[i]*w,Q14_ONE);}
 }
 else if (c.info.x>=21u&&c.info.x<=23u)||c.info.x==26u{out=mask_stage(ix);}
 else if c.info.x==24u{out=blur_stage(ix);}
 else if c.info.x==25u{out=resample_stage(ix);}
 else if c.info.x==27u{out=clip_layer_q14(texel_a(ix),texel_b(ix),settings.x,u32(settings.y));}
 else if c.info.x==28u{out=clip_adjust_q14(texel_a(ix),texel_b(ix));}
 else if c.info.x==29u{
  let before=texel_a(ix);let after=texel_b(ix);let k=clamp(texel_clip(ix).r,0,Q14_ONE);
  for(var i=0u;i<4u;i++){out[i]=div_q(before[i]*(Q14_ONE-k)+after[i]*k,Q14_ONE);}
 }
 else if c.info.x==30u{out=invert_q14(texel_a(ix));}
 else if c.info.x==31u{out=curves_q14(texel_a(ix),u32(settings.x));}
 else if c.info.x==32u{out=levels_q14(texel_a(ix));}
 else if c.info.x==33u{out=hue_saturation_q14(texel_a(ix));}
 textureStore(output,ix,vec4u(clamp(out,vec4i(0),vec4i(Q14_ONE))));
}
