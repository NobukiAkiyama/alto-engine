// Appended after q14.wgsl. Coverage is CPU-rasterized Q8 cell area stored as
// R8Unorm; unorm_byte is the only permitted floating-point operation.
struct VectorParams { color: vec4u, size: vec4u };
@group(0) @binding(0) var previous: texture_2d<u32>;
@group(0) @binding(1) var coverage: texture_2d<f32>;
@group(0) @binding(2) var output: texture_storage_2d<rgba16uint, write>;
@group(0) @binding(3) var<uniform> params: VectorParams;

fn div28(n: Wide) -> u32 {
  let adjusted = wide_add(n, Wide(1u << 27u, 0u));
  return (adjusted.lo >> 28u) | (adjusted.hi << 4u);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) id: vec3u) {
  if any(id.xy >= params.size.xy) { return; }
  let xy = vec2i(id.xy);
  let old = textureLoad(previous, xy, 0);
  let byte = unorm_byte(textureLoad(coverage, xy, 0).r);
  let k = byte_q14(byte);
  let f = k * params.color.a;
  let sa = (f + 8192u) >> 14u;
  var value = vec4u(0u);
  for (var channel = 0u; channel < 3u; channel++) {
    let source = div28(wide_mul_u32(params.color[channel], f));
    value[channel] = (source * 16384u + old[channel] * (16384u - sa) + 8192u) >> 14u;
  }
  value.a = (sa * 16384u + old.a * (16384u - sa) + 8192u) >> 14u;
  textureStore(output, xy, min(value, vec4u(16384u)));
}
