struct View {
  pan_zoom: vec4f,
  viewport: vec4f,
};
@group(0) @binding(0) var tile_texture: texture_2d_array<f32>;
@group(0) @binding(1) var tile_sampler: sampler;
@group(0) @binding(2) var<uniform> view: View;

struct VertexOut {
  @builtin(position) position: vec4f,
  @location(0) uv: vec2f,
  @location(1) @interpolate(flat) layer: u32,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32) -> VertexOut {
  let local = array<vec2f, 6>(
    vec2f(0.0, 0.0), vec2f(1.0, 0.0), vec2f(0.0, 1.0),
    vec2f(0.0, 1.0), vec2f(1.0, 0.0), vec2f(1.0, 1.0)
  )[vertex];
  let origin = vec2f(f32(instance % 10u), f32(instance / 10u)) * 256.0;
  let screen = (origin + local * 256.0 - view.pan_zoom.xy) * view.pan_zoom.z;
  var out: VertexOut;
  out.position = vec4f(screen.x / view.viewport.x * 2.0 - 1.0, 1.0 - screen.y / view.viewport.y * 2.0, 0.0, 1.0);
  out.uv = local;
  out.layer = instance;
  return out;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4f {
  return textureSample(tile_texture, tile_sampler, input.uv, input.layer);
}
