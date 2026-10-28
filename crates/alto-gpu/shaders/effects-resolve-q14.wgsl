// Render version 3 M5 resolve: premultiplied Q14 to tile bytes, the same
// `div_q(q * 255, 16384)` as the integer tile compositor, written as rgba8uint.
// q is 0..=16384, so the numerator is non-negative and `div_q` is (n + 8192) >> 14.
struct Config { origin_scale:vec4f, info:vec4u };
@group(0) @binding(0) var input:texture_2d<u32>;
@group(0) @binding(1) var output:texture_storage_2d_array<rgba8uint,write>;
@group(0) @binding(2) var<uniform> c:Config;
@compute @workgroup_size(16,16) fn main(@builtin(global_invocation_id) id:vec3u){
 if any(id.xy>=vec2u(256)){return;}
 let p=c.origin_scale.xy+(vec2f(id.xy)+0.5)*c.origin_scale.z;var bytes=vec4u(0u);
 if p.x>=0.0&&p.y>=0.0&&p.x<c.origin_scale.w&&p.y<f32(c.info.y){
  let q=min(textureLoad(input,vec2i(id.xy),0),vec4u(16384u));
  bytes=min((q*255u+vec4u(8192u))>>vec4u(14u),vec4u(255u));
 }
 textureStore(output,vec2i(id.xy),i32(c.info.x),bytes);
}
