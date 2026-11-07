//! Rounding, f16 and fixed-affine unit tests.
use super::*;

#[test]
fn f16_storage_round_trip_matches_known_bits() {
    assert_eq!(f32_to_f16_bits(0.0), 0);
    assert_eq!(f32_to_f16_bits(1.0), 0x3c00);
    assert_eq!(f32_to_f16_bits(0.5), 0x3800);
    assert_eq!(f16_to_f32(1).to_bits(), 0x3380_0000);
    assert_eq!(f16_round(1.0), 1.0);
}

#[test]
fn color_adjust_q14_known_values() {
    use crate::renderer::color_adjust_q14;
    assert_eq!(color_adjust_q14(0.0), 0);
    assert_eq!(color_adjust_q14(1.0), Q14_ONE);
    assert_eq!(color_adjust_q14(0.5), Q14_HALF);
    assert_eq!(color_adjust_q14(-1.0), -Q14_ONE);
    assert_eq!(color_adjust_q14(4.0), Q14_ONE * 4);
    assert_eq!(Q14_LUMA[0] + Q14_LUMA[1] + Q14_LUMA[2], Q14_ONE);
    let anchor = [251, 2, 251, 128];
    let v2 = color_adjust_texel(anchor, -0.35f64 as f32, 0.5f64 as f32, 0.0);
    let v3 = color_adjust_texel_v3(anchor, -0.35f64 as f32, 0.5f64 as f32, 0.0);
    assert_eq!(v2[3], 128);
    assert_eq!(v3[3], v2[3]);
    assert_ne!(v2, v3);
    assert_eq!(
        color_adjust_texel_v3([40, 80, 120, 0], 0.45, 1.7, 1.6),
        [0, 0, 0, 0]
    );
}

/// `v * 16384` rounded half away from zero by an independent formulation: the
/// f64 product is exact for an f32 `v`, and `f64::round` rounds it exactly.
pub(super) fn q14_round_f64(v: f32) -> i32 {
    (f64::from(v) * 16384.0).round() as i32
}

/// Item 12 of the float purge: the host's opacity and color-adjust Q14
/// quantizers round `v * 16384` exactly half away from zero, at every tie
/// `(q + 1/2) / 16384` and at the f32 values on both sides of it, over a stride
/// of every f32 in range, and at the value the old f32 `+ 0.5` got wrong.
#[test]
fn q14_quantizers_round_exactly_half_away() {
    use crate::renderer::color_adjust_q14;
    use crate::{composite_opacity_q14, q14_round};
    let near = |v: f32| {
        [
            f32::from_bits(v.to_bits() - 1),
            v,
            f32::from_bits(v.to_bits() + 1),
        ]
    };
    let mut checked = 0u64;
    let mut check = |v: f32| {
        if !(-4.0..=4.0).contains(&v) {
            return;
        }
        assert_eq!(q14_round(v), q14_round_f64(v), "{v:e} ({:#x})", v.to_bits());
        let o = v.clamp(0.0, 1.0);
        assert_eq!(composite_opacity_q14(v), q14_round_f64(o), "opacity {v:e}");
        let c = v.clamp(-1.0, 4.0);
        assert_eq!(color_adjust_q14(v), q14_round_f64(c), "color-adjust {v:e}");
        checked += 1;
    };
    for q in 0..4 * 16384 {
        // Every tie is exact in f32: (2q + 1) / 2^15 has at most 18 bits.
        let tie = (2 * q + 1) as f32 / 32768.0;
        assert_eq!(f64::from(tie) * 16384.0, f64::from(q) + 0.5);
        for v in near(tie) {
            check(v);
            check(-v);
        }
        assert_eq!(q14_round(tie), q + 1, "the tie {q}.5 goes up");
        assert_eq!(q14_round(-tie), -(q + 1), "the tie -{q}.5 goes down");
    }
    for bits in (0..=4.0f32.to_bits()).step_by(1_009) {
        check(f32::from_bits(bits));
        check(-f32::from_bits(bits));
    }
    for v in [0.0, -0.0, f32::MIN_POSITIVE, f32::from_bits(1), 1.0, 4.0] {
        check(v);
        check(-v);
    }
    assert!(checked > 2_000_000, "{checked}");
    // The largest f32 below 2^-15 is 0.5 - 2^-25 in Q14, which rounds to 0. The
    // old `(v * 16384 + 0.5).trunc()` in f32 rounded the sum up to 1.
    let below = f32::from_bits((1.0f32 / 32768.0).to_bits() - 1);
    assert_eq!(f64::from(below) * 16384.0, 0.5 - 2f64.powi(-25));
    assert_eq!((below * 16384.0 + 0.5).trunc(), 1.0);
    assert_eq!(composite_opacity_q14(below), 0);
    assert_eq!(color_adjust_q14(below), 0);
    assert_eq!(color_adjust_q14(-below), 0);
    // Out of range and not finite, as before.
    assert_eq!(composite_opacity_q14(f32::NAN), 0);
    assert_eq!(composite_opacity_q14(f32::INFINITY), 16384);
    assert_eq!(composite_opacity_q14(f32::NEG_INFINITY), 0);
    assert_eq!(composite_opacity_q14(-0.25), 0);
    assert_eq!(composite_opacity_q14(1.5), 16384);
    assert_eq!(color_adjust_q14(f32::NAN), 0);
    assert_eq!(color_adjust_q14(f32::INFINITY), 0);
    assert_eq!(color_adjust_q14(-7.0), -16384);
    assert_eq!(color_adjust_q14(9.0), 65536);
}

/// Values at and next to Q16 ties for [`q16_rounding_is_exact_half_away`] and
/// `tests/e2e/q16-reference.mjs`: 2^-17 (Q16 0.5) and the f64 on each side of
/// it, `(k + 1/2) / 65536` and its neighbors for k up to 2^30, all negated too.
pub(super) fn q16_tie_values() -> Vec<f64> {
    let near = |v: f64| {
        [
            f64::from_bits(v.to_bits() - 1),
            v,
            f64::from_bits(v.to_bits() + 1),
        ]
    };
    let mut out = vec![0.0, 1.0, 2f64.powi(-80)];
    for k in [
        0u64,
        1,
        2,
        3,
        255,
        256,
        32_767,
        65_535,
        65_536,
        1 << 20,
        (1 << 30) - 1,
        1 << 30,
    ] {
        out.extend(near((k as f64 + 0.5) / 65_536.0));
    }
    let negated: Vec<f64> = out.iter().map(|v| -v).collect();
    out.extend(negated);
    out
}

/// The 03:25 ruling: render version 3 rounds Q16 exactly half away from zero.
/// `(scaled + 0.5).floor()` rounds the sum first: at v = 2^-17 - 2^-70 the
/// scaled value is 0.5 - 2^-54, the sum rounds to 1, and the floor is 1, not
/// 0. Version 2 keeps that rounding (`integer_inverse_v2`) so its quantization
/// and hashes do not move.
#[test]
fn q16_rounding_is_exact_half_away() {
    use crate::sample::{
        integer_inverse, integer_inverse_v2, quantize_coeff, quantize_translation,
    };
    let tie = 2f64.powi(-17);
    let below = tie - 2f64.powi(-70);
    assert_eq!(
        below,
        f64::from_bits(tie.to_bits() - 1),
        "the f64 just below 2^-17"
    );
    assert_eq!(below * 65_536.0, 0.5 - 2f64.powi(-54));
    assert_eq!((below * 65_536.0 + 0.5).floor(), 1.0, "the old rounding");
    for (v, want) in [
        (tie, 1),
        (below, 0),
        (f64::from_bits(tie.to_bits() + 1), 1),
        (-tie, -1),
        (-below, 0),
        (-f64::from_bits(tie.to_bits() + 1), -1),
    ] {
        assert_eq!(quantize_coeff(v), Some(want), "{v:e}");
        assert_eq!(quantize_translation(v), Some(i64::from(want)), "{v:e}");
        let q = integer_inverse([v, v, v, v, v, v]).unwrap();
        assert_eq!(q.m, [want; 4], "{v:e}");
        assert_eq!((q.tx, q.ty), (i64::from(want), i64::from(want)), "{v:e}");
    }
    // Version 2 is unchanged: just below the tie it still rounds up.
    let v2 = |v: f64| integer_inverse_v2([v, 0.0, 0.0, v, v, 0.0]).unwrap();
    assert_eq!(v2(below).m[0], 1);
    assert_eq!(v2(-below).tx, -1);
    assert_eq!(v2(tie).m[0], 1);
    // Every value: the exact rule, from the f64 itself (no rounded sum).
    for v in q16_tie_values() {
        let scaled = v * 65_536.0;
        let (fl, frac) = (scaled.abs().floor(), scaled.abs() - scaled.abs().floor());
        let magnitude = if frac >= 0.5 { fl + 1.0 } else { fl };
        let want = if scaled < 0.0 { -magnitude } else { magnitude } as i64;
        assert_eq!(quantize_translation(v), Some(want), "{v:e}");
        assert_eq!(
            integer_inverse([1.0, 0.0, 0.0, 1.0, v, -v]).unwrap().ty,
            -want,
            "{v:e}"
        );
    }
}

/// Fixed-seed chains cover the v3 host's Q32 composition and exact Q16 inverse
/// against the independent JavaScript BigInt reference, including half ties.
#[test]
fn fixed_affine_rust_and_js_agree_on_ten_thousand_chains() {
    use std::io::Write;
    let mut seed = 0x56d3_9b2a_a93f_7251u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut input = String::from("[");
    let mut expected = String::new();
    for case in 0..10_000 {
        if case != 0 {
            input.push(',');
        }
        input.push('[');
        let depth = 1 + (next() % 8) as usize;
        let mut world = alto_core::FixedAffine::IDENTITY;
        let mut valid = true;
        for step in 0..depth {
            if step != 0 {
                input.push(',');
            }
            let near = |v: u64| (i64::try_from(v % 1001).unwrap() - 500) as f64 / 100_000.0;
            let translate = |v: u64| (i64::try_from(v % 200_001).unwrap() - 100_000) as f64 / 16.0;
            let local = [
                1.0 + near(next()),
                near(next()) / 2.0,
                if case % 127 == 0 {
                    -(2f64.powi(-17) - 2f64.powi(-70))
                } else {
                    near(next()) / 2.0
                },
                1.0 + near(next()),
                translate(next()),
                translate(next()),
            ];
            input.push_str(&format!("[{}]", local.map(|v| format!("{v:?}")).join(",")));
            let fixed = alto_core::FixedAffine::from_affine(Affine2(local));
            world = match fixed.and_then(|local| world.concat(local)) {
                Some(value) => value,
                None => {
                    valid = false;
                    world
                }
            };
        }
        input.push(']');
        let line = if valid {
            match crate::sample::fixed_inverse(&world) {
                Some(Some(q)) => format!(
                    "{},{},{},{},{},{}",
                    q.m[0], q.m[1], q.m[2], q.m[3], q.tx, q.ty
                ),
                Some(None) => "S".into(),
                None => "X".into(),
            }
        } else {
            "X".into()
        };
        expected.push_str(&line);
        expected.push('\n');
    }
    input.push(']');
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut child = std::process::Command::new("node")
        .arg(root.join("tests/e2e/fixed-affine-reference.mjs"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("node is required for the v3 transform reference");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual = String::from_utf8(output.stdout).unwrap();
    if actual != expected {
        let index = actual
            .lines()
            .zip(expected.lines())
            .position(|(a, b)| a != b)
            .unwrap_or(10_000);
        panic!(
            "v3 Rust and JS inverse differ at chain {index}: Rust {:?}, JS {:?}",
            expected.lines().nth(index),
            actual.lines().nth(index)
        );
    }
}

/// The v3 host functions after the stored-value boundary must contain only
/// integer operations. The ROI's f32-to-i32 validation occurs before the
/// fixed mask sampler; the shader's bit transport is outside these functions.
#[test]
fn v3_host_transform_functions_have_no_float_arithmetic() {
    let core = include_str!("../../../alto-core/src/fixed_affine.rs");
    let sample = include_str!("../sample.rs");
    let renderer = include_str!("../renderer/geometry.rs");
    fn body<'a>(source: &'a str, name: &str) -> &'a str {
        let start = source
            .find(&format!("fn {name}("))
            .expect("named v3 host function");
        let open = start + source[start..].find('{').unwrap();
        let mut depth = 0i32;
        let end = source[open..]
            .char_indices()
            .find_map(|(i, c)| {
                if c == '{' {
                    depth += 1;
                }
                if c == '}' {
                    depth -= 1;
                }
                (depth == 0).then_some(open + i + 1)
            })
            .unwrap();
        &source[open..end]
    }
    for (source, name) in [
        (core, "q_from_f64_bits"),
        (core, "round_div"),
        (core, "round_div_signed"),
        (core, "concat"),
        (core, "determinant"),
        (core, "inverse_q16"),
        (sample, "fixed_inverse"),
        (renderer, "plain_draw_v3"),
        (renderer, "v3_reach"),
        (renderer, "fixed_mask_sampler"),
    ] {
        let source = body(source, name);
        for banned in [
            "f32",
            "f64",
            ".round(",
            ".floor(",
            ".ceil(",
            "integer_inverse(",
            "multiply(",
        ] {
            assert!(!source.contains(banned), "{name} contains {banned}");
        }
    }
}
