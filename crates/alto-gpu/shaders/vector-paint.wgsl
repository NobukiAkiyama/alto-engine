struct Config { matrix:vec4f, offset_size:vec4f, color:vec4f };
@group(0) @binding(0) var<uniform> c:Config;
@group(0) @binding(1) var mask:texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4f {
 let p=array<vec2f,3>(vec2f(-1,-1),vec2f(3,-1),vec2f(-1,3))[i];return vec4f(p,0,1);
}
@fragment fn paint(@builtin(position) p:vec4f)->@location(0) vec4f {
 let a=textureLoad(mask,vec2i(p.xy),0).r*c.color.a;return vec4f(c.color.rgb*a,a);
}
