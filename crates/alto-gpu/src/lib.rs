pub use alto_scene::*;
pub use error::RenderError;

mod error;
pub mod glyph_atlas;
pub mod glyph_q8;
pub mod raster_q8;
pub mod renderer;
mod sample;
mod stroke_q8;

#[cfg(test)]
mod blend_ref;
#[cfg(test)]
mod cpu_composite;

/// Blend math lives in one shader. Composite and the effect pass both prepend it.
pub fn shader_with_blend(body: &str) -> String {
    format!("{}{body}", include_str!("../shaders/blend.wgsl"))
}

/// Assemble the v3 compositor: its bindings and 64-bit helpers, the Q14 helpers
/// `composite-v3-layout.wgsl` lacks, the integer atlas fetch, then the Q14 blend
/// and stage. Version 3 reads no float but the atlas texel (`unorm_byte`): the
/// command inverse and the tile origin are integers, every draw is the Q16
/// nearest (`SAMPLE_NEAREST`) or the integer 4-tap (`renderer::plain_draw_v3`),
/// and the canvas test is the host's exact pixel rectangle
/// (`renderer::resolve_span`). `v3_shader_sources_are_pinned` fixes the bytes.
pub fn integer_composite_shader() -> String {
    let q14 = include_str!("../shaders/q14.wgsl");
    // composite-v3-layout.wgsl already defines `Wide`; take only the functions it lacks.
    let div = &q14[q14.find("fn div_q(").unwrap()..q14.find("// Same 64-bit pair").unwrap()];
    let q14 = &q14[q14.find("fn wide_div_q14(").unwrap()..];
    format!(
        "{}\n{div}\n{q14}\n{}\n{}\n{}",
        include_str!("../shaders/composite-v3-layout.wgsl"),
        include_str!("../shaders/composite-v3-fetch.wgsl"),
        include_str!("../shaders/blend-q14.wgsl"),
        include_str!("../shaders/composite-q14.wgsl")
    )
}

/// Render version 3 M5 stages: Q14 helpers, the Q14 blend, then the stage modes.
/// The mask stages place their texel with the Q8 position helpers in
/// `effects-v3-wide.wgsl` (`pixel_q8`, `coord_q8`, `wide_round_shr16`).
/// `q14.wgsl` supplies the `Wide` they use.
pub fn integer_effects_shader() -> String {
    format!(
        "{}\n{}\n{}\n{}",
        include_str!("../shaders/q14.wgsl"),
        include_str!("../shaders/blend-q14.wgsl"),
        include_str!("../shaders/effects-v3-wide.wgsl"),
        include_str!("../shaders/effects-q14.wgsl")
    )
}

/// `v * 16384` rounded half away from zero, for a finite `v` with `|v| <= 16`
/// (levels gamma reaches 10). Exact: it is evaluated on the bits of `v`, not in float. The f32 product is
/// exact, but the f32 sum with one half is not (`0.5 - 2^-25` plus `0.5` rounds
/// to 1, so the largest f32 below `2^-15` would quantize to 1, not 0).
pub(crate) fn q14_round(v: f32) -> i32 {
    debug_assert!(v.is_finite() && v.abs() <= 16.0);
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

#[cfg(test)]
mod v3_shader_pins {
    /// The v3 WGSL is part of what fixes v3 pixels. Moving or deleting other shader
    /// files must not change a byte of it; a deliberate edit updates these hashes.
    #[test]
    fn v3_shader_sources_are_pinned() {
        let hash = |s: String| alto_core::ContentHash::of(s.as_bytes()).hex();
        assert_eq!(
            hash(super::integer_composite_shader()),
            "e82f1875f12b6f337f7011200eb1e8d2ec6565976f356432084f8eba76dbe3be"
        );
        assert_eq!(
            hash(super::integer_effects_shader()),
            "33dfbd287ad597e3a45eb7133850002b4c1aa920540f5fa6dbba937d35ff7857"
        );
    }
}
