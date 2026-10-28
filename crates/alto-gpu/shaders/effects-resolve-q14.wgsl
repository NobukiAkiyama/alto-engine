// Render version 3 M5 resolve: premultiplied Q14 to tile bytes, the same
// `div_q(q * 255, 16384)` as the integer tile compositor, written as rgba8uint.
// q is 0..=16384, so the numerator is non-negative and `div_q` is (n + 8192) >> 14.
// `span` is the pixel-index rectangle [x0, y0, x1, y1) of the tile whose centers
// lie on the canvas, computed exactly by the host (renderer.rs `resolve_span`);
// pixels outside it are transparent. `info.x` is the cache slot.
struct Config { span:vec4u, info:vec4u };
@group(0) @binding(0) var input:texture_2d<u32>;
@group(0) @binding(1) var output:texture_storage_2d_array<rgba8uint,write>;
@group(0) @binding(2) var<uniform> c:Config;
@compute @workgroup_size(16,16) fn main(@builtin(global_invocation_id) id:vec3u){
 if any(id.xy>=vec2u(256)){return;}
 var bytes=vec4u(0u);
 if all(id.xy>=c.span.xy)&&all(id.xy<c.span.zw){
  let q=min(textureLoad(input,vec2i(id.xy),0),vec4u(16384u));
  bytes=min((q*255u+vec4u(8192u))>>vec4u(14u),vec4u(255u));
 }
 textureStore(output,vec2i(id.xy),i32(c.info.x),bytes);
}
