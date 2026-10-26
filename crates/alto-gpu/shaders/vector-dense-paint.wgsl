struct Config { matrix:vec4f, offset_size:vec4f, color:vec4f, slot:vec4u };
@group(0) @binding(0) var<uniform> c:Config;
@group(0) @binding(1) var mask0:texture_2d<f32>;
@group(0) @binding(2) var mask1:texture_2d<f32>;
@group(0) @binding(3) var mask2:texture_2d<f32>;
@group(0) @binding(4) var mask3:texture_2d<f32>;
@group(0) @binding(5) var mask4:texture_2d<f32>;
@group(0) @binding(6) var mask5:texture_2d<f32>;
@group(0) @binding(7) var mask6:texture_2d<f32>;
@group(0) @binding(8) var mask7:texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4f {
 let p=array<vec2f,3>(vec2f(-1,-1),vec2f(3,-1),vec2f(-1,3))[i];return vec4f(p,0,1);
}
@fragment fn paint(@builtin(position) p:vec4f)->@location(0) vec4f {
 var coverage:f32;
 switch c.slot.x {
  case 0u: {coverage=textureLoad(mask0,vec2i(p.xy),0).r;}
  case 1u: {coverage=textureLoad(mask1,vec2i(p.xy),0).r;}
  case 2u: {coverage=textureLoad(mask2,vec2i(p.xy),0).r;}
  case 3u: {coverage=textureLoad(mask3,vec2i(p.xy),0).r;}
  case 4u: {coverage=textureLoad(mask4,vec2i(p.xy),0).r;}
  case 5u: {coverage=textureLoad(mask5,vec2i(p.xy),0).r;}
  case 6u: {coverage=textureLoad(mask6,vec2i(p.xy),0).r;}
  default: {coverage=textureLoad(mask7,vec2i(p.xy),0).r;}
 }
 let a=coverage*c.color.a;return vec4f(c.color.rgb*a,a);
}
