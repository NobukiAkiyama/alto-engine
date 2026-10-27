// ROI coordinates are in document pixels. All intermediates are premultiplied RGBA16F.
struct Tile { xy: vec2i, slot: u32, pad: u32 };
struct Config { out_region: vec4f, region_a: vec4f, region_b: vec4f, inv0: vec4f, inv1: vec4f, settings: vec4f, info: vec4u, extra: vec4f, curve: array<vec4f, 16>, sizes: vec4u, output_clip: vec4u };
@group(0) @binding(0) var a: texture_2d<f32>;
@group(0) @binding(1) var b: texture_2d<f32>;
@group(0) @binding(2) var clip_base: texture_2d<f32>;
@group(0) @binding(3) var atlas: texture_2d_array<f32>;
@group(0) @binding(4) var output: texture_storage_2d<rgba16float,write>;
@group(0) @binding(5) var<storage,read> tiles: array<Tile>;
@group(0) @binding(6) var<uniform> c: Config;
fn load_a(p:vec2f)->vec4f { let q=vec2i(floor((p-c.region_a.xy)/c.region_a.z));if any(q<vec2i(0))||any(q>=vec2i(c.sizes.xy)){return vec4f(0);}return textureLoad(a,q,0); }
fn a_at(q:vec2i)->vec4f {if any(q<vec2i(0))||any(q>=vec2i(c.sizes.xy)){return vec4f(0);}return textureLoad(a,q,0);}
fn a_linear(p:vec2f)->vec4f {let q=(p-c.region_a.xy)/c.region_a.z-0.5;let v=vec2i(floor(q));let f=fract(q);return mix(mix(a_at(v),a_at(v+vec2i(1,0)),f.x),mix(a_at(v+vec2i(0,1)),a_at(v+vec2i(1,1)),f.x),f.y);}
fn load_b(p:vec2f)->vec4f { let q=vec2i(floor((p-c.region_b.xy)/c.region_b.z));if any(q<vec2i(0))||any(q>=vec2i(c.sizes.zw)){return vec4f(0);}return textureLoad(b,q,0); }
fn load_clip(q:vec2i)->vec4f { if any(q<vec2i(0))||any(q>=vec2i(c.output_clip.zw)){return vec4f(0);}return textureLoad(clip_base,q,0); }
fn sparse(p:vec2f)->vec4f {
 let q=vec2f(c.inv0.x*p.x+c.inv0.z*p.y+c.inv1.x,c.inv0.y*p.x+c.inv0.w*p.y+c.inv1.y);
 let key=vec2i(floor(q/256.0));var lo=0u;var hi=c.info.z;
 loop {if lo>=hi{break;}let mid=(lo+hi)/2u;let t=tiles[mid];if t.xy.x<key.x||(t.xy.x==key.x&&t.xy.y<key.y){lo=mid+1u;}else{hi=mid;}}
 if lo>=c.info.z{return vec4f(0);}let t=tiles[lo];if any(t.xy!=key){return vec4f(0);}
 if t.slot==0xffffffffu{return vec4f(1,0,0.2,1);}
 return textureLoad(atlas,vec2i(floor(q))-key*256,i32(t.slot),0);
}
// Render version 3 color-adjust. Q14, 1.0 = 16384. Settings are i32 bitcasts.
const Q14_ONE: i32 = 16384;
const Q14_HALF: i32 = 8192;
fn f32_to_f16_bits(value: f32) -> u32 {
  let x = bitcast<u32>(value);
  let sign = (x >> 16u) & 0x8000u;
  let mantissa = x & 0x007fffffu;
  let exp = (x & 0x7f800000u) >> 23u;
  if exp == 0xffu {
    if mantissa == 0u { return sign | 0x7c00u; }
    let payload = mantissa >> 13u;
    return sign | 0x7c00u | payload | select(0u, 1u, payload == 0u);
  }
  let e = i32(exp) - 127 + 15;
  if e <= 0 {
    if e < -10 { return sign; }
    let m = mantissa | 0x00800000u;
    let shift = u32(14 - e);
    var half = m >> shift;
    let rem = m & ((1u << shift) - 1u);
    let halfway = 1u << (shift - 1u);
    if rem > halfway || (rem == halfway && (half & 1u) == 1u) { half = (half + 1u) & 0xffffu; }
    return (sign | half) & 0xffffu;
  }
  if e >= 31 { return sign | 0x7c00u; }
  var half = (mantissa >> 13u) | (u32(e) << 10u);
  let rem = mantissa & 0x1fffu;
  if rem > 0x1000u || (rem == 0x1000u && (half & 1u) == 1u) { half = (half + 1u) & 0xffffu; }
  return (sign | half) & 0xffffu;
}
fn f16_bits_to_f32_bits(half: u32) -> u32 {
  let h = half & 0xffffu;
  let sign = (h & 0x8000u) << 16u;
  let exp = (h >> 10u) & 0x1fu;
  let mant = h & 0x3ffu;
  if exp == 0u {
    if mant == 0u { return sign; }
    var m = mant;
    var e = 113u;
    loop {
      if (m & 0x400u) != 0u { break; }
      m = m << 1u;
      e = e - 1u;
    }
    return sign | (e << 23u) | ((m & 0x3ffu) << 13u);
  }
  if exp == 31u { return sign | 0x7f800000u | (mant << 13u); }
  return sign | ((exp + (127u - 15u)) << 23u) | (mant << 13u);
}
fn f16_bits_to_q14(half: u32) -> u32 {
  let h = half & 0xffffu;
  if (h & 0x8000u) != 0u { return 0u; }
  let exp = (h >> 10u) & 0x1fu;
  let mant = h & 0x3ffu;
  if exp == 31u { return select(0u, 16384u, mant == 0u); }
  if exp == 0u { return 0u; }
  let sig = 1024u + mant;
  var q: u32;
  if exp >= 11u { q = sig << (exp - 11u); } else { q = sig >> (11u - exp); }
  return min(q, 16384u);
}
fn floor_log2_u(value: u32) -> u32 {
  var n = value;
  var p = 0u;
  if n >= 256u { p += 8u; n >>= 8u; }
  if n >= 16u { p += 4u; n >>= 4u; }
  if n >= 4u { p += 2u; n >>= 2u; }
  if n >= 2u { p += 1u; }
  return p;
}
fn q14_to_f16_bits(q_in: u32) -> u32 {
  if q_in == 0u { return 0u; }
  let q = min(q_in, 16384u);
  if q == 16384u { return 0x3c00u; }
  let p = floor_log2_u(q);
  var sig = q;
  var exp_p = p;
  if p >= 10u {
    let cut = p - 10u;
    if cut > 0u {
      let half = 1u << (cut - 1u);
      let rem = sig & ((1u << cut) - 1u);
      sig = sig >> cut;
      if rem > half || (rem == half && (sig & 1u) == 1u) { sig += 1u; }
    }
    if sig >= 2048u { sig = 1024u; exp_p = p + 1u; }
  } else {
    sig = q << (10u - p);
  }
  return ((exp_p + 1u) << 10u) | (sig & 1023u);
}
fn div_q(n: i32, d: i32) -> i32 {
  let half = d / 2;
  if n >= 0 { return (n + half) / d; }
  return -(((-n) + half) / d);
}
// Same 64-bit pair as composite.wgsl `Wide`. effects.wgsl is a separate shader,
// so the emulation is copied rather than shared. Factors here are far from
// i32::MIN, which `wide_mul_i32` cannot negate.
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
fn q14_stored(q: u32) -> f32 {
  return bitcast<f32>(f16_bits_to_f32_bits(q14_to_f16_bits(q)));
}
fn color_adjust_exact(v: vec4f, settings: vec4i) -> vec4f {
  let brightness = settings.x;
  let contrast = settings.y;
  let saturation = settings.z;
  let qr = i32(f16_bits_to_q14(f32_to_f16_bits(v.r)));
  let qg = i32(f16_bits_to_q14(f32_to_f16_bits(v.g)));
  let qb = i32(f16_bits_to_q14(f32_to_f16_bits(v.b)));
  let qa = i32(f16_bits_to_q14(f32_to_f16_bits(v.a)));
  var sr = 0;
  var sg = 0;
  var sb = 0;
  if qa != 0 {
    sr = clamp(i32((u32(qr) << 14u) / u32(qa)), 0, Q14_ONE);
    sg = clamp(i32((u32(qg) << 14u) / u32(qa)), 0, Q14_ONE);
    sb = clamp(i32((u32(qb) << 14u) / u32(qa)), 0, Q14_ONE);
  }
  // No clamp between contrast and saturation. Straight 0..=16384, contrast
  // Q14 -16384..=65536, brightness -16384..=16384: the contrast result is
  // -40960..=57344, and luma stays in that span. The mix sum
  // luma*(16384-saturation)+channel*saturation is -5502926848..=5771362304
  // when luma and the channel are independent inside that span. It fits i64
  // and does not fit i32, so those two products use Wide.
  var r = div_q((sr - Q14_HALF) * contrast, Q14_ONE) + Q14_HALF + brightness;
  var g = div_q((sg - Q14_HALF) * contrast, Q14_ONE) + Q14_HALF + brightness;
  var b = div_q((sb - Q14_HALF) * contrast, Q14_ONE) + Q14_HALF + brightness;
  let luma = div_q(r * 3483 + g * 11718 + b * 1183, Q14_ONE);
  let gap = Q14_ONE - saturation;
  r = wide_div_q14(wide_add(wide_mul_i32(luma, gap), wide_mul_i32(r, saturation)));
  g = wide_div_q14(wide_add(wide_mul_i32(luma, gap), wide_mul_i32(g, saturation)));
  b = wide_div_q14(wide_add(wide_mul_i32(luma, gap), wide_mul_i32(b, saturation)));
  r = div_q(clamp(r, 0, Q14_ONE) * qa, Q14_ONE);
  g = div_q(clamp(g, 0, Q14_ONE) * qa, Q14_ONE);
  b = div_q(clamp(b, 0, Q14_ONE) * qa, Q14_ONE);
  return vec4f(q14_stored(u32(r)), q14_stored(u32(g)), q14_stored(u32(b)), v.a);
}
@compute @workgroup_size(16,16) fn main(@builtin(global_invocation_id) id:vec3u){
 if any(id.xy>=c.output_clip.xy){return;}
 let p=c.out_region.xy+(vec2f(id.xy)+0.5)*c.out_region.z;var out=vec4f(0);
 if c.info.x==0u{let v=sparse(p);out=vec4f(v.rgb*v.a,v.a);}
 else if c.info.x==1u{
  var src=load_b(p);if c.settings.z>0.5{src*=load_clip(vec2i(id.xy)).a;}
  let straight=select(vec3f(0.0),src.rgb/max(src.a,0.000001),src.a>0.0);
  out=blend(load_a(p),vec4f(straight,src.a),u32(c.settings.y));
 }else if c.info.x==2u{
  var sum=vec4f(0);var total=0.0;let radius=i32(c.settings.w);let sigma=max(c.settings.x,0.333333);
  for(var i=-64;i<=64;i++){if abs(i)>radius{continue;}let weight=exp(-f32(i*i)/(2.0*sigma*sigma));var delta=vec2f(f32(i)*c.out_region.z,0);if c.info.w==1u{delta=delta.yx;}sum+=load_a(p+delta)*weight;total+=weight;}
  out=sum/max(total,0.000001);
 }else if c.info.x==3u{
  // Render version 1 and 2. This statement stays so those pixels do not move.
  let v=load_a(p);var rgb=v.rgb/max(v.a,0.000001);rgb=(rgb-0.5)*c.settings.y+0.5+c.settings.x;let l=dot(rgb,vec3f(0.2126,0.7152,0.0722));rgb=mix(vec3f(l),rgb,c.settings.z);out=vec4f(clamp(rgb,vec3f(0),vec3f(1))*v.a,v.a);
 }else if c.info.x==17u{out=color_adjust_exact(load_a(p),bitcast<vec4i>(c.settings));
 }else if c.info.x==18u{
  let v=sparse(p);let a=unorm_byte(v.a);
  out=vec4f(q14_stored(premul_byte_q14(unorm_byte(v.r),a)),q14_stored(premul_byte_q14(unorm_byte(v.g),a)),q14_stored(premul_byte_q14(unorm_byte(v.b),a)),q14_stored(byte_q14(a)));
 }else if c.info.x==19u{
  var src=load_b(p);if c.settings.z>0.5{src*=load_clip(vec2i(id.xy)).a;}
  let dst=load_a(p);
  if u32(c.settings.y)==0u && all(dst==vec4f(0.0)){out=src;}
  else {let straight=select(vec3f(0.0),src.rgb/max(src.a,0.000001),src.a>0.0);out=blend(dst,vec4f(straight,src.a),u32(c.settings.y));}
 }else if c.info.x==4u{
  var mask=1.0;if c.settings.y>0.5{mask=sparse(p).r;}out=load_a(p)*mask*c.settings.x;
 }else if c.info.x==5u{out=mix(load_a(p),load_b(p),sparse(p).r);}
 else if c.info.x==7u{var amount=c.settings.x;if c.settings.y>0.5{amount*=sparse(p).r;}if c.settings.z>0.5{amount*=load_clip(vec2i(id.xy)).a;}let original=load_a(p);var filtered=load_b(p);if c.settings.w>0.5{let d=original.rgb/max(original.a,0.000001);let f=filtered.rgb/max(filtered.a,0.000001);var color=d*f;if u32(c.settings.w)==2u{color=1.0-(1.0-d)*(1.0-f);}filtered=vec4f(color*original.a,original.a);}out=mix(original,filtered,amount);}
 else if c.info.x==8u{out=load_a(p);if c.region_a.z>c.out_region.z{out=a_linear(p);}}
 else if c.info.x==9u{let base=load_a(p);let src=load_b(p);let d=base.rgb/max(base.a,0.000001);let s=src.rgb/max(src.a,0.000001);let mode=u32(c.settings.y);let blended=mix_color(s,d,select(0u,mode,mode<=2u));out=vec4f(mix(d,blended,src.a)*base.a,base.a);}
 else if c.info.x==10u{let base=load_a(p);let filtered=load_b(p);out=vec4f(filtered.rgb/max(filtered.a,0.000001)*base.a,base.a);}
 else if c.info.x==11u{out=load_a(p)*load_b(p).r;}
 else if c.info.x==12u{out=mix(load_a(p),load_b(p),load_clip(vec2i(id.xy)).r);}
 else if c.info.x==13u{let v=load_a(p);var rgb=v.rgb/max(v.a,0.000001);let span=max(c.settings.y-c.settings.x,0.000001);rgb=clamp((rgb-c.settings.x)/span,vec3f(0),vec3f(1));rgb=pow(rgb,vec3f(1.0/max(c.settings.z,0.1)));rgb=mix(vec3f(c.settings.w),vec3f(c.extra.x),rgb);out=vec4f(clamp(rgb,vec3f(0),vec3f(1))*v.a,v.a);}
 else if c.info.x==14u{let v=load_a(p);let rgb=1.0-v.rgb/max(v.a,0.000001);out=vec4f(clamp(rgb,vec3f(0),vec3f(1))*v.a,v.a);}
 else if c.info.x==15u{let v=load_a(p);var rgb=v.rgb/max(v.a,0.000001);let l=lum3(rgb);let angle=c.settings.x*0.017453292;let cs=cos(angle);let sn=sin(angle);let y=l;let u=rgb.r-l;let vv=rgb.b-l;let uu=u*cs-vv*sn;let ww=u*sn+vv*cs;rgb=vec3f(y+uu,y-0.39465*uu-0.58060*ww,y+ww);rgb=mix(vec3f(lum3(rgb)),rgb,c.settings.y);rgb=clamp(rgb+vec3f(c.settings.z),vec3f(0),vec3f(1));out=vec4f(rgb*v.a,v.a);}
 else if c.info.x==16u{let v=load_a(p);var rgb=v.rgb/max(v.a,0.000001);let n=u32(c.extra.x);for(var ch=0;ch<3;ch++){var x=rgb[ch];var y=x;for(var i=0u;i+1u<n;i++){let a=c.curve[i].xy;let b=c.curve[i+1u].xy;if x>=a.x && x<=b.x {let t=(x-a.x)/max(b.x-a.x,0.000001);y=mix(a.y,b.y,t);}}rgb[ch]=y;}out=vec4f(clamp(rgb,vec3f(0),vec3f(1))*v.a,v.a);}
 textureStore(output,vec2i(id.xy),out);
}
