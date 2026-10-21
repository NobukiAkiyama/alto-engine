struct View { center_zoom: vec4f, screen: vec4f, canvas: vec4f };
struct Tile { origin_scale: vec4f, info: vec4u };
@group(0) @binding(0) var cache: texture_2d_array<f32>;
@group(0) @binding(1) var sampling: sampler;
@group(0) @binding(2) var<uniform> view: View;
@group(0) @binding(3) var<storage, read> tiles: array<Tile>;
struct Out { @builtin(position) pos: vec4f, @location(0) uv: vec2f, @location(1) @interpolate(flat) slot: u32, @location(2) doc: vec2f };
@vertex fn vs(@builtin(vertex_index) vi:u32,@builtin(instance_index) ii:u32)->Out {
  let uv=array<vec2f,6>(vec2f(0,0),vec2f(1,0),vec2f(0,1),vec2f(0,1),vec2f(1,0),vec2f(1,1))[vi];
  let t=tiles[ii];let p=t.origin_scale.xy+uv*256.0*t.origin_scale.z;
  let q=(p-view.center_zoom.xy)*view.center_zoom.z;
  let c=cos(view.center_zoom.w);let s=sin(view.center_zoom.w);
  let r=vec2f(c*q.x-s*q.y,s*q.x+c*q.y);
  var o:Out;o.pos=vec4f(r.x/view.screen.x*2.0,-r.y/view.screen.y*2.0,0,1);o.uv=uv;o.slot=t.info.x;o.doc=p;return o;
}
@fragment fn fs(i:Out)->@location(0) vec4f {
  if i.doc.x<0.0 || i.doc.y<0.0 || i.doc.x>=view.canvas.x || i.doc.y>=view.canvas.y { discard; }
  let a=textureSample(cache,sampling,i.uv,i32(i.slot));
  let checker=select(0.86,0.95,(u32(floor(i.doc.x/16.0))+u32(floor(i.doc.y/16.0)))%2u==0u);
  return vec4f(a.rgb+vec3f(checker)*(1.0-a.a),1.0);
}
