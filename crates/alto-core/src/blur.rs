//! Bit-exact radius and coarsening decisions for version 3 Gaussian blur.

use crate::q_from_f64_bits;

pub fn blur_radius_q8_f64(radius: f64) -> Option<u32> {
    u32::try_from(q_from_f64_bits(radius.to_bits(), 8)?)
        .ok()
        .filter(|&v| v <= 16384)
}

pub fn blur_radius_q8_f32(radius: f32) -> Option<u32> {
    let bits = radius.to_bits();
    if bits >> 31 != 0 && bits & 0x7fff_ffff != 0 {
        return None;
    }
    let exponent = ((bits >> 23) & 0xff) as i32;
    if exponent == 0xff {
        return None;
    }
    let fraction = u64::from(bits & 0x7f_ffff);
    let (mantissa, power) = if exponent == 0 {
        (fraction, -149)
    } else {
        (fraction | (1 << 23), exponent - 150)
    };
    let shift = power + 8;
    let rounded = if shift >= 0 {
        let shift = shift as u32;
        if shift >= 64 || mantissa > (u64::MAX >> shift) {
            return None;
        }
        mantissa << shift
    } else {
        let right = (-shift) as u32;
        if right >= 25 {
            0
        } else {
            (mantissa + (1 << (right - 1))) >> right
        }
    };
    u32::try_from(rounded).ok().filter(|&v| v <= 16384)
}

/// The least integer k for which `radius <= 64 * 2^k`, from f32 bits only.
pub fn blur_coarsen_k(radius: f32) -> Option<i32> {
    let bits = radius.to_bits();
    if bits >> 31 != 0 || bits & 0x7fff_ffff == 0 {
        return None;
    }
    let exponent = ((bits >> 23) & 0xff) as i32;
    if exponent == 0xff {
        return None;
    }
    let fraction = bits & 0x7f_ffff;
    let (floor_log, exact_power) = if exponent == 0 {
        let bit = 31 - fraction.leading_zeros() as i32;
        (bit - 149, fraction.is_power_of_two())
    } else {
        (exponent - 127, fraction == 0)
    };
    Some(floor_log - 6 + i32::from(!exact_power))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_radius_q8_matches_ties() {
        assert_eq!(blur_radius_q8_f64(0.0), Some(0));
        assert_eq!(blur_radius_q8_f64(1.0 / 512.0), Some(1));
        assert_eq!(blur_radius_q8_f64(1.0 / 512.0 - 2f64.powi(-40)), Some(0));
        assert_eq!(blur_radius_q8_f64(1.0 / 512.0 + 2f64.powi(-40)), Some(1));
        assert_eq!(blur_radius_q8_f64(64.0), Some(16384));
        assert_eq!(blur_radius_q8_f32(f32::from_bits(1)), Some(0));
        assert_eq!(blur_radius_q8_f32(1.0 / 512.0), Some(1));
        assert_eq!(blur_radius_q8_f32(64.0), Some(16384));
    }

    #[test]
    fn blur_radius_q8_matches_rational() {
        // k / 512: even k are exact values, odd k are ties (rounded up).
        for k in 0..=32768u32 {
            let want = u64::from(k).div_ceil(2);
            let x = f64::from(k) / 512.0;
            assert_eq!(
                blur_radius_q8_f64(x).map(u64::from),
                Some(want),
                "f64 {k}/512"
            );
            assert_eq!(
                blur_radius_q8_f32(x as f32).map(u64::from),
                Some(want),
                "f32 {k}/512"
            );
        }
        assert_eq!(blur_radius_q8_f64(-0.0), Some(0));
        assert_eq!(blur_radius_q8_f32(-0.0), Some(0));
        assert_eq!(blur_radius_q8_f64(f64::MIN_POSITIVE), Some(0));
        assert_eq!(blur_radius_q8_f64(f64::from_bits(1)), Some(0));
        assert_eq!(blur_radius_q8_f32(f32::MIN_POSITIVE), Some(0));
        assert_eq!(blur_radius_q8_f32(f32::from_bits(1)), Some(0));
        assert_eq!(blur_radius_q8_f64(64.0), Some(16384));
        assert_eq!(blur_radius_q8_f32(64.0), Some(16384));
        // Outside the validated domain the decoders refuse rather than wrap.
        assert_eq!(blur_radius_q8_f64(65.0), None);
        assert_eq!(blur_radius_q8_f64(-1.0), None);
        assert_eq!(blur_radius_q8_f32(-1.0), None);
        assert_eq!(blur_radius_q8_f32(f32::NAN), None);
    }

    #[test]
    fn blur_coarsen_k_exact() {
        let up = |x: f32| f32::from_bits(x.to_bits() + 1);
        for (radius, k) in [
            (f32::from_bits(1), -155),
            (f32::from_bits(0x007f_ffff), -132),
            (1.0 / 512.0, -15),
            (up(1.0), -5),
            (up(2.0), -4),
            (32.001, 0),
            (64.0, 0),
            (64.001, 1),
            (128.0, 1),
            (128.001, 2),
        ] {
            assert_eq!(blur_coarsen_k(radius), Some(k), "{radius:e}");
        }
        assert_eq!(blur_coarsen_k(0.0), None);
        assert_eq!(blur_coarsen_k(-0.0), None);
        // 64 * 2^n exactly, one ULP below and one ULP above.
        for n in -15..=1 {
            let x = 64.0 * 2f32.powi(n);
            assert_eq!(blur_coarsen_k(x), Some(n), "64*2^{n}");
            assert_eq!(blur_coarsen_k(f32::from_bits(x.to_bits() - 1)), Some(n));
            assert_eq!(blur_coarsen_k(up(x)), Some(n + 1));
        }
    }

    #[test]
    fn blur_coarsen_k_respects_power_boundaries() {
        assert_eq!(blur_coarsen_k(f32::from_bits(1)), Some(-155));
        assert_eq!(blur_coarsen_k(1.0 / 512.0), Some(-15));
        for (radius, k) in [(1.0f32, -6), (2.0, -5), (64.0, 0), (128.0, 1)] {
            assert_eq!(blur_coarsen_k(radius), Some(k));
            assert_eq!(
                blur_coarsen_k(f32::from_bits(radius.to_bits() - 1)),
                Some(k)
            );
            assert_eq!(
                blur_coarsen_k(f32::from_bits(radius.to_bits() + 1)),
                Some(k + 1)
            );
        }
    }
}
