//! Fixed-point sampling for render version 2.
//!
//! Render version 1 (`RENDER_VERSION_F32`) keeps the f32 path in `composite.wgsl`:
//! source coordinates are an f32 matrix multiply, and bilinear is `mix`
//! (`a * (1 - t) + b * t`). GPU compilers may contract that and the coordinate
//! multiply, so version 1 bilinear and rotation are not bit-exact across devices.
//! Documents saved before version 2 omit `render_version` and stay on this path.
//!
//! Render version 2 packs the f64 inverse as Q16 and filters with integer weights.
//! The shader bitcasts those bits back; it does not evaluate the coordinate in f32.
//!
//! * Matrix coefficients are Q16 (`value * 65536`), rounded half away from zero.
//!   They must fit in `i32::MIN + 1 ..= i32::MAX`: the shader negates a coefficient,
//!   and `i32::MIN` has no positive counterpart.
//! * Translation is Q16 in a 64-bit two's complement (`Wide` in the shader), with
//!   the same rounding. A translation that does not fit in `i32` is still exact.
//!   A value that is not an exact integer in `f64` (`|value * 65536| >= 2^53`), or
//!   a coefficient outside the `i32` range, does not take this path: the draw falls
//!   back to the f32 fetch and a warning is logged. Nothing is clamped.
//! * A pixel center is Q8 and exact, because the origin and the level scale are
//!   integers: `p = origin * 256 + (i * 256 + 128) * scale`. When that sum does
//!   not fit in `i32` (a tile origin past ±2^23 px, or a large level scale), the
//!   sample is transparent. The center is not wrapped.
//! * `Q16 * Q8` is Q24. Translation is shifted from Q16 to Q24 (`<< 8`). The sum
//!   is rounded half away from zero into Q8 (add or subtract `2^15`, then an
//!   arithmetic `>> 16`). When the rounded Q8 coordinate does not fit in `i32`
//!   (source position past ±2^23 px), the sample is transparent. The low 32 bits
//!   are not used. A 0.01× layer moved 84000px inverts to about −8.4M px, which
//!   is past that limit.
//! * The low 8 bits of that Q8 position are the bilinear weights `fx, fy` in
//!   `0..=255`. Each channel is a byte:
//!   `ab = a * (256 - fx) + b * fx`, likewise `de`, then
//!   `byte = (ab * (256 - fy) + de * fy + 32768) >> 16`.
//!   The added half is round half up. Every term is non-negative, so that is also
//!   round half away from zero.
//! * The byte is handed to the existing f32 blend as `byte / 255`.
//!
//! Axis-aligned unit scales stay on the f32 nearest fetch in both versions, so
//! those pixels do not move. Only bilinear and rotated samples change, and only
//! for render version 2.
//!
//! The integer arithmetic is a few dozen extra ALU ops per filtered pixel on top
//! of the four `textureLoad`s the float path already issued. Nearest layers do
//! not take it.

use alto_core::RENDER_VERSION;

/// `Command.info[3]`: the inverse in the command is f32, and the shader uses
/// the legacy fetch (nearest, or float bilinear for render version 1).
pub const SAMPLE_F32: u32 = 0;
/// `Command.info[3]`: the inverse is Q16 bitcast into the f32 slots, and the
/// shader runs the integer 4-tap.
pub const SAMPLE_INTEGER: u32 = 1;
/// `Command.info[3]`, render version 3 only: the inverse is Q16 as for
/// `SAMPLE_INTEGER`, and the texel is the nearest one, the floor of the Q8
/// position (`axis_unit_q16` draws). Version 3 has no f32 fetch.
pub const SAMPLE_NEAREST: u32 = 2;

/// Render version 3: the integer counterpart of `axis_unit` on the Q16
/// coefficients `m` (`m00, m10, m01, m11`). The f32 test `|v| < 0.0001` is
/// `|v| * 65536 < 6.5536`, so a rounded Q16 coefficient within 6 of 0 (off the
/// axes) or of ±65536 (on them) is axis-aligned and unit: the draw is nearest.
pub fn axis_unit_q16(m: [i32; 4]) -> bool {
    let near = |v: i32, to: i32| (i64::from(v).abs() - i64::from(to)).abs() <= 6;
    near(m[1], 0) && near(m[2], 0) && near(m[0], 65536) && near(m[3], 65536)
}

/// Same thresholds as `fetch_legacy` in `composite.wgsl`.
pub fn axis_unit(inv: [f32; 6]) -> bool {
    let axis = inv[1].abs() < 0.0001 && inv[2].abs() < 0.0001;
    let unit = (inv[0].abs() - 1.0).abs() < 0.0001 && (inv[3].abs() - 1.0).abs() < 0.0001;
    axis && unit
}

/// Integers with magnitude below this are exact in `f64`.
const F64_EXACT_INT: f64 = (1u64 << 53) as f64;

/// Round half away from zero, exactly: `f64::round` decides on the value itself.
/// Render version 3 (`integer_inverse`).
fn round_half_away(scaled: f64) -> f64 {
    scaled.round()
}

/// Render version 2's rounding, kept so its quantization and hashes never move:
/// `(scaled + 0.5).floor()` rounds the sum first, so a value just below a tie
/// (`0.5 - 2^-54`) goes up. Only `integer_inverse_v2` uses it.
fn round_half_away_v2(scaled: f64) -> f64 {
    if scaled >= 0.0 {
        (scaled + 0.5).floor()
    } else {
        (scaled - 0.5).ceil()
    }
}

/// Rounded Q16 with `round`, or `None` when `v` is not finite or `v * 65536` is
/// not an exact `f64` integer. `v * 65536` is exact (a power of two).
fn quantize_q16(v: f64, round: fn(f64) -> f64) -> Option<f64> {
    if !v.is_finite() {
        return None;
    }
    let scaled = v * 65536.0;
    if !scaled.is_finite() || scaled.abs() >= F64_EXACT_INT {
        return None;
    }
    Some(round(scaled))
}

fn coeff(v: f64, round: fn(f64) -> f64) -> Option<i32> {
    let rounded = quantize_q16(v, round)?;
    if rounded > i32::MAX as f64 || rounded < (i32::MIN + 1) as f64 {
        None
    } else {
        Some(rounded as i32)
    }
}

/// Q16 translation as `i64`. `None` when the rounded value is not exact in `f64`.
/// `|q| < 2^53` implies the Q24 shift (`<< 8`) fits in `i64`.
pub fn quantize_translation(v: f64) -> Option<i64> {
    Some(quantize_q16(v, round_half_away)? as i64)
}

/// Q16 coefficient, rounded exactly half away from zero (render version 3).
/// `None` when it does not fit in `i32::MIN + 1 ..= i32::MAX`.
pub fn quantize_coeff(v: f64) -> Option<i32> {
    coeff(v, round_half_away)
}

/// Coefficients in `m` (`m00, m10, m01, m11`) and translation, all Q16.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntegerInverse {
    pub m: [i32; 4],
    pub tx: i64,
    pub ty: i64,
}

/// The Q16 inverse of render version 3, every value rounded exactly half away
/// from zero.
pub fn integer_inverse(inv: [f64; 6]) -> Option<IntegerInverse> {
    integer_inverse_with(inv, round_half_away)
}

/// The Q16 inverse of render version 2 (`pack_inverse`), with its frozen
/// rounding (`round_half_away_v2`).
pub fn integer_inverse_v2(inv: [f64; 6]) -> Option<IntegerInverse> {
    integer_inverse_with(inv, round_half_away_v2)
}

fn integer_inverse_with(inv: [f64; 6], round: fn(f64) -> f64) -> Option<IntegerInverse> {
    let t = |v| Some(quantize_q16(v, round)? as i64);
    Some(IntegerInverse {
        m: [
            coeff(inv[0], round)?,
            coeff(inv[1], round)?,
            coeff(inv[2], round)?,
            coeff(inv[3], round)?,
        ],
        tx: t(inv[4])?,
        ty: t(inv[5])?,
    })
}

/// Little-endian `i64` bits: `tx` low, `tx` high, `ty` low, `ty` high.
pub fn pack_wide(tx: i64, ty: i64) -> [u32; 4] {
    let tx = tx as u64;
    let ty = ty as u64;
    [tx as u32, (tx >> 32) as u32, ty as u32, (ty >> 32) as u32]
}

pub struct PackedInverse {
    pub inv0: [f32; 4],
    /// f32 translation on the legacy path. Zero when `sample_mode` is integer.
    pub translation: [f32; 2],
    /// `i64` Q16 translation bits. Zero on the f32 path.
    pub trans: [u32; 4],
    pub sample_mode: u32,
}

/// Float inverse for nearest and for render version 1. Q16 for a version 2 sample
/// that is not an axis-aligned unit scale and that fits the integer format.
/// A sample that does not fit uses the f32 path; it is never clamped.
pub fn pack_inverse(inv: [f64; 6], render_version: u32) -> PackedInverse {
    let f32_inv = inv.map(|v| v as f32);
    let float = PackedInverse {
        inv0: [f32_inv[0], f32_inv[1], f32_inv[2], f32_inv[3]],
        translation: [f32_inv[4], f32_inv[5]],
        trans: [0; 4],
        sample_mode: SAMPLE_F32,
    };
    if render_version < RENDER_VERSION || axis_unit(f32_inv) {
        return float;
    }
    let Some(q) = integer_inverse_v2(inv) else {
        log::warn!(
            "compositor inverse does not fit the integer sampler (translation {}, {}); using the f32 path",
            inv[4],
            inv[5]
        );
        return float;
    };
    PackedInverse {
        inv0: [
            f32::from_bits(q.m[0].cast_unsigned()),
            f32::from_bits(q.m[1].cast_unsigned()),
            f32::from_bits(q.m[2].cast_unsigned()),
            f32::from_bits(q.m[3].cast_unsigned()),
        ],
        translation: [0.0, 0.0],
        trans: pack_wide(q.tx, q.ty),
        sample_mode: SAMPLE_INTEGER,
    }
}

/// Q8 source coordinate. `px` and `py` are Q8 pixel centers.
/// `None` when the i64 sum overflows or the rounded coordinate does not fit in `i32`.
pub fn source_q8(inv: &IntegerInverse, px: i32, py: i32) -> Option<[i32; 2]> {
    Some([
        source_axis(inv.m[0], px, inv.m[2], py, inv.tx)?,
        source_axis(inv.m[1], px, inv.m[3], py, inv.ty)?,
    ])
}

fn source_axis(m0: i32, px: i32, m1: i32, py: i32, translation: i64) -> Option<i32> {
    let a = i64::from(m0).checked_mul(i64::from(px))?;
    let b = i64::from(m1).checked_mul(i64::from(py))?;
    let t = translation.checked_mul(256)?;
    let sum = a.checked_add(b)?.checked_add(t)?;
    i32::try_from(round_shr16(sum)?).ok()
}

/// Round half away from zero, then arithmetic `>> 16`. `None` when the half-step overflows `i64`.
fn round_shr16(v: i64) -> Option<i64> {
    let half = 1i64 << 15;
    let adjusted = if v >= 0 {
        v.checked_add(half)?
    } else {
        v.checked_sub(half)?
    };
    Some(adjusted >> 16)
}

/// Floor of a Q8 coordinate and its weight in `0..=255`.
#[cfg(test)]
pub fn floor_frac_q8(q: i32) -> (i32, i32) {
    let floor = q >> 8;
    (floor, q - (floor << 8))
}

/// Q8 pixel center. `origin_px` is the tile origin in document pixels.
/// `None` when the center does not fit in `i32`.
pub fn pixel_q8(origin_px: i32, index: i32, scale: i32) -> Option<i32> {
    let term = i64::from(index).checked_mul(256)?.checked_add(128)?;
    let q = i64::from(origin_px)
        .checked_mul(256)?
        .checked_add(term.checked_mul(i64::from(scale))?)?;
    i32::try_from(q).ok()
}

/// One channel of the 4-tap. `fx` and `fy` are `0..=255`.
#[cfg(test)]
pub fn bilinear_byte(a: u8, b: u8, d: u8, e: u8, fx: i32, fy: i32) -> u8 {
    let ab = i32::from(a) * (256 - fx) + i32::from(b) * fx;
    let de = i32::from(d) * (256 - fx) + i32::from(e) * fx;
    let acc = ab * (256 - fy) + de * fy;
    ((acc + 32768) >> 16) as u8
}

#[cfg(test)]
mod tests {
    use super::source_axis;

    #[test]
    fn cancelling_q24_sum_is_exact_and_out_of_range_is_transparent() {
        // m0*px is past 2^53. f64 rounds it, so the cancelled sum is 32768
        // (Q8 1). The i64 sum is 32735, which rounds to Q8 0.
        let q = source_axis(2_147_483_647, 2_000_000_033, 0, 0, -16_777_216_269_011_436);
        assert_eq!(q, Some(0));
        let f64_sum = 2_147_483_647f64 * 2_000_000_033f64 + -16_777_216_269_011_436f64 * 256.0;
        assert_eq!(f64_sum, 32_768.0);
        // Rounded Q8 is 2^31, outside i32.
        assert_eq!(source_axis(0, 0, 0, 0, 1_i64 << 39), None);
        // translation * 256 overflows i64.
        assert_eq!(source_axis(0, 0, 0, 0, i64::MAX), None);
    }
}
