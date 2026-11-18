struct Dab { center_radius: vec4f, extra: vec4f };
struct Config { color: vec4f, params: vec4f, origin: vec4f, info: vec4u };
@group(0) @binding(0) var base: texture_2d<f32>;
@group(0) @binding(1) var output: texture_storage_2d<rgba8unorm,write>;
@group(0) @binding(2) var<storage,read_write> coverage: array<f32>;
@group(0) @binding(3) var<storage,read> dabs: array<Dab>;
@group(0) @binding(4) var<uniform> config: Config;
@group(0) @binding(5) var selection: texture_2d<f32>;
@group(0) @binding(6) var tip: texture_2d<f32>;

fn tipTexel(p: vec2i, size: vec2i) -> f32 {
  if any(p < vec2i(0)) || any(p >= size) { return 0.0; }
  return textureLoad(tip, p, 0).r;
}

fn tipCoverage(pos: vec2f, radius: f32) -> f32 {
  let size = vec2f(textureDimensions(tip));
  let p = pos * (max(size.x, size.y) / (2.0 * radius)) + size * 0.5 - 0.5;
  if any(p <= vec2f(-1.0)) || any(p >= size) { return 0.0; }
  let corner = vec2i(floor(p));
  let f = fract(p);
  let dims = vec2i(size);
  let top = mix(tipTexel(corner, dims), tipTexel(corner + vec2i(1, 0), dims), f.x);
  let bottom = mix(tipTexel(corner + vec2i(0, 1), dims), tipTexel(corner + vec2i(1, 1), dims), f.x);
  return mix(top, bottom, f.y);
}

@compute @workgroup_size(16,16) fn main(@builtin(global_invocation_id) id:vec3u) {
  if any(id.xy>=vec2u(256)) { return; }
  let p=config.origin.xy+vec2f(id.xy)+0.5;let idx=id.y*256u+id.x;
  var v=coverage[idx];
  for(var i=0u;i<config.info.x;i++) {
    let d=dabs[i];
    let radius=max(0.01,d.center_radius.z);
    var shape: f32;
    let hasTip = (config.info.w & 1u) != 0u;
    if !hasTip && d.extra.z>=1.0 {
      let dist=distance(p,d.center_radius.xy)/radius;
      shape=1.0-smoothstep(config.params.x,1.0,dist);
    } else {
      let q=p-d.center_radius.xy;
      let u=q.x*d.extra.x+q.y*d.extra.y;
      let minor=(-q.x*d.extra.y+q.y*d.extra.x)/max(d.extra.z,0.01);
      if hasTip {
        shape=tipCoverage(vec2f(u,minor),radius);
      } else {
        let dist=length(vec2f(u,minor))/radius;
        shape=1.0-smoothstep(config.params.x,1.0,dist);
      }
    }
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
