// Appended after q14.wgsl. Render version 3 text paint
// (doc/design/2026-10-28-v3-integer-text.md "着色と重ね"): each glyph's
// CPU-rasterized coverage, cropped to the ROI and packed into an R8Unorm
// scratch, painted in instance order with the Q14 normal over its straight
// colour. The same formula as vector-q14.wgsl; unorm_byte is the only
// floating-point operation.
struct GlyphQ14 { dst: vec4i, src: vec4u, color: vec4u };
struct GlyphParams { count: u32, w: u32, h: u32, pad: u32 };
@group(0) @binding(0) var<uniform> params: GlyphParams;
@group(0) @binding(1) var<storage, read> glyphs: array<GlyphQ14>;
@group(0) @binding(2) var scratch: texture_2d<f32>;
@group(0) @binding(3) var previous: texture_2d<u32>;
@group(0) @binding(4) var output: texture_storage_2d<rgba16uint, write>;

fn div28(n: Wide) -> u32 {
  let adjusted = wide_add(n, Wide(1u << 27u, 0u));
  return (adjusted.lo >> 28u) | (adjusted.hi << 4u);
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
  if any(id.xy >= vec2u(params.w, params.h)) { return; }
  let p = vec2i(id.xy);
  var acc = textureLoad(previous, p, 0);
  for (var i = 0u; i < params.count; i++) {
    let g = glyphs[i];
    let l = p - g.dst.xy;
    if any(l < vec2i(0)) || any(l >= g.dst.zw) { continue; }
    let byte = unorm_byte(textureLoad(scratch, vec2i(g.src.xy) + l, 0).r);
    if byte == 0u { continue; }
    let f = byte_q14(byte) * g.color.a;
    let sa = (f + 8192u) >> 14u;
    var value = vec4u(0u);
    for (var c = 0u; c < 3u; c++) {
      let source = div28(wide_mul_u32(g.color[c], f));
      value[c] = (source * 16384u + acc[c] * (16384u - sa) + 8192u) >> 14u;
    }
    value.a = (sa * 16384u + acc.a * (16384u - sa) + 8192u) >> 14u;
    acc = min(value, vec4u(16384u));
  }
  textureStore(output, p, acc);
}
