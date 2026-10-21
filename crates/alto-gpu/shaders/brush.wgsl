struct Dab { center_radius: vec4f, extra: vec4f };
struct Config { color: vec4f, params: vec4f, origin: vec4f, info: vec4u };
@group(0) @binding(0) var base: texture_2d<f32>;
@group(0) @binding(1) var output: texture_storage_2d<rgba8unorm,write>;
@group(0) @binding(2) var<storage,read_write> coverage: array<f32>;
@group(0) @binding(3) var<storage,read> dabs: array<Dab>;
@group(0) @binding(4) var<uniform> config: Config;
@group(0) @binding(5) var selection: texture_2d<f32>;
@compute @workgroup_size(16,16) fn main(@builtin(global_invocation_id) id:vec3u) {
  if any(id.xy>=vec2u(256)) { return; }
  let p=config.origin.xy+vec2f(id.xy)+0.5;let idx=id.y*256u+id.x;
  var v=coverage[idx];
  for(var i=0u;i<config.info.x;i++) {
    let d=dabs[i];let dist=distance(p,d.center_radius.xy)/max(0.01,d.center_radius.z);
    let shape=1.0-smoothstep(config.params.x,1.0,dist);
    v=1.0-(1.0-v)*(1.0-shape*d.center_radius.w);
  }
  coverage[idx]=v;
  var selected=1.0;if config.info.z!=0u {selected=textureLoad(selection,vec2i(id.xy),0).r;}
  let alpha=min(v,config.params.y)*config.color.a*selected;
  let b=textureLoad(base,vec2i(id.xy),0);
  if alpha<=0.0 {textureStore(output,vec2i(id.xy),b);return;}
  var out=vec4f(b.rgb,b.a*(1.0-alpha));
  if config.info.y==0u {
    let a=alpha+b.a*(1.0-alpha);
    out=vec4f((config.color.rgb*alpha+b.rgb*b.a*(1.0-alpha))/max(a,0.000001),a);
  }
  textureStore(output,vec2i(id.xy),out);
}
