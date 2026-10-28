pub use alto_scene::*;
pub use error::RenderError;

mod error;
pub mod glyph_atlas;
pub mod renderer;
mod sample;

#[cfg(test)]
mod blend_ref;
#[cfg(test)]
mod cpu_composite;

/// Blend math lives in one shader. Composite and the effect pass both prepend it.
pub fn shader_with_blend(body: &str) -> String {
    format!("{}{body}", include_str!("../shaders/blend.wgsl"))
}

/// Assemble v3 from the frozen coordinate implementation and the shared Q14 helpers.
/// The v1/v2 shader sources and their pipelines remain unchanged.
pub fn integer_composite_shader() -> String {
    let composite = include_str!("../shaders/composite.wgsl");
    let q14 = include_str!("../shaders/q14.wgsl");
    let prefix = composite.split("fn sample_texel").next().unwrap();
    let prefix = prefix.replace(
        "struct Command { inv0: vec4f, inv1: vec4f, info: vec4u, trans: vec4u };",
        "struct Command { inv0: vec4f, translation: vec2f, opacity_q: u32, mode: u32, info: vec4u, trans: vec4u };",
    );
    let prefix = prefix.replace(
        "var output: texture_storage_2d_array<rgba8unorm, write>;",
        "var<storage, read_write> output: array<u32>;",
    );
    let sample = &composite
        [composite.find("fn sample_byte").unwrap()..composite.find("fn fetch_legacy").unwrap()];
    let begin = sample.find("  let s = textureLoad").unwrap();
    let end = sample.find("fn channel_byte").unwrap();
    let sample = format!(
        "{}  let s = textureLoad(atlas, local, i32(t.slot), 0);\n  return vec4u(unorm_byte(s.r),unorm_byte(s.g),unorm_byte(s.b),unorm_byte(s.a));\n}}\n{}",
        &sample[..begin],
        &sample[end..]
    );
    let fetch = &composite
        [composite.find("fn fetch_integer").unwrap()..composite.find("fn fetch(c:").unwrap()];
    let fetch = fetch
        .replace("-> vec4f", "-> vec4u")
        .replace("vec4f(0.0)", "vec4u(0u)")
        .replace(
            "return vec4f(f32(r), f32(g), f32(bl), f32(al)) / 255.0;",
            "return vec4u(r,g,bl,al);",
        );
    // composite.wgsl already defines `Wide`; take only the functions it lacks.
    let div = &q14[q14.find("fn div_q(").unwrap()..q14.find("// Same 64-bit pair").unwrap()];
    let q14 = &q14[q14.find("fn wide_div_q14(").unwrap()..];
    format!(
        "{prefix}\n{div}\n{q14}\n{sample}\n{fetch}\n{}\n{}\n{}",
        r#"
fn fetch_bytes(c: Command,p:vec2f,ix:vec2i)->vec4u {
  if c.info.w!=0u {return fetch_integer(c,ix);}
  let q=vec2f(c.inv0.x*p.x+c.inv0.z*p.y+c.translation.x,c.inv0.y*p.x+c.inv0.w*p.y+c.translation.y);
  return sample_byte(c,i32(floor(q.x)),i32(floor(q.y)));
}
"#,
        include_str!("../shaders/blend-q14.wgsl"),
        include_str!("../shaders/composite-q14.wgsl")
    )
}

/// Render version 3 M5 stages: Q14 helpers, the Q14 blend, then the stage modes.
/// The mask stages place their texel with the Q8 position of the frozen
/// `composite.wgsl` (`pixel_q8`, `coord_q8`, `wide_round_shr16`), copied from that
/// source here rather than restated. `q14.wgsl` supplies the `Wide` they use.
pub fn integer_effects_shader() -> String {
    let composite = include_str!("../shaders/composite.wgsl");
    let between = |from: &str, to: &str| {
        &composite[composite.find(from).unwrap()..composite.find(to).unwrap()]
    };
    format!(
        "{}\n{}\n{}{}{}\n{}",
        include_str!("../shaders/q14.wgsl"),
        include_str!("../shaders/blend-q14.wgsl"),
        between("struct WideSum", "@group(0)"),
        between("fn wide_shl8", "fn wide_negative"),
        between("// Signed 64-bit add", "fn sample_texel"),
        include_str!("../shaders/effects-q14.wgsl")
    )
}

/// `v * 16384` rounded half away from zero, for a finite `v` with `|v| <= 4`.
/// Exact: it is evaluated on the bits of `v`, not in float. The f32 product is
/// exact, but the f32 sum with one half is not (`0.5 - 2^-25` plus `0.5` rounds
/// to 1, so the largest f32 below `2^-15` would quantize to 1, not 0).
pub(crate) fn q14_round(v: f32) -> i32 {
    debug_assert!(v.is_finite() && v.abs() <= 4.0);
    let bits = v.to_bits();
    let exponent = ((bits >> 23) & 0xff) as i32;
    let fraction = u64::from(bits & 0x7f_ffff);
    // |v| = m * 2^e, subnormals included.
    let (m, e) = if exponent == 0 {
        (fraction, -149)
    } else {
        (fraction | 0x80_0000, exponent - 150)
    };
    // |v| * 2^14 = m * 2^shift. With m < 2^24, a shift below -40 is under 1/2.
    let shift = e + 14;
    let magnitude = if shift >= 0 {
        m << shift
    } else if shift < -40 {
        0
    } else {
        (m + (1 << (-shift - 1))) >> -shift
    } as i32;
    if bits >> 31 == 0 {
        magnitude
    } else {
        -magnitude
    }
}

/// Quantize the document's f32 opacity on the host, before uploading commands:
/// clamped to 0..=1, then `q14_round`. NaN is 0.
pub(crate) fn composite_opacity_q14(v: f32) -> i32 {
    if v.is_nan() {
        return 0;
    }
    q14_round(v.clamp(0.0, 1.0))
}
