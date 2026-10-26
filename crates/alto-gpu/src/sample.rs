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
//! * Matrix coefficients and translation are Q16 (`value * 65536`), rounded half
//!   away from zero, then clamped to `i32::MIN + 1 ..= i32::MAX` so a negation in
//!   the shader cannot overflow.
//! * A pixel center is Q8 and exact, because the origin and the level scale are
//!   integers: `p = origin * 256 + (i * 256 + 128) * scale`.
//! * `Q16 * Q8` is Q24. Translation is shifted from Q16 to Q24 (`<< 8`). The sum
//!   is rounded half away from zero into Q8 (add or subtract `2^15`, then an
//!   arithmetic `>> 16`).
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

/// Same thresholds as `fetch_legacy` in `composite.wgsl`.
pub fn axis_unit(inv: [f32; 6]) -> bool {
    let axis = inv[1].abs() < 0.0001 && inv[2].abs() < 0.0001;
    let unit = (inv[0].abs() - 1.0).abs() < 0.0001 && (inv[3].abs() - 1.0).abs() < 0.0001;
    axis && unit
}

/// Q16, round half away from zero.
pub fn quantize_q16(v: f64) -> i32 {
    if !v.is_finite() {
        return 0;
    }
    let scaled = v * 65536.0;
    let rounded = if scaled >= 0.0 {
        (scaled + 0.5).floor()
    } else {
        (scaled - 0.5).ceil()
    };
    let max = i32::MAX as f64;
    let min = (i32::MIN + 1) as f64;
    if rounded >= max {
        i32::MAX
    } else if rounded <= min {
        i32::MIN + 1
    } else {
        rounded as i32
    }
}

pub struct PackedInverse {
    pub inv0: [f32; 4],
    pub translation: [f32; 2],
    pub sample_mode: u32,
}

/// Float inverse for nearest and for render version 1. Q16 bitcast for a
/// version 2 sample that is not an axis-aligned unit scale.
pub fn pack_inverse(inv: [f64; 6], render_version: u32) -> PackedInverse {
    let f32_inv = inv.map(|v| v as f32);
    if render_version >= RENDER_VERSION && !axis_unit(f32_inv) {
        let q = inv.map(quantize_q16);
        PackedInverse {
            inv0: [
                f32::from_bits(q[0].cast_unsigned()),
                f32::from_bits(q[1].cast_unsigned()),
                f32::from_bits(q[2].cast_unsigned()),
                f32::from_bits(q[3].cast_unsigned()),
            ],
            translation: [
                f32::from_bits(q[4].cast_unsigned()),
                f32::from_bits(q[5].cast_unsigned()),
            ],
            sample_mode: SAMPLE_INTEGER,
        }
    } else {
        PackedInverse {
            inv0: [f32_inv[0], f32_inv[1], f32_inv[2], f32_inv[3]],
            translation: [f32_inv[4], f32_inv[5]],
            sample_mode: SAMPLE_F32,
        }
    }
}

/// Q8 source coordinate. `px` and `py` are Q8 pixel centers.
#[cfg(test)]
pub fn source_q8(inv_q16: [i32; 6], px: i32, py: i32) -> [i32; 2] {
    let x = i64::from(inv_q16[0]) * i64::from(px)
        + i64::from(inv_q16[2]) * i64::from(py)
        + (i64::from(inv_q16[4]) << 8);
    let y = i64::from(inv_q16[1]) * i64::from(px)
        + i64::from(inv_q16[3]) * i64::from(py)
        + (i64::from(inv_q16[5]) << 8);
    [round_shr16(x) as i32, round_shr16(y) as i32]
}

#[cfg(test)]
fn round_shr16(v: i64) -> i64 {
    let half = 1i64 << 15;
    let adjusted = if v >= 0 { v + half } else { v - half };
    adjusted >> 16
}

/// Floor of a Q8 coordinate and its weight in `0..=255`.
#[cfg(test)]
pub fn floor_frac_q8(q: i32) -> (i32, i32) {
    let floor = q >> 8;
    (floor, q - (floor << 8))
}

/// Q8 pixel center. `origin_px` is the tile origin in document pixels.
#[cfg(test)]
pub fn pixel_q8(origin_px: i32, index: i32, scale: i32) -> i32 {
    origin_px * 256 + (index * 256 + 128) * scale
}

/// One channel of the 4-tap. `fx` and `fy` are `0..=255`.
#[cfg(test)]
pub fn bilinear_byte(a: u8, b: u8, d: u8, e: u8, fx: i32, fy: i32) -> u8 {
    let ab = i32::from(a) * (256 - fx) + i32::from(b) * fx;
    let de = i32::from(d) * (256 - fx) + i32::from(e) * fx;
    let acc = ab * (256 - fy) + de * fy;
    ((acc + 32768) >> 16) as u8
}
