//! Coordinate math shared by the passes: reaches, inverses, mask grids and v3 integer draws.
use super::*;

pub(super) fn mode(m: RenderBlend) -> f32 {
    m as u32 as f32
}

/// Q14 quantize of a color-adjust setting. 1.0 is 16384. The value is clamped
/// to [-1, 4], and `value * 16384` is rounded half away from zero exactly
/// (`q14_round`, integer). A value that is not finite is 0.
/// Render version 3 stores these Q14 integers in the mode 17 uniform.
pub(crate) fn color_adjust_q14(value: f32) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    crate::q14_round(value.clamp(-1.0, 4.0))
}

/// Q14 quantize of a levels gamma, clamped to the document range [0.1, 10]
/// rather than the [-1, 4] of [`color_adjust_q14`]. A value that is not finite is 1.
pub(crate) fn levels_gamma_q14(value: f32) -> i32 {
    if !value.is_finite() {
        return 1 << 14;
    }
    crate::q14_round(value.clamp(0.1, 10.0))
}

/// Where a v3 stage reads the atlas: the mask of modes 21 to 23, or the raster
/// tiles of mode 18. The output ROI uses integer document pixels for scale >= 1,
/// or grid units with a negative reciprocal scale for levels -6..-1. The Q16
/// inverse and its i64 Q16 translation bits (`effects-q14.wgsl` `Config.grid`
/// rows 3 to 5). The positions are always `SAMPLE_INTEGER` (`sample.rs`), and the
/// texel is the nearest one (`effects-q14.wgsl` `lookup`).
pub(crate) struct MaskSampler {
    pub origin: [i32; 3],
    pub inverse: [i32; 4],
    pub translation: [u32; 4],
}

/// An f32 that is exactly an `i32`.
pub(super) fn exact_i32(v: f32) -> Option<i32> {
    (v.fract() == 0.0 && (-2_147_483_648.0..2_147_483_648.0).contains(&v)).then_some(v as i32)
}

/// The sampler for a v3 mask with document-to-pixel `transform`, read at an
/// output ROI with origin `origin` and level scale `scale`. Every v3 mask takes
/// the Q16 lookup (`SAMPLE_INTEGER`), axis-aligned unit inverses included; there
/// is no f32 lookup. The Q16 inverse is `integer_inverse`, and the Q8 pixel
/// center needs an origin and a scale that are exact integers, with the scale at
/// least 1. `None` when any of that fails or `transform` is singular: the host
/// refuses the render (DQ3-4).
#[cfg(test)]
pub(crate) fn mask_sampler(
    transform: [f64; 6],
    origin: [f32; 2],
    scale: f32,
) -> Option<MaskSampler> {
    let x = exact_i32(origin[0])?;
    let y = exact_i32(origin[1])?;
    let scale = exact_i32(scale).filter(|s| *s >= 1)?;
    let q = crate::sample::integer_inverse(inverse(transform)?)?;
    Some(MaskSampler {
        origin: [x, y, scale],
        inverse: q.m,
        translation: crate::sample::pack_wide(q.tx, q.ty),
    })
}

pub(crate) fn fixed_mask_sampler(
    transform: &alto_core::FixedAffine,
    origin: [i32; 3],
) -> Option<MaskSampler> {
    if origin[2] == 0
        || (origin[2] < 0 && !matches!(origin[2].checked_neg()?, 2 | 4 | 8 | 16 | 32 | 64))
    {
        return None;
    }
    let q = crate::sample::fixed_inverse(transform)??;
    Some(MaskSampler {
        origin,
        inverse: q.m,
        translation: crate::sample::pack_wide(q.tx, q.ty),
    })
}

/// The Q16 inverse of a [`MaskSampler`], unpacked.
pub(crate) fn sampler_inverse(s: &MaskSampler) -> crate::sample::IntegerInverse {
    let wide = |lo: u32, hi: u32| ((u64::from(hi) << 32) | u64::from(lo)) as i64;
    let t = s.translation;
    crate::sample::IntegerInverse {
        m: s.inverse,
        tx: wide(t[0], t[1]),
        ty: wide(t[2], t[3]),
    }
}

/// What the integer lookup (`pixel_q8`, `coord_q8`) of the pixels
/// `[x0, x1) x [y0, y1)` of a target at `origin` and level `scale` reads,
/// evaluated exactly in i128 at the four corners. The Q24 sum is affine in the
/// pixel index, and the half-away rounding and the floors are monotone, so the
/// corners bound every pixel.
///
/// i128 bound (Linus 05:25). The inputs are a Q16 coefficient `|m| < 2^31`
/// (`quantize_coeff`), a Q16 translation `|t| < 2^53` (`quantize_translation`),
/// and from the callers an origin `|o| < 2^70` (a tile key below 2^31 times a
/// tile of at most `256 << 30` px, `invalidate_reach`), a pixel index
/// `0 <= i < 2^39` and a scale `1 <= s < 2^31`. Then a pixel center
/// `o * 256 + (i * 256 + 128) * s` is below `2^78 + 2^79 < 2^80`, each product
/// `m * center` below `2^111`, `t * 256` below `2^61`, and the Q24 sum below
/// `2^113`: far inside i128. Every step is still checked: an input outside
/// those bounds gives `None`, which `v3_reach` refuses (GpuUnsupported) and
/// `invalidate_reach` treats as reaching the tile.
pub(crate) struct ExactReach {
    /// Inclusive source tile keys `[min x, min y, max x, max y]`, with `reach`
    /// more texels on each axis (1 for the 4-tap, 0 for nearest).
    pub keys: [i128; 4],
    /// Whether every corner's Q8 pixel center fits i32 (the shader's `pixel_q8`).
    pub centers_fit: bool,
    /// Per axis, whether some corner's source Q8 leaves i32: the shader drops
    /// (makes transparent) exactly those pixels, since its 64-bit sums cannot
    /// overflow first (each product is below 2^62, the translation below 2^61).
    pub dropped: [bool; 2],
}

pub(crate) fn exact_reach(
    inv: &crate::sample::IntegerInverse,
    origin: [i64; 2],
    scale: i64,
    span: [i64; 4],
    reach: i32,
) -> Option<ExactReach> {
    exact_reach_q8(
        inv,
        origin.map(|o| i128::from(o) * 256),
        i128::from(scale) * 256,
        span,
        reach,
    )
}

pub(super) fn exact_reach_q8(
    inv: &crate::sample::IntegerInverse,
    origin: [i128; 2],
    step: i128,
    span: [i64; 4],
    reach: i32,
) -> Option<ExactReach> {
    let center = |o: i128, i: i64| {
        o.checked_add(i128::from(i).checked_mul(step)?)?
            .checked_add(step / 2)
    };
    let source = |m0: i32, px: i128, m1: i32, py: i128, t: i64| {
        let sum = i128::from(m0)
            .checked_mul(px)?
            .checked_add(i128::from(m1).checked_mul(py)?)?
            .checked_add(i128::from(t).checked_mul(256)?)?;
        let half = 1i128 << 15;
        Some(
            (if sum >= 0 {
                sum.checked_add(half)?
            } else {
                sum.checked_sub(half)?
            }) >> 16,
        )
    };
    let i32_range = i128::from(i32::MIN)..=i128::from(i32::MAX);
    let mut out = ExactReach {
        keys: [i128::MAX, i128::MAX, i128::MIN, i128::MIN],
        centers_fit: true,
        dropped: [false; 2],
    };
    let [x0, y0, x1, y1] = span;
    for (x, y) in [(x0, y0), (x1 - 1, y0), (x0, y1 - 1), (x1 - 1, y1 - 1)] {
        let (px, py) = (center(origin[0], x)?, center(origin[1], y)?);
        out.centers_fit &= i32_range.contains(&px) && i32_range.contains(&py);
        let q = [
            source(inv.m[0], px, inv.m[2], py, inv.tx)?,
            source(inv.m[1], px, inv.m[3], py, inv.ty)?,
        ];
        for (axis, q) in q.into_iter().enumerate() {
            out.dropped[axis] |= !i32_range.contains(&q);
            let texel = q >> 8;
            out.keys[axis] = out.keys[axis].min(texel >> 8);
            out.keys[axis + 2] = out.keys[axis + 2].max((texel + i128::from(reach)) >> 8);
        }
    }
    Some(out)
}

/// The tiles a v3 atlas read (a plain draw, mode 18's raster, a mask) needs on
/// the pixels `span` of a target at `origin` and `scale` (Linus 04:40, DQ3-4):
/// `Ok(None)` when no tile of `keys` (the source's tiles) is in the
/// [`exact_reach`] range, so the read contributes nothing and is skipped;
/// otherwise the inclusive key range to select. `Err` only when pixels that do
/// reach a tile cannot be computed exactly by the shader: a Q8 pixel center
/// outside i32 (a tile origin at 2^23 px or past), or a pixel the shader drops
/// for a source Q8 outside i32 that could read a listed tile (the tiles in
/// range must lie in texels `-2^23 + reach ..= 2^23 - 1` on every axis that
/// drops a pixel).
pub(crate) fn v3_reach(
    inv: &crate::sample::IntegerInverse,
    origin: [i64; 2],
    scale: i64,
    span: [i64; 4],
    reach: i32,
    keys: impl Iterator<Item = TileKey>,
) -> std::result::Result<Option<[i32; 4]>, ()> {
    v3_reach_q8(
        inv,
        origin.map(|o| i128::from(o) * 256),
        i128::from(scale) * 256,
        span,
        reach,
        keys,
    )
}

pub(crate) fn v3_reach_q8(
    inv: &crate::sample::IntegerInverse,
    origin: [i128; 2],
    step: i128,
    span: [i64; 4],
    reach: i32,
    keys: impl Iterator<Item = TileKey>,
) -> std::result::Result<Option<[i32; 4]>, ()> {
    if span[0] >= span[2] || span[1] >= span[3] {
        return Ok(None);
    }
    let r = exact_reach_q8(inv, origin, step, span, reach).ok_or(())?;
    let mut present = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    for k in keys {
        let (x, y) = (i128::from(k.tx), i128::from(k.ty));
        if (r.keys[0]..=r.keys[2]).contains(&x) && (r.keys[1]..=r.keys[3]).contains(&y) {
            present = [
                present[0].min(k.tx),
                present[1].min(k.ty),
                present[2].max(k.tx),
                present[3].max(k.ty),
            ];
        }
    }
    if present[0] > present[2] {
        return Ok(None);
    }
    if !r.centers_fit {
        return Err(());
    }
    let low = -(1i64 << 23) + i64::from(reach);
    let high = (1i64 << 23) - 1;
    for axis in 0..2 {
        let (first, last) = (
            i64::from(present[axis]) * 256,
            i64::from(present[axis + 2]) * 256 + 255,
        );
        if r.dropped[axis] && (first < low || last > high) {
            return Err(());
        }
    }
    let clamp = |v: i128| v.clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as i32;
    Ok(Some(r.keys.map(clamp)))
}

/// The integer texel map of one input of a v3 mask stage (`effects-q14.wgsl`
/// `input_texel`). `out` and `input` are ROIs as `[x, y, scale]`, and `size` is
/// the output size in pixels. Equal ROIs map pixel `i` to texel `i`: `[0; 4]`.
/// Otherwise the texel of output pixel `i` is `floor((p - in_x) / in_scale)` at
/// the center `p = out_x + (i + 1/2) * out_scale`. Both ROIs are converted to
/// the finer common power-of-two grid, so this also covers levels -6..-1.
/// The doubled numerator, step and divisor must fit the shader's i32 words.
pub(crate) fn input_map(out: [f32; 3], size: [u32; 2], input: [f32; 3]) -> Option<[i32; 4]> {
    if out == input {
        return Some([0; 4]);
    }
    let [ox, oy, out_level] = crate::raster_q8::roi_grid(out[0], out[1], out[2])?;
    let [ix, iy, in_level] = crate::raster_q8::roi_grid(input[0], input[1], input[2])?;
    let common = out_level.min(in_level);
    let out_step = 1_i128.checked_shl(u32::try_from(out_level - common).ok()?)?;
    let in_step = 1_i128.checked_shl(u32::try_from(in_level - common).ok()?)?;
    let step = 2_i128.checked_mul(out_step)?;
    let denominator = 2_i128.checked_mul(in_step)?;
    let numerator = |o: i64, i: i64, len: u32| -> Option<i32> {
        let base = (i128::from(o).checked_mul(out_step)?)
            .checked_sub(i128::from(i).checked_mul(in_step)?)?
            .checked_mul(2)?
            .checked_add(out_step)?;
        let last = i128::from(len.max(1) - 1).checked_mul(step)?;
        i32::try_from(last).ok()?;
        i32::try_from(base.checked_add(last)?).ok()?;
        i32::try_from(base).ok()
    };
    Some([
        numerator(ox, ix, size[0])?,
        numerator(oy, iy, size[1])?,
        i32::try_from(step).ok()?,
        i32::try_from(denominator).ok()?,
    ])
}

/// The pixel-index rectangle `[x0, y0, x1, y1)` of a 256² tile whose pixel
/// centers lie on the canvas, for `effects-resolve-q14.wgsl`. The tile has
/// integer origin `origin` and level scale `scale`; pixel `i` is on the canvas
/// when `0 <= origin + (i + 1/2) * scale < end`, the test of the v1/v2 resolve,
/// here doubled and exact in i64. An empty rectangle is all zero.
pub(crate) fn resolve_span(origin: [i64; 2], scale: i64, end: [i64; 2]) -> [u32; 4] {
    let axis = |o: i64, e: i64| {
        let inside = |i: i64| (0..2 * e).contains(&(2 * o + (2 * i + 1) * scale));
        let lo = (0..256).find(|&i| inside(i)).unwrap_or(256);
        let hi = (lo..256)
            .take_while(|&i| inside(i))
            .last()
            .map_or(lo, |i| i + 1);
        (lo as u32, hi as u32)
    };
    let (x0, x1) = axis(origin[0], end[0]);
    let (y0, y1) = axis(origin[1], end[1]);
    if x0 == x1 || y0 == y1 {
        return [0; 4];
    }
    [x0, y0, x1, y1]
}

/// How a version 3 plain draw (`integer_composite_shader`) reads its tiles in
/// the level-0 display tile `key`: the Q16 inverse, the filter
/// (`SAMPLE_NEAREST` for an axis-aligned unit inverse, `axis_unit_q16`, else
/// the integer 4-tap `SAMPLE_INTEGER`), and the inclusive tile keys
/// `[min tx, min ty, max tx, max ty]` its on-canvas pixels read (`None`: no
/// pixel of the tile is on the canvas). There is no f32 fetch.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PlainDraw {
    pub inverse: crate::sample::IntegerInverse,
    pub sample: u32,
    pub keys: Option<[i32; 4]>,
}

impl PlainDraw {
    /// Whether the draw reads source tile `k`.
    pub(super) fn reads(&self, k: TileKey) -> bool {
        self.keys
            .is_some_and(|[x0, y0, x1, y1]| (x0..=x1).contains(&k.tx) && (y0..=y1).contains(&k.ty))
    }
    /// The draw command. `submit_composite` turns the opacity and the mode into
    /// their integers.
    pub(super) fn command(
        &self,
        opacity: f32,
        blend: RenderBlend,
        start: u32,
        count: u32,
    ) -> Command {
        Command {
            inv0: self.inverse.m.map(|v| f32::from_bits(v.cast_unsigned())),
            inv1: [0.0, 0.0, opacity, mode(blend)],
            info: [0, start, count, self.sample],
            trans: crate::sample::pack_wide(self.inverse.tx, self.inverse.ty),
        }
    }
}

/// The [`PlainDraw`] of a draw with an integer Q16 inverse and source tiles `tiles`
/// in the level-0 display tile `key` of a canvas ending at `end`. The pixels are
/// those of `resolve_span`, and the tiles are chosen by [`v3_reach`] (the 4-tap
/// also reads the next texel on each axis): a draw that reaches none of its
/// tiles has `keys: None` and is skipped. Refused (`GpuUnsupported`, DQ3-4) when
/// pixels that reach a tile have no exact
/// shader value; never sampled in f32.
pub(crate) fn plain_draw_v3(
    inverse: crate::sample::IntegerInverse,
    key: CacheKey,
    end: [i32; 2],
    tiles: impl Iterator<Item = TileKey>,
) -> Result<PlainDraw> {
    let unsupported = |what: &str| RenderError::GpuUnsupported(format!("v3 draw {what}"));
    if key.0 != 0 {
        return Err(RenderError::Internal(
            "v3 plain tile above level 0 (the pyramid builds those)".into(),
        ));
    }
    let origin = [key.1, key.2].map(|k| i64::from(k) * 256);
    let nearest = crate::sample::axis_unit_q16(inverse.m);
    let span = resolve_span(origin, 1, [i64::from(end[0]), i64::from(end[1])]).map(i64::from);
    let keys = v3_reach(&inverse, origin, 1, span, i32::from(!nearest), tiles)
        .map_err(|()| unsupported("ROI has no integer tile range"))?;
    Ok(PlainDraw {
        inverse,
        sample: if nearest {
            crate::sample::SAMPLE_NEAREST
        } else {
            crate::sample::SAMPLE_INTEGER
        },
        keys,
    })
}

/// The integer uniform of a v3 M5 stage, `effects-q14.wgsl` `Config.grid`:
/// row 0 the Q14 settings, rows 1 and 2 the `input_map` of inputs `a` and `b`,
/// rows 3 to 5 the atlas lookup: the mask of modes 21 to 23 or the raster of
/// mode 18 (zero without one; `mask_sampler` builds both). The rows go into the
/// `curve` slots of the M5 uniform, which no v3 stage reads otherwise. That
/// uniform is shared with versions 1 and 2 and its Rust type is `[f32; 64]`,
/// so the integers travel as f32 bit patterns (`f32::from_bits`, a copy). The
/// shader declares the slots `array<vec4i, 16>`; no float is loaded or computed.
pub(crate) fn mask_grid(
    settings: [i32; 4],
    a: [i32; 4],
    b: [i32; 4],
    sampler: Option<&MaskSampler>,
) -> [f32; 64] {
    let mut rows = [[0u32; 4]; 16];
    rows[0] = settings.map(i32::cast_unsigned);
    rows[1] = a.map(i32::cast_unsigned);
    rows[2] = b.map(i32::cast_unsigned);
    if let Some(s) = sampler {
        rows[3] = [s.origin[0], s.origin[1], s.origin[2], 0].map(i32::cast_unsigned);
        rows[4] = s.inverse.map(i32::cast_unsigned);
        rows[5] = s.translation;
    }
    let mut out = [0.0; 64];
    for (slot, bits) in out.iter_mut().zip(rows.as_flattened()) {
        *slot = f32::from_bits(*bits);
    }
    out
}

/// The feature that render version 3 has no integer definition for, or `None`.
/// Version 3 draws plain scenes with the integer tile compositor and M5 stages
/// for all document effects, adjustments, masks, filled and stroked vectors,
/// clipping and baked text. Direct layout runs are an invalid v3 scene input.
/// Every node is checked, visible or not, before any tile is written.
/// `tests/e2e/cpu-composite.js` `sceneGap` mirrors this.
pub fn v3_gap(scene: &RenderScene) -> Option<&'static str> {
    let integer_effect = |e: &crate::REffect| {
        matches!(
            e.kind,
            crate::REffectKind::ColorAdjust { .. }
                | crate::REffectKind::GaussianBlur { .. }
                | crate::REffectKind::Invert
                | crate::REffectKind::Curves { .. }
                | crate::REffectKind::Levels { .. }
                | crate::REffectKind::HueSaturation { .. }
        )
    };
    for n in &scene.nodes {
        let f = &n.features;
        if !f.effects.iter().all(integer_effect) {
            return Some("effects");
        }
        match &n.content {
            // Version 3 text draws from its bake (`TextBake`); layout runs are
            // never a v3 input.
            RContent::Text(_) => return Some("text"),
            RContent::Adjustment(e) if !integer_effect(e) => {
                return Some("adjustment");
            }
            _ => {}
        }
    }
    None
}
// The renderer keeps raw matrices in its hot loops; the math is `Affine2`'s.
pub(crate) fn multiply(a: [f64; 6], b: [f64; 6]) -> [f64; 6] {
    alto_core::Affine2(a).concat(alto_core::Affine2(b)).0
}

/// A document-to-canvas matrix while rendering. Version 3 never performs
/// arithmetic on the stored f64 values after `FixedAffine::from_affine`.
#[derive(Clone, Copy)]
pub(crate) enum WorldTransform {
    Legacy([f64; 6]),
    V3(alto_core::FixedAffine),
}

impl WorldTransform {
    pub(crate) fn root(version: u32) -> Self {
        if version == 3 {
            Self::V3(alto_core::FixedAffine::IDENTITY)
        } else {
            Self::Legacy([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
        }
    }

    pub(crate) fn concat(self, local: [f64; 6]) -> Result<Self> {
        match self {
            Self::Legacy(parent) => Ok(Self::Legacy(multiply(parent, local))),
            Self::V3(parent) => {
                let local = alto_core::FixedAffine::from_affine(alto_core::Affine2(local))
                    .ok_or_else(|| {
                        RenderError::GpuUnsupported("v3 transform out of range".into())
                    })?;
                let world = parent.concat(local).ok_or_else(|| {
                    RenderError::GpuUnsupported("v3 composed transform out of range".into())
                })?;
                Ok(Self::V3(world))
            }
        }
    }

    pub(crate) fn legacy(self) -> Result<[f64; 6]> {
        match self {
            Self::Legacy(world) => Ok(world),
            Self::V3(_) => Err(RenderError::Internal(
                "v3 reached a legacy transform path".into(),
            )),
        }
    }

    pub(crate) fn fixed(self) -> Result<alto_core::FixedAffine> {
        match self {
            Self::V3(world) => Ok(world),
            Self::Legacy(_) => Err(RenderError::Internal(
                "legacy render reached a v3 transform path".into(),
            )),
        }
    }
}

pub(crate) fn v3_inverse(
    world: alto_core::FixedAffine,
) -> Result<Option<crate::sample::IntegerInverse>> {
    crate::sample::fixed_inverse(&world)
        .ok_or_else(|| RenderError::GpuUnsupported("v3 draw transform has no Q16 inverse".into()))
}
pub(crate) fn inverse(m: [f64; 6]) -> Option<[f64; 6]> {
    alto_core::Affine2(m).inverse().map(|m| m.0)
}
pub(crate) fn point(m: [f64; 6], p: [f64; 2]) -> [f64; 2] {
    alto_core::Affine2(m).apply(p)
}
/// The world transform and the key box of every source tile a stroke has
/// touched, for the version 3 integer invalidation (`invalidate_reach`).
pub(super) fn stroke_reach(
    stroke: &GpuStroke,
) -> Option<(Option<alto_core::FixedAffine>, [i32; 4])> {
    let keys = stroke.tiles.keys().fold(None, |b: Option<[i32; 4]>, k| {
        Some(match b {
            None => [k.tx, k.ty, k.tx, k.ty],
            Some([x0, y0, x1, y1]) => [x0.min(k.tx), y0.min(k.ty), x1.max(k.tx), y1.max(k.ty)],
        })
    })?;
    Some((stroke.fixed_world, keys))
}
/// The canvas end `[x1, y1]` of the version 3 resolve tests (they start at 0).
pub(super) fn canvas_end(scene: &RenderScene) -> [i32; 2] {
    [scene.canvas.x1, scene.canvas.y1]
}
/// Bounding box of `[x0, y0, x1, y1]` mapped through `m`, as (min, max).
pub(super) fn bounds_through(m: [f64; 6], r: [f64; 4]) -> ([f64; 2], [f64; 2]) {
    let corners = [[r[0], r[1]], [r[2], r[1]], [r[0], r[3]], [r[2], r[3]]].map(|p| point(m, p));
    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    for p in corners {
        for axis in 0..2 {
            min[axis] = min[axis].min(p[axis]);
            max[axis] = max[axis].max(p[axis]);
        }
    }
    (min, max)
}
