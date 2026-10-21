// Small glyphs: 1:1 copies of atlas coverage onto the output grid, tinted and premultiplied.
struct Config { size:vec4f };
struct Glyph { @location(0) dst:vec4i, @location(1) src:vec2u, @location(2) color:vec4f };
struct VertexOut { @builtin(position) position:vec4f, @location(0) @interpolate(flat) dst:vec2i,
 @location(1) @interpolate(flat) src:vec2u, @location(2) @interpolate(flat) color:vec4f };
@group(0) @binding(0) var<uniform> c:Config;
@group(0) @binding(1) var atlas:texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) i:u32, g:Glyph)->VertexOut {
 let corner=array<vec2f,6>(vec2f(0,0),vec2f(1,0),vec2f(0,1),vec2f(0,1),vec2f(1,0),vec2f(1,1))[i];
 let q=vec2f(g.dst.xy)+corner*vec2f(g.dst.zw);
 var o:VertexOut;
 o.position=vec4f(q.x/c.size.x*2.0-1.0,1.0-q.y/c.size.y*2.0,0,1);
 o.dst=g.dst.xy; o.src=g.src; o.color=g.color; return o;
}
@fragment fn paint(v:VertexOut)->@location(0) vec4f {
 let t=vec2i(v.src)+vec2i(floor(v.position.xy))-v.dst;
 let a=textureLoad(atlas,t,0).r*v.color.a; return vec4f(v.color.rgb*a,a);
}
