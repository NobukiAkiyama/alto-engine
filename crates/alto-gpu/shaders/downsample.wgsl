@group(0) @binding(0) var input_image:texture_2d<f32>;
@group(0) @binding(1) var output_image:texture_storage_2d_array<rgba8unorm,write>;
@group(0) @binding(2) var<uniform> config:vec4u;
@compute @workgroup_size(16,16) fn main(@builtin(global_invocation_id) id:vec3u){if any(id.xy>=vec2u(256)){return;}let p=vec2i(id.xy)*2;let a=textureLoad(input_image,p,0)+textureLoad(input_image,p+vec2i(1,0),0)+textureLoad(input_image,p+vec2i(0,1),0)+textureLoad(input_image,p+vec2i(1,1),0);textureStore(output_image,vec2i(id.xy),i32(config.x),a*.25);}
