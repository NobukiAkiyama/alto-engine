struct Config { origin_scale:vec4f, info:vec4u };
@group(0) @binding(0) var input:texture_2d<f32>;
@group(0) @binding(1) var output:texture_storage_2d_array<rgba8unorm,write>;
@group(0) @binding(2) var<uniform> c:Config;
@compute @workgroup_size(16,16) fn main(@builtin(global_invocation_id) id:vec3u){
 if any(id.xy>=vec2u(256)){return;}
 let p=c.origin_scale.xy+(vec2f(id.xy)+0.5)*c.origin_scale.z;var color=vec4f(0);
 if p.x>=0.0&&p.y>=0.0&&p.x<c.origin_scale.w&&p.y<f32(c.info.y){color=textureLoad(input,vec2i(id.xy),0);}
 textureStore(output,vec2i(id.xy),i32(c.info.x),color);
}
