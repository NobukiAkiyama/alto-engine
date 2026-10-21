// ROI coordinates are in document pixels. All intermediates are premultiplied RGBA16F.
struct Tile { xy: vec2i, slot: u32, pad: u32 };
struct Config { out_region: vec4f, region_a: vec4f, region_b: vec4f, inv0: vec4f, inv1: vec4f, settings: vec4f, info: vec4u, extra: vec4f, curve: array<vec4f, 16> };
@group(0) @binding(0) var a: texture_2d<f32>;
@group(0) @binding(1) var b: texture_2d<f32>;
@group(0) @binding(2) var clip_base: texture_2d<f32>;
@group(0) @binding(3) var atlas: texture_2d_array<f32>;
@group(0) @binding(4) var output: texture_storage_2d<rgba16float,write>;
@group(0) @binding(5) var<storage,read> tiles: array<Tile>;
@group(0) @binding(6) var<uniform> c: Config;
fn load_a(p:vec2f)->vec4f { let q=vec2i(floor((p-c.region_a.xy)/c.region_a.z));if any(q<vec2i(0))||any(q>=vec2i(textureDimensions(a))){return vec4f(0);}return textureLoad(a,q,0); }
fn a_at(q:vec2i)->vec4f {if any(q<vec2i(0))||any(q>=vec2i(textureDimensions(a))){return vec4f(0);}return textureLoad(a,q,0);}
fn a_linear(p:vec2f)->vec4f {let q=(p-c.region_a.xy)/c.region_a.z-0.5;let v=vec2i(floor(q));let f=fract(q);return mix(mix(a_at(v),a_at(v+vec2i(1,0)),f.x),mix(a_at(v+vec2i(0,1)),a_at(v+vec2i(1,1)),f.x),f.y);}
fn load_b(p:vec2f)->vec4f { let q=vec2i(floor((p-c.region_b.xy)/c.region_b.z));if any(q<vec2i(0))||any(q>=vec2i(textureDimensions(b))){return vec4f(0);}return textureLoad(b,q,0); }
fn sparse(p:vec2f)->vec4f {
 let q=vec2f(c.inv0.x*p.x+c.inv0.z*p.y+c.inv1.x,c.inv0.y*p.x+c.inv0.w*p.y+c.inv1.y);
 let key=vec2i(floor(q/256.0));var lo=0u;var hi=c.info.z;
 loop {if lo>=hi{break;}let mid=(lo+hi)/2u;let t=tiles[mid];if t.xy.x<key.x||(t.xy.x==key.x&&t.xy.y<key.y){lo=mid+1u;}else{hi=mid;}}
 if lo>=c.info.z{return vec4f(0);}let t=tiles[lo];if any(t.xy!=key){return vec4f(0);}
 if t.slot==0xffffffffu{return vec4f(1,0,0.2,1);}
 return textureLoad(atlas,vec2i(floor(q))-key*256,i32(t.slot),0);
}
@compute @workgroup_size(16,16) fn main(@builtin(global_invocation_id) id:vec3u){
 if any(id.xy>=textureDimensions(output)){return;}
 let p=c.out_region.xy+(vec2f(id.xy)+0.5)*c.out_region.z;var out=vec4f(0);
 if c.info.x==0u{let v=sparse(p);out=vec4f(v.rgb*v.a,v.a);}
 else if c.info.x==1u{
  var src=load_b(p);if c.settings.z>0.5{src*=textureLoad(clip_base,vec2i(id.xy),0).a;}
  let straight=select(vec3f(0.0),src.rgb/max(src.a,0.000001),src.a>0.0);
  out=blend(load_a(p),vec4f(straight,src.a),u32(c.settings.y));
 }else if c.info.x==2u{
  var sum=vec4f(0);var total=0.0;let radius=i32(c.settings.w);let sigma=max(c.settings.x,0.333333);
  for(var i=-64;i<=64;i++){if abs(i)>radius{continue;}let weight=exp(-f32(i*i)/(2.0*sigma*sigma));var delta=vec2f(f32(i)*c.out_region.z,0);if c.info.w==1u{delta=delta.yx;}sum+=load_a(p+delta)*weight;total+=weight;}
  out=sum/max(total,0.000001);
 }else if c.info.x==3u{
  let v=load_a(p);var rgb=v.rgb/max(v.a,0.000001);rgb=(rgb-0.5)*c.settings.y+0.5+c.settings.x;let l=dot(rgb,vec3f(0.2126,0.7152,0.0722));rgb=mix(vec3f(l),rgb,c.settings.z);out=vec4f(clamp(rgb,vec3f(0),vec3f(1))*v.a,v.a);
 }else if c.info.x==4u{
  var mask=1.0;if c.settings.y>0.5{mask=sparse(p).r;}out=load_a(p)*mask*c.settings.x;
 }else if c.info.x==5u{out=mix(load_a(p),load_b(p),sparse(p).r);}
 else if c.info.x==7u{var amount=c.settings.x;if c.settings.y>0.5{amount*=sparse(p).r;}if c.settings.z>0.5{amount*=textureLoad(clip_base,vec2i(id.xy),0).a;}let original=load_a(p);var filtered=load_b(p);if c.settings.w>0.5{let d=original.rgb/max(original.a,0.000001);let f=filtered.rgb/max(filtered.a,0.000001);var color=d*f;if u32(c.settings.w)==2u{color=1.0-(1.0-d)*(1.0-f);}filtered=vec4f(color*original.a,original.a);}out=mix(original,filtered,amount);}
 else if c.info.x==8u{out=load_a(p);if c.region_a.z>c.out_region.z{out=a_linear(p);}}
 else if c.info.x==9u{let base=load_a(p);let src=load_b(p);let d=base.rgb/max(base.a,0.000001);let s=src.rgb/max(src.a,0.000001);let mode=u32(c.settings.y);let blended=mix_color(s,d,select(0u,mode,mode<=2u));out=vec4f(mix(d,blended,src.a)*base.a,base.a);}
 else if c.info.x==10u{let base=load_a(p);let filtered=load_b(p);out=vec4f(filtered.rgb/max(filtered.a,0.000001)*base.a,base.a);}
 else if c.info.x==11u{out=load_a(p)*load_b(p).r;}
 else if c.info.x==12u{out=mix(load_a(p),load_b(p),textureLoad(clip_base,vec2i(id.xy),0).r);}
 else if c.info.x==13u{let v=load_a(p);var rgb=v.rgb/max(v.a,0.000001);let span=max(c.settings.y-c.settings.x,0.000001);rgb=clamp((rgb-c.settings.x)/span,vec3f(0),vec3f(1));rgb=pow(rgb,vec3f(1.0/max(c.settings.z,0.1)));rgb=mix(vec3f(c.settings.w),vec3f(c.extra.x),rgb);out=vec4f(clamp(rgb,vec3f(0),vec3f(1))*v.a,v.a);}
 else if c.info.x==14u{let v=load_a(p);let rgb=1.0-v.rgb/max(v.a,0.000001);out=vec4f(clamp(rgb,vec3f(0),vec3f(1))*v.a,v.a);}
 else if c.info.x==15u{let v=load_a(p);var rgb=v.rgb/max(v.a,0.000001);let l=lum3(rgb);let angle=c.settings.x*0.017453292;let cs=cos(angle);let sn=sin(angle);let y=l;let u=rgb.r-l;let vv=rgb.b-l;let uu=u*cs-vv*sn;let ww=u*sn+vv*cs;rgb=vec3f(y+uu,y-0.39465*uu-0.58060*ww,y+ww);rgb=mix(vec3f(lum3(rgb)),rgb,c.settings.y);rgb=clamp(rgb+vec3f(c.settings.z),vec3f(0),vec3f(1));out=vec4f(rgb*v.a,v.a);}
 else if c.info.x==16u{let v=load_a(p);var rgb=v.rgb/max(v.a,0.000001);let n=u32(c.extra.x);for(var ch=0;ch<3;ch++){var x=rgb[ch];var y=x;for(var i=0u;i+1u<n;i++){let a=c.curve[i].xy;let b=c.curve[i+1u].xy;if x>=a.x && x<=b.x {let t=(x-a.x)/max(b.x-a.x,0.000001);y=mix(a.y,b.y,t);}}rgb[ch]=y;}out=vec4f(clamp(rgb,vec3f(0),vec3f(1))*v.a,v.a);}
 textureStore(output,vec2i(id.xy),out);
}
