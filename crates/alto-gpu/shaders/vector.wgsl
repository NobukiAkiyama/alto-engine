// Positions are local to the path origin. ROI translation is rebased in f64 on CPU.
struct Config { matrix:vec4f, offset_size:vec4f, color:vec4f };
@group(0) @binding(0) var<uniform> c:Config;
struct VertexOut { @builtin(position) position:vec4f };
@vertex fn vs(@location(0) p:vec2f)->VertexOut {
 let q=vec2f(c.matrix.x*p.x+c.matrix.z*p.y,c.matrix.y*p.x+c.matrix.w*p.y)+c.offset_size.xy;
 var o:VertexOut; o.position=vec4f(q.x/c.offset_size.z*2.0-1.0,1.0-q.y/c.offset_size.w*2.0,0,1);return o;
}
@fragment fn coverage()->@location(0) f32 {return 1.0;}
