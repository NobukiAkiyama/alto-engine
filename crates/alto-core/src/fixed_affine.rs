//! Version 3's drawing transform. Stored `Affine2` values are only read as bits;
//! all composition and inversion inputs are integers.

use crate::Affine2;

const LINEAR_SCALE: i128 = 1 << 32;
const LINEAR_LIMIT: i64 = 1 << 46;
const TRANSLATION_LIMIT: i64 = 1 << 47;

/// Exact half-away quantization of an IEEE-754 binary64 bit pattern to Q`f`.
/// This function performs no floating-point arithmetic.
pub fn q_from_f64_bits(bits: u64, f: u32) -> Option<i64> {
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    if exponent == 0x7ff {
        return None;
    }
    let fraction = bits & ((1u64 << 52) - 1);
    let (mantissa, power) = if exponent == 0 {
        (u128::from(fraction), -1074)
    } else {
        (u128::from(fraction | (1u64 << 52)), exponent - 1075)
    };
    let shift = power.checked_add(i32::try_from(f).ok()?)?;
    let magnitude = if shift >= 0 {
        let shift: u32 = shift.try_into().ok()?;
        if mantissa != 0 && shift >= mantissa.leading_zeros() {
            return None;
        }
        mantissa.checked_shl(shift)?
    } else {
        let right = (-shift) as u32;
        if right >= 54 {
            0
        } else {
            (mantissa + (1u128 << (right - 1))) >> right
        }
    };
    let signed = i128::try_from(magnitude).ok()?;
    let signed = if bits >> 63 == 0 { signed } else { -signed };
    i64::try_from(signed).ok()
}

/// Half-away division with a positive denominator.
pub(crate) fn round_div(n: i128, d: i128) -> Option<i128> {
    if d <= 0 {
        return None;
    }
    let magnitude = n.unsigned_abs();
    let divisor = d as u128;
    let quotient = magnitude / divisor;
    let remainder = magnitude % divisor;
    let rounded = quotient.checked_add(u128::from(remainder >= divisor.div_ceil(2)))?;
    let signed = i128::try_from(rounded).ok()?;
    Some(if n < 0 { -signed } else { signed })
}

/// Half-away division by a signed nonzero denominator.
pub(crate) fn round_div_signed(n: i128, d: i128) -> Option<i128> {
    if d == 0 {
        return None;
    }
    let magnitude = n.unsigned_abs();
    let divisor = d.unsigned_abs();
    let quotient = magnitude / divisor;
    let remainder = magnitude % divisor;
    let rounded = quotient.checked_add(u128::from(remainder >= divisor.div_ceil(2)))?;
    let signed = i128::try_from(rounded).ok()?;
    Some(if (n < 0) != (d < 0) { -signed } else { signed })
}

/// The v3 world matrix. Linear terms are Q32; translations are Q16.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedAffine {
    pub m: [i64; 4],
    pub t: [i64; 2],
}

impl Default for FixedAffine {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl FixedAffine {
    pub const IDENTITY: Self = Self {
        m: [1 << 32, 0, 0, 1 << 32],
        t: [0, 0],
    };

    /// The only v3 read of a stored floating-point matrix: `to_bits` only.
    pub fn from_affine(value: Affine2) -> Option<Self> {
        let v = value.0;
        let result = Self {
            m: [
                q_from_f64_bits(v[0].to_bits(), 32)?,
                q_from_f64_bits(v[1].to_bits(), 32)?,
                q_from_f64_bits(v[2].to_bits(), 32)?,
                q_from_f64_bits(v[3].to_bits(), 32)?,
            ],
            t: [
                q_from_f64_bits(v[4].to_bits(), 16)?,
                q_from_f64_bits(v[5].to_bits(), 16)?,
            ],
        };
        result.in_range().then_some(result)
    }

    pub fn in_range(self) -> bool {
        self.m
            .iter()
            .all(|v| v.unsigned_abs() < LINEAR_LIMIT as u64)
            && self
                .t
                .iter()
                .all(|v| v.unsigned_abs() < TRANSLATION_LIMIT as u64)
    }

    fn term(a: i64, b: i64, c: i64, d: i64, t: i128) -> Option<i64> {
        let n = i128::from(a)
            .checked_mul(i128::from(b))?
            .checked_add(i128::from(c).checked_mul(i128::from(d))?)?
            .checked_add(t)?;
        i64::try_from(round_div(n, LINEAR_SCALE)?).ok()
    }

    pub fn concat(self, local: Self) -> Option<Self> {
        if !self.in_range() || !local.in_range() {
            return None;
        }
        let p = self.m;
        let n = local.m;
        let result = Self {
            m: [
                Self::term(p[0], n[0], p[2], n[1], 0)?,
                Self::term(p[1], n[0], p[3], n[1], 0)?,
                Self::term(p[0], n[2], p[2], n[3], 0)?,
                Self::term(p[1], n[2], p[3], n[3], 0)?,
            ],
            t: [
                Self::term(
                    p[0],
                    local.t[0],
                    p[2],
                    local.t[1],
                    i128::from(self.t[0]) << 32,
                )?,
                Self::term(
                    p[1],
                    local.t[0],
                    p[3],
                    local.t[1],
                    i128::from(self.t[1]) << 32,
                )?,
            ],
        };
        result.in_range().then_some(result)
    }

    pub fn determinant(self) -> Option<i128> {
        i128::from(self.m[0])
            .checked_mul(i128::from(self.m[3]))?
            .checked_sub(i128::from(self.m[1]).checked_mul(i128::from(self.m[2]))?)
    }

    /// Exact rational inverse, rounded once into the sampler's Q16 format.
    /// `None` is overflow; `Some(None)` is a singular matrix.
    pub fn inverse_q16(self) -> Option<Option<[i64; 6]>> {
        if !self.in_range() {
            return None;
        }
        let determinant = self.determinant()?;
        if determinant == 0 {
            return Some(None);
        }
        let [a, b, c, d] = self.m.map(i128::from);
        let [tx, ty] = self.t.map(i128::from);
        let q = |v| i64::try_from(round_div_signed(v, determinant)?).ok();
        Some(Some([
            q(d.checked_mul(1 << 48)?)?,
            q((-b).checked_mul(1 << 48)?)?,
            q((-c).checked_mul(1 << 48)?)?,
            q(a.checked_mul(1 << 48)?)?,
            q(c.checked_mul(ty)?
                .checked_sub(d.checked_mul(tx)?)?
                .checked_mul(1 << 32)?)?,
            q(b.checked_mul(tx)?
                .checked_sub(a.checked_mul(ty)?)?
                .checked_mul(1 << 32)?)?,
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ties_and_neighbors_are_exact() {
        let tie = 2f64.powi(-17);
        let below = tie - 2f64.powi(-70);
        assert_eq!(q_from_f64_bits(below.to_bits(), 16), Some(0));
        assert_eq!(q_from_f64_bits(tie.to_bits(), 16), Some(1));
        assert_eq!(q_from_f64_bits((-tie).to_bits(), 16), Some(-1));
        assert_eq!(q_from_f64_bits((-below).to_bits(), 16), Some(0));
        assert_eq!(q_from_f64_bits(f64::MIN_POSITIVE.to_bits(), 32), Some(0));
        assert_eq!(q_from_f64_bits(f64::NAN.to_bits(), 32), None);
        assert_eq!(q_from_f64_bits(f64::INFINITY.to_bits(), 16), None);
        let q32_tie = 2f64.powi(-33);
        let q32_below = f64::from_bits(q32_tie.to_bits() - 1);
        let q32_above = f64::from_bits(q32_tie.to_bits() + 1);
        assert_eq!(q_from_f64_bits(q32_below.to_bits(), 32), Some(0));
        assert_eq!(q_from_f64_bits(q32_tie.to_bits(), 32), Some(1));
        assert_eq!(q_from_f64_bits(q32_above.to_bits(), 32), Some(1));
        assert_eq!(q_from_f64_bits((-q32_tie).to_bits(), 32), Some(-1));
        assert_eq!(
            FixedAffine::from_affine(Affine2([16384.0, 0.0, 0.0, 1.0, 0.0, 0.0])),
            None
        );
        assert_eq!(
            FixedAffine::from_affine(Affine2([1.0, 0.0, 0.0, 1.0, 2_147_483_648.0, 0.0])),
            None
        );
    }

    #[test]
    fn translation_rounds_the_whole_coordinate() {
        let parent = FixedAffine {
            m: [1 << 32, 0, -(1 << 31), 1 << 32],
            t: [1, 0],
        };
        let child = FixedAffine {
            t: [0, 1],
            ..FixedAffine::IDENTITY
        };
        assert_eq!(parent.concat(child).unwrap().t[0], 1);
    }

    #[test]
    fn inverse_is_one_rational_rounding() {
        assert_eq!(
            FixedAffine::IDENTITY.inverse_q16(),
            Some(Some([1 << 16, 0, 0, 1 << 16, 0, 0]))
        );
        let rotate = FixedAffine {
            m: [0, 1 << 32, -(1 << 32), 0],
            t: [3 << 16, -2 << 16],
        };
        assert_eq!(
            rotate.inverse_q16(),
            Some(Some([0, -(1 << 16), 1 << 16, 0, 2 << 16, 3 << 16]))
        );
        let singular = FixedAffine {
            m: [1 << 32, 2 << 32, 2 << 32, 4 << 32],
            t: [0, 0],
        };
        assert_eq!(singular.inverse_q16(), Some(None));
    }

    #[test]
    fn q32_entry_precedes_q16_inverse_rounding() {
        // The old Q16 path leaves this just below a tie. Version 3 rounds each
        // stored local matrix to Q32 first, making the shear an exact Q16 tie.
        let below = 2f64.powi(-17) - 2f64.powi(-70);
        let world = FixedAffine::from_affine(Affine2([1.0, 0.0, -below, 1.0, 0.0, 0.0]))
            .expect("Q32 shear");
        assert_eq!(world.m[2], -(1 << 15));
        assert_eq!(
            world.inverse_q16(),
            Some(Some([1 << 16, 0, 1, 1 << 16, 0, 0]))
        );
    }

    /// The memo's fixed vectors (doc/design/2026-10-28-v3-integer-text.md
    /// "浮動小数のビットの整数分解"): bit patterns, never re-parsed decimals.
    #[test]
    fn q_from_float_bits_rounds_half_away() {
        let q16 = |hex: u64| q_from_f64_bits(hex, 16);
        // Just below, at and just above Q16's 0.5 (2^-17), and negated.
        for (bits, want) in [
            (0x3edf_ffff_ffff_ffff, 0),
            (0x3ee0_0000_0000_0000, 1),
            (0x3ee0_0000_0000_0001, 1),
            (0xbedf_ffff_ffff_ffff, 0),
            (0xbee0_0000_0000_0000, -1),
            (0xbee0_0000_0000_0001, -1),
            // Around Q16's 1.5 (1.5 * 2^-16).
            (0x3ef7_ffff_ffff_ffff, 1),
            (0x3ef8_0000_0000_0000, 2),
            (0x3ef8_0000_0000_0001, 2),
            (0xbef7_ffff_ffff_ffff, -1),
            (0xbef8_0000_0000_0000, -2),
            (0xbef8_0000_0000_0001, -2),
        ] {
            assert_eq!(q16(bits), Some(want), "{bits:016x}");
        }
        assert_eq!(f64::from_bits(0x3edf_ffff_ffff_ffff), 7.629394531249999e-06);
        // Subnormals, signed zeros, infinities and NaN.
        assert_eq!(q16(1), Some(0));
        assert_eq!(q16(0.0f64.to_bits()), Some(0));
        assert_eq!(q16((-0.0f64).to_bits()), Some(0));
        assert_eq!(q16(f64::INFINITY.to_bits()), None);
        assert_eq!(q16(f64::NEG_INFINITY.to_bits()), None);
        assert_eq!(q16(f64::NAN.to_bits()), None);
        // The memo's k = 9 / k = 10 boundary. The memo's text_q16_from_bits
        // refuses k = 10 (2^46); this decoder returns it, and v3 text never
        // sees it: FixedAffine refuses translations of 2^47 Q16 and more, and
        // linear terms of 2^46 Q32 and more.
        assert_eq!(q16(2f64.powi(45).to_bits()), Some(1 << 61));
        assert_eq!(q16(2f64.powi(46).to_bits()), Some(1 << 62));
        assert_eq!(q16(2f64.powi(47).to_bits()), None);
    }
}
