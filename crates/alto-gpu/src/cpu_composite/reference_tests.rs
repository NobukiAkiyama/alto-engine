//! Rust and JavaScript references agree, and the Q14 paths do not overflow.
use super::*;

/// Rust = JS for the Q16 rounding of both versions: `tests/e2e/q16-reference.mjs`
/// quantizes [`q16_tie_values`] with `quantizeQ16` (version 3) and
/// `quantizeQ16V2`, from the same f64 bits.
#[test]
fn q16_rust_and_js_references_agree() {
    let values = q16_tie_values();
    let input = values
        .iter()
        .map(|v| format!("\"{:016x}\"", v.to_bits()))
        .collect::<Vec<_>>()
        .join(",");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let js = std::process::Command::new("node")
        .arg(root.join("tests/e2e/q16-reference.mjs"))
        .arg(format!("[{input}]"))
        .output()
        .expect("node is required to compare the Rust and JavaScript Q16 rounding");
    assert!(
        js.status.success(),
        "JS reference failed: {}",
        String::from_utf8_lossy(&js.stderr)
    );
    let text = String::from_utf8(js.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), values.len());
    for (v, line) in values.iter().zip(lines) {
        let v3 = crate::sample::quantize_translation(*v).unwrap();
        let v2 = crate::sample::integer_inverse_v2([0.0, 0.0, 0.0, 0.0, *v, 0.0])
            .unwrap()
            .tx;
        assert_eq!(line, format!("{v3} {v2}"), "{v:e}");
    }
}

/// The saturation sum is i64. Clamping it, or the contrast result, would change
/// the mix. This walks the quantizer box and the straight-Q14 domain with
/// checked arithmetic.
#[test]
fn color_adjust_q14_mix_does_not_overflow() {
    // Clamped contrast stage from the review: brightness -0.5, contrast 4,
    // saturation 4, straight (255, 255, 0) became rgb (32767, 32767, -32768)
    // and luma 28035. That product does not fit in i32.
    let clamped = saturation_mix_sum(28_035, -32_768, 65_536).unwrap();
    assert_eq!(clamped, -3_525_459_968);
    assert!(i32::try_from(clamped).is_err());

    // The formula does not apply that clamp. The same input's contrast stage
    // is (32768, 32768, -32768) and luma is 28036. The blue product still
    // does not fit in i32, and the checked path accepts it.
    let rgb = [
        div_q_i64((16_384 - 8_192) * 65_536, 16_384).unwrap() + 8_192 - 8_192,
        div_q_i64((16_384 - 8_192) * 65_536, 16_384).unwrap() + 8_192 - 8_192,
        div_q_i64((0 - 8_192) * 65_536, 16_384).unwrap() + 8_192 - 8_192,
    ];
    assert_eq!(rgb, [32_768, 32_768, -32_768]);
    let luma = div_q_i64(rgb[0] * 3_483 + rgb[1] * 11_718 + rgb[2] * 1_183, 16_384).unwrap();
    assert_eq!(luma, 28_036);
    let unclamped = saturation_mix_sum(luma, rgb[2], 65_536).unwrap();
    assert_eq!(unclamped, -3_525_509_120);
    assert!(i32::try_from(unclamped).is_err());
    let q = [16_384u32, 16_384, 0, 16_384];
    assert!(color_adjust_exact_q14_checked(q, -8_192, 65_536, 65_536).is_some());
    let _ = color_adjust_texel_v3([255, 255, 0, 255], -0.5, 4.0, 4.0);

    // luma and the channel are not independent; this box contains every pair
    // the contrast stage can produce. Corners are the extrema of the bilinear sum.
    let span = [-40_960i64, 57_344];
    let sats = [-16_384i64, 65_536];
    let mut min_sum = i64::MAX;
    let mut max_sum = i64::MIN;
    for &luma in &span {
        for &channel in &span {
            for &sat in &sats {
                let sum = saturation_mix_sum(luma, channel, sat).unwrap();
                min_sum = min_sum.min(sum);
                max_sum = max_sum.max(sum);
            }
        }
    }
    assert_eq!((min_sum, max_sum), (-5_502_926_848, 5_771_362_304));
    assert!(i32::try_from(min_sum).is_err() && i32::try_from(max_sum).is_err());

    // Straight Q14, step 512, including both ends (33 values, 33³ colors).
    // Settings: brightness {-16384,-8192,0,8192,16384}, contrast and saturation
    // {-16384,0,16384,32768,49152,65536}. Alpha 16384 makes unpremultiply the
    // identity. Five extra alphas cover the final multiply, which is checked too.
    let straight: Vec<u32> = (0..=16_384).step_by(512).map(|v| v as u32).collect();
    assert_eq!(straight.len(), 33);
    let brightness = [-16_384i32, -8_192, 0, 8_192, 16_384];
    let contrast = [-16_384i32, 0, 16_384, 32_768, 49_152, 65_536];
    let saturation = [-16_384i32, 0, 16_384, 32_768, 49_152, 65_536];
    let mut checked = 0u64;
    for &b in &brightness {
        for &c in &contrast {
            for &s in &saturation {
                for &r in &straight {
                    for &g in &straight {
                        for &bl in &straight {
                            assert!(
                                color_adjust_exact_q14_checked([r, g, bl, 16_384], b, c, s)
                                    .is_some(),
                                "overflow straight ({r},{g},{bl}) b={b} c={c} s={s}"
                            );
                            checked += 1;
                        }
                    }
                }
                for alpha in [0u32, 1, 8_192, 16_383, 16_384] {
                    assert!(
                        color_adjust_exact_q14_checked([0, 16_384, 8_192, alpha], b, c, s)
                            .is_some(),
                        "overflow alpha {alpha} b={b} c={c} s={s}"
                    );
                    checked += 1;
                }
            }
        }
    }
    // Every Q14 value of one channel, the other two at 0, 0.5, and 1, on the
    // endpoint settings (including negative contrast and saturation, which the
    // quantizer still emits).
    let anchors = [0u32, 8_192, 16_384];
    let end_b = [-16_384i32, 16_384];
    let end_c = [-16_384i32, 0, 65_536];
    let end_s = [-16_384i32, 0, 65_536];
    for channel in 0..3 {
        for value in 0..=16_384u32 {
            for &left in &anchors {
                for &right in &anchors {
                    let mut px = [16_384u32; 4];
                    px[channel] = value;
                    px[(channel + 1) % 3] = left;
                    px[(channel + 2) % 3] = right;
                    for &b in &end_b {
                        for &c in &end_c {
                            for &s in &end_s {
                                assert!(
                                    color_adjust_exact_q14_checked(px, b, c, s).is_some(),
                                    "overflow axis {channel}={value} b={b} c={c} s={s}"
                                );
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    // 33³ × 5 × 6 × 6 + 5 × 180 + 3 × 16385 × 9 × 18.
    assert_eq!(checked, 14_432_670);
}

/// The bounds in the memo's overflow table, checked on the analytic endpoints.
#[test]
fn mask_q14_does_not_overflow() {
    const E: [i64; 10] = [0, 1, 4095, 4096, 4097, 8191, 8192, 8193, 16383, 16384];
    const WIDE: i64 = 4_398_180_728_832; // 2^42 + 2^27
    let q = q14::Q;
    // Premultiplied pixels with rgb <= a: 55 of them.
    let pixels: Vec<[i64; 4]> = E
        .iter()
        .flat_map(|&a| {
            E.iter()
                .filter(move |&&v| v <= a)
                .map(move |&v| [v, v, v, a])
        })
        .collect();
    assert_eq!(pixels.len(), 55);
    let ordered = |p: [i64; 4]| p[..3].iter().all(|v| *v <= p[3]);
    let in_range = |p: [i64; 4]| p.iter().all(|v| (0..=q).contains(v));

    let coverage: Vec<i64> = (0..=255).map(|b| i64::from(byte_q14(b))).collect();
    // The memo's table prints 4_177_967 for this sum; the value is 4_178_047.
    // Either fits u32 with room to spare.
    assert_eq!(255 * 16384 + 127, 4_178_047);
    assert_eq!((coverage[0], coverage[255]), (0, q));
    assert!(coverage.windows(2).all(|w| w[0] < w[1]));

    // Mode 21.
    for &v in &pixels {
        for &m in &coverage {
            for o in E {
                let f = m.checked_mul(o).unwrap();
                assert!((0..=Q28).contains(&f));
                for c in v {
                    assert!(c.checked_mul(f).unwrap().checked_add(1 << 27).unwrap() <= WIDE);
                }
                let out = mask_layer_q14(v, f).unwrap();
                assert!(in_range(out) && ordered(out), "mode 21 {v:?} m={m} o={o}");
            }
        }
    }
    // Mode 21 identities: no mask is one Q14 opacity product; F = 0 and F = 2^28.
    for v in 0..=q {
        let p = [v; 4];
        for o in E {
            assert_eq!(
                mask_layer_q14(p, q * o).unwrap()[0],
                div_q_i64(v * o, q).unwrap()
            );
        }
        assert_eq!(mask_layer_q14(p, 0).unwrap(), [0; 4]);
        assert_eq!(mask_layer_q14(p, Q28).unwrap(), p);
    }
    // Mode 22.
    for &input in &pixels {
        for &filtered in &pixels {
            for &s in &coverage {
                for c in 0..4 {
                    let n = input[c] * (q - s) + filtered[c] * s + 8192;
                    assert!(n <= 268_443_648 && n <= i64::from(i32::MAX));
                }
                let out = mask_select_q14(input, filtered, s).unwrap();
                assert!(in_range(out) && ordered(out), "mode 22");
            }
            assert_eq!(mask_select_q14(input, filtered, 0).unwrap(), input);
            assert_eq!(mask_select_q14(input, filtered, q).unwrap(), filtered);
        }
    }
    // Mode 23. Blend 3 (overlay) falls to multiply, as v2.
    for &orig in &pixels {
        for &filt in &pixels {
            for b in [0u32, 1, 127, 128, 254, 255] {
                let m = i64::from(byte_q14(b));
                for o in E {
                    let f = m * o;
                    for blend in 0..4 {
                        let d = q14::straight(orig);
                        let t = q14::straight(filt);
                        for c in 0..3 {
                            for n in [orig[c] * q, d[c] * t[c], (q - d[c]) * (q - t[c])] {
                                assert!(n <= Q28);
                            }
                        }
                        let g = mask_adjust_blend_q14(orig, filt, blend).unwrap();
                        assert!(in_range(g) && ordered(g), "mode 23 g");
                        for c in 0..4 {
                            let n = orig[c]
                                .checked_mul(Q28 - f)
                                .unwrap()
                                .checked_add(g[c].checked_mul(f).unwrap())
                                .unwrap()
                                .checked_add(1 << 27)
                                .unwrap();
                            assert!(n <= WIDE);
                        }
                        let out = mask_adjust_q14(orig, filt, f, blend).unwrap();
                        assert!(in_range(out) && ordered(out), "mode 23");
                    }
                }
            }
        }
    }
    assert_eq!(
        mask_adjust_blend_q14([0, 0, 0, q], [q, q, q, q], 3),
        mask_adjust_blend_q14([0, 0, 0, q], [q, q, q, q], 1)
    );
    // The mask texel of a negative Q8 position is the floor, as `div_euclid`.
    for v in [-257, -256, -255, -1, 0, 255, 256] {
        assert_eq!(v >> 8, i32::div_euclid(v, 256), "{v}");
        assert_eq!(floor_frac_q8(v).0, v >> 8);
    }
    // The resolve product.
    assert_eq!(q * 255, 4_177_920);
}

/// WGSL = Rust = JS: `tests/e2e/mask-v3-reference.mjs` evaluates the JS mask
/// formulas and lookup on the same grids, and every value must equal Rust's.
#[test]
fn mask_v3_rust_and_js_references_agree() {
    const E: [i64; 10] = [0, 1, 4095, 4096, 4097, 8191, 8192, 8193, 16383, 16384];
    let pixels: Vec<[i64; 4]> = E
        .iter()
        .flat_map(|&a| {
            E.iter()
                .filter(move |&&v| v <= a)
                .map(move |&v| [v, v, v, a])
        })
        .collect();
    let q = |b: u32| i64::from(byte_q14(b));
    let all: Vec<i64> = (0..=255).map(q).collect();
    let some: Vec<i64> = [0, 1, 2, 127, 128, 129, 253, 254, 255].map(q).to_vec();
    let few: Vec<i64> = [0, 1, 127, 128, 254, 255].map(q).to_vec();
    let opacities = [0, 1, 8192, 16383, 16384];
    // Rotated, a unit move, a 2x scale with a move, and the rounding tie: all Q16.
    let transforms = [
        rotated_mask_transform(),
        [1.0, 0.0, 0.0, 1.0, 3.0, -7.0],
        [2.0, 0.0, 0.0, 2.0, -5.0, 9.0],
        tie_mask_transform(),
    ];
    let mut rust = Vec::new();
    let mut push = |v: [i64; 4]| {
        for c in v {
            rust.extend_from_slice(&u16::try_from(c).unwrap().to_le_bytes());
        }
    };
    for &v in &pixels {
        for &m in &all {
            for o in E {
                push(mask_layer_q14(v, m * o).unwrap());
            }
        }
    }
    for &input in &pixels {
        for &filtered in &pixels {
            for &s in &some {
                push(mask_select_q14(input, filtered, s).unwrap());
            }
        }
    }
    for &orig in &pixels {
        for &filt in &pixels {
            for &m in &few {
                for o in opacities {
                    for blend in 0..4 {
                        push(mask_adjust_q14(orig, filt, m * o, blend).unwrap());
                    }
                }
            }
        }
    }
    let pattern = mask_image(|x, y| ((x * 7 + y * 13) & 255) as u8);
    for transform in transforms {
        for y in 0..256 {
            for x in 0..256 {
                rust.push(mask_byte_cpu(&pattern, transform, [0, 0, 1], x, y).unwrap());
            }
        }
    }
    let feathered = feathered_rectangle(FEATHER_RECT, FEATHER);
    rust.extend(feathered.as_chunks::<4>().0.iter().map(|px| px[0]));

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let args = format!(
        "[{}]",
        transforms
            .iter()
            .map(|t| format!("[{}]", t.map(|v| format!("{v:?}")).join(",")))
            .collect::<Vec<_>>()
            .join(",")
    );
    let js = std::process::Command::new("node")
        .arg(root.join("tests/e2e/mask-v3-reference.mjs"))
        .arg(args)
        .output()
        .expect("node is required to compare the Rust and JavaScript v3 mask references");
    assert!(
        js.status.success(),
        "JS reference failed: {}",
        String::from_utf8_lossy(&js.stderr)
    );
    assert_eq!(js.stdout.len(), rust.len(), "JS produced a different count");
    if let Some(i) = rust.iter().zip(&js.stdout).position(|(a, b)| a != b) {
        panic!("Rust and JS v3 mask references differ first at byte {i}");
    }
    // Every lookup is Q16, the axis-aligned unit move included.
    for t in transforms {
        assert!(
            crate::renderer::mask_sampler(t, [0.0, 0.0], 1.0).is_some(),
            "{t:?}"
        );
    }
}

/// The host's integer texel map (`renderer::input_map`, evaluated as
/// `effects-q14.wgsl` `input_texel` does) is the CPU's exact floor at every
/// pixel of the offset programs, negative numerators included, and the origin
/// programs are not the same tiles as at the origin.
#[test]
fn mask_origin_programs_place_inputs_and_masks_by_integers() {
    // WGSL `floor_div`: truncating `/`, one less for a negative remainder.
    let floor_div = |n: i32, d: i32| {
        let q = n / d;
        if n < 0 && q * d != n { q - 1 } else { q }
    };
    let mut truncation_differs = false;
    for ((region, _), (a, b)) in mask_origin_cases().into_iter().zip(mask_offset_regions()) {
        let f = |r: [i32; 3]| r.map(|v| v as f32);
        for input in [a, b] {
            let m = crate::renderer::input_map(f(region), [256, 256], f(input)).unwrap();
            assert_ne!(m[3], 0, "{region:?} from {input:?} is not the identity");
            for v in 0..256 {
                let (nx, ny) = (m[0] + v * m[2], m[1] + v * m[2]);
                let shader = |tx: i32, ty: i32| {
                    ((0..256).contains(&tx) && (0..256).contains(&ty))
                        .then(|| (ty * 256 + tx) as usize)
                };
                for (x, y) in [(v, 0), (0, v), (v, v), (v, 255 - v)] {
                    let (nx, ny) = (m[0] + x * m[2], m[1] + y * m[2]);
                    assert_eq!(
                        shader(floor_div(nx, m[3]), floor_div(ny, m[3])),
                        input_texel_cpu(region, input, x, y),
                        "{region:?} from {input:?} at ({x}, {y})"
                    );
                }
                // A negative numerator above -divisor: the floor is -1 (outside),
                // truncation would read texel 0.
                for n in [nx, ny] {
                    if n < 0 && n > -m[3] {
                        assert_eq!(floor_div(n, m[3]), -1);
                        assert_eq!(n / m[3], 0);
                        truncation_differs = true;
                    }
                }
            }
        }
    }
    assert!(
        truncation_differs,
        "no case puts a negative numerator above -divisor"
    );
    // Every origin case puts part of the mask under the tile, and moving the
    // ROI changes the tile.
    let images = mask_images();
    let programs = mask_origin_programs();
    for (region, t) in mask_origin_cases() {
        let bytes: Vec<u8> = (0..256 * 256)
            .map(|i| mask_byte_cpu(&images[5], t, region, i % 256, i / 256).unwrap())
            .collect();
        assert!(bytes.contains(&0), "{region:?}: the mask covers the tile");
        assert!(
            bytes.iter().filter(|b| **b != 0).count() > 1000,
            "{region:?}: no mask"
        );
    }
    for program in &programs {
        let at_origin = Q14Program {
            name: program.name.clone(),
            targets: program.targets,
            regions: vec![],
            stages: program.stages.clone(),
            result: program.result,
        };
        assert_ne!(
            q14_program_cpu(&images, program),
            q14_program_cpu(&images, &at_origin),
            "{}",
            program.name
        );
    }
}

/// The v3 tile selection (`renderer::exact_reach`, used by `v3_reach` for the
/// masks, mode 18 and the plain draws) is the tiles the Q16 lookup reads: the
/// four corners, evaluated exactly, bound every pixel. At origin 999936 that
/// includes tile (3907, 0), which the f64 inverse misses.
#[test]
fn v3_mask_tile_range_is_the_q16_lookup() {
    let renderer_cases = renderer_mask_cases();
    let mut cases: Vec<([i32; 3], [f64; 6])> = renderer_cases
        .iter()
        .map(|c| (c.region(), c.document_transform()))
        .collect();
    cases.extend(mask_origin_cases());
    cases.push(([0, 0, 1], tie_mask_transform()));
    cases.push(([512, -768, 2], rotated_mask_transform()));
    let range_of = |region: [i32; 3], t: [f64; 6]| {
        let inv = integer_inverse(crate::renderer::inverse(t).unwrap()).unwrap();
        let [x, y, scale] = region.map(i64::from);
        crate::renderer::exact_reach(&inv, [x, y], scale, [0, 0, 256, 256], 0)
            .unwrap()
            .keys
            .map(|k| k as i32)
    };
    for (region, t) in cases {
        let inv = integer_inverse(crate::renderer::inverse(t).unwrap()).unwrap();
        let mut every = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        for y in 0..256 {
            for x in 0..256 {
                let px = pixel_q8(region[0], x, region[2]).unwrap();
                let py = pixel_q8(region[1], y, region[2]).unwrap();
                let q = source_q8(&inv, px, py).unwrap();
                let key = [(q[0] >> 8) >> 8, (q[1] >> 8) >> 8];
                every = [
                    every[0].min(key[0]),
                    every[1].min(key[1]),
                    every[2].max(key[0]),
                    every[3].max(key[1]),
                ];
            }
        }
        assert_eq!(range_of(region, t), every, "{region:?} {t:?}");
    }
    let far = &renderer_cases[0];
    let t = far.document_transform();
    assert_eq!(range_of(far.region(), t), [3906, 0, 3907, 0]);
    let right = crate::renderer::point(crate::renderer::inverse(t).unwrap(), [1_000_192.0, 0.0]);
    // The f64 inverse ends the ROI before tile 3907, which the Q16 lookup reads.
    assert!(
        right[0] <= 3907.0 * 256.0,
        "the f64 inverse now reaches tile 3907, so this case no longer separates the f64 and Q16 selections"
    );
    let bytes: Vec<_> = far.mask.iter().map(|(k, s)| (*k, s.mask_rgba())).collect();
    let tiles: Vec<_> = bytes.iter().map(|(k, b)| (*k, &b[..])).collect();
    assert_eq!(
        mask_tiles_byte_cpu(&tiles, t, far.region(), 255, 0),
        Some(255),
        "pixel (255, 0) reads the white tile (3907, 0)"
    );
    let rotated = &renderer_cases[1];
    assert_eq!(
        range_of(rotated.region(), rotated.document_transform()),
        [4, 2, 6, 4]
    );
}

/// DQ3-4 as read by Linus 04:40 (`renderer::v3_reach`): a source that no pixel
/// reaches is skipped, never refused; a refusal needs a reached tile and pixels
/// the shader cannot compute exactly (a Q8 pixel center outside i32, or a pixel
/// dropped for a source Q8 outside i32 that could read a listed tile).
#[test]
fn v3_reach_refuses_only_what_reaches_a_tile() {
    let reach = |inv: [f64; 6], origin: [i64; 2], reach: i32, keys: &[[i32; 2]]| {
        let q = integer_inverse(inv).unwrap();
        crate::renderer::v3_reach(
            &q,
            origin,
            1,
            [0, 0, 256, 256],
            reach,
            keys.iter()
                .map(|k| alto_core::TileKey { tx: k[0], ty: k[1] }),
        )
    };
    // large-translation-out-of-range: 0.01x moved 84000 px inverts to 100x
    // moved -8.4M px. On tile (0, 0) every source Q8 is past -2^31, and the
    // layer's tile (0, 0) is far from that: skipped, not refused, in any cache
    // state. The planted probe tile (32723, 0) is not reached either.
    let far = crate::renderer::inverse([0.01, 0.0, 0.0, 0.01, 84_000.0, 0.0]).unwrap();
    assert_eq!(reach(far, [0, 0], 1, &[[0, 0], [32_723, 0]]), Ok(None));
    let r = crate::renderer::exact_reach(
        &integer_inverse(far).unwrap(),
        [0, 0],
        1,
        [0, 0, 256, 256],
        1,
    )
    .unwrap();
    assert!(r.dropped[0] && r.centers_fit);
    // At the tile the layer covers it is drawn.
    assert!(matches!(reach(far, [84_000, 0], 1, &[[0, 0]]), Ok(Some(_))));
    // A tile origin at 2^23 px has no i32 Q8 pixel center: refused only when a
    // listed tile is reached ("plain [32768, 0] refused").
    let id = ident();
    assert_eq!(reach(id, [8_388_608, 0], 0, &[[32_768, 0]]), Err(()));
    assert_eq!(reach(id, [8_388_608, 0], 0, &[[0, 0]]), Ok(None));
    assert_eq!(
        reach(id, [8_388_352, 0], 0, &[[32_767, 0]]),
        Ok(Some([32_767, 0, 32_767, 0]))
    );
    // Pixels dropped past 2^23 texels read nothing when the reached tiles end
    // at texel 2^23 - 1 (the mask of "mask up to 2^23 draws"); a tile at 2^23
    // could be read by them, so that tile is refused ("mask tile at 2^23 refused").
    let shifted = crate::renderer::inverse([1.0, 0.0, 0.0, 1.0, -8_388_600.0, 0.0]).unwrap();
    assert!(
        crate::renderer::exact_reach(
            &integer_inverse(shifted).unwrap(),
            [0, 0],
            1,
            [0, 0, 256, 256],
            0
        )
        .unwrap()
        .dropped[0]
    );
    assert_eq!(
        reach(shifted, [0, 0], 0, &[[32_767, 0]]),
        Ok(Some([32_767, 0, 32_768, 0]))
    );
    assert_eq!(
        reach(shifted, [0, 0], 0, &[[32_767, 0], [32_768, 0]]),
        Err(())
    );
    // The negative side: a dropped pixel's texel is at most -2^23 - 1, so the
    // nearest lookup cannot read texel -2^23 but the 4-tap's second texel can.
    let left = crate::renderer::inverse([1.0, 0.0, 0.0, 1.0, 8_388_610.0, 0.0]).unwrap();
    assert!(
        crate::renderer::exact_reach(
            &integer_inverse(left).unwrap(),
            [0, 0],
            1,
            [0, 0, 256, 256],
            1
        )
        .unwrap()
        .dropped[0]
    );
    assert!(matches!(
        reach(left, [0, 0], 0, &[[-32_768, 0]]),
        Ok(Some(_))
    ));
    assert_eq!(reach(left, [0, 0], 1, &[[-32_768, 0]]), Err(()));
    // Inputs past the documented bounds overflow i128: checked, so refused
    // (GpuUnsupported), never wrapped.
    let q = integer_inverse(id).unwrap();
    assert!(
        crate::renderer::exact_reach(&q, [i64::MAX, 0], i64::MAX, [0, 0, i64::MAX, 256], 0)
            .is_none()
    );
    assert_eq!(
        crate::renderer::v3_reach(
            &q,
            [i64::MAX, 0],
            i64::MAX,
            [0, 0, i64::MAX, 256],
            0,
            std::iter::once(alto_core::TileKey { tx: 0, ty: 0 })
        ),
        Err(())
    );
    // An empty span reads nothing.
    let q = integer_inverse(id).unwrap();
    assert_eq!(
        crate::renderer::v3_reach(
            &q,
            [0, 0],
            1,
            [5, 0, 5, 256],
            0,
            std::iter::once(alto_core::TileKey { tx: 0, ty: 0 })
        ),
        Ok(None)
    );
}

/// Rust = JS away from the origin: `tests/e2e/mask-v3-origin-reference.mjs`
/// draws the `origin ...` programs of [`mask_origin_programs`] with
/// `renderM5V3` at the same ROIs, and every byte must equal `q14_program_cpu`.
#[test]
fn mask_origin_rust_and_js_references_agree() {
    let images = mask_images();
    let mut rust = Vec::new();
    for program in mask_origin_programs()
        .iter()
        .filter(|p| p.name.starts_with("origin "))
    {
        rust.extend(q14_program_cpu(&images, program));
    }
    let cases = mask_origin_cases();
    assert_eq!(rust.len(), cases.len() * 3 * 256 * 256 * 4);
    let args = format!(
        "[{}]",
        cases
            .iter()
            .map(|(r, t)| format!(
                "[[{}],[{}]]",
                r.map(|v| v.to_string()).join(","),
                t.map(|v| format!("{v:?}")).join(",")
            ))
            .collect::<Vec<_>>()
            .join(",")
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let js = std::process::Command::new("node")
        .arg(root.join("tests/e2e/mask-v3-origin-reference.mjs"))
        .arg(args)
        .output()
        .expect("node is required to compare the Rust and JavaScript v3 mask references");
    assert!(
        js.status.success(),
        "JS reference failed: {}",
        String::from_utf8_lossy(&js.stderr)
    );
    assert_eq!(js.stdout.len(), rust.len(), "JS produced a different count");
    if let Some(i) = rust.iter().zip(&js.stdout).position(|(a, b)| a != b) {
        let tile = i / (256 * 256 * 4);
        panic!(
            "Rust and JS v3 mask references differ first in tile {tile} at byte {}",
            i % (256 * 256 * 4)
        );
    }
}

/// A [`RendererCase`] as `tests/e2e/m5-v3-renderer-reference.mjs` reads it:
/// tiles as seeds, masks with their document transform after linking.
pub(super) fn renderer_case_json(case: &RendererCase) -> String {
    let floats = |v: &[f64]| {
        v.iter()
            .map(|x| format!("{x:?}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let tiles = |list: &[([i32; 2], TileSpec)]| {
        list.iter()
            .map(|(k, spec)| format!("[[{},{}],{}]", k[0], k[1], spec.json()))
            .collect::<Vec<_>>()
            .join(",")
    };
    let mask = |m: &Option<RendererCaseMask>, world: [f64; 6]| match m {
        None => "null".to_string(),
        Some(m) => format!(
            "{{\"transform\":[{}],\"parent\":{},\"tiles\":[{}]}}",
            floats(&m.transform),
            if m.linked {
                format!("[{}]", floats(&world))
            } else {
                "null".into()
            },
            tiles(&m.tiles)
        ),
    };
    let settings = |(c, s, b): (f32, f32, f32)| format!("[{c:?},{s:?},{b:?}]");
    let layers: Vec<String> = case
        .layers
        .iter()
        .map(|layer| match layer {
            RendererLayer::Raster {
                tiles: list,
                transform,
                adjust,
                selection,
                mask: m,
                opacity,
                blend,
            } => format!(
                "{{\"tiles\":[{}],\"transform\":[{}],\"adjust\":{},\"selection\":{},\"mask\":{},\"opacity\":{opacity:?},\"blend\":{}}}",
                tiles(list),
                floats(transform),
                adjust.map_or("null".into(), settings),
                mask(selection, *transform),
                mask(m, *transform),
                *blend as u32
            ),
            RendererLayer::Adjust {
                settings: s,
                mask: m,
                opacity,
                blend,
            } => format!(
                "{{\"adjustment\":{},\"mask\":{},\"opacity\":{opacity:?},\"blend\":{}}}",
                settings(*s),
                mask(m, ident()),
                *blend as u32
            ),
        })
        .collect();
    format!(
        "{{\"level\":{},\"key\":[{},{}],\"canvas\":[{},{}],\"layers\":[{}]}}",
        case.level,
        case.key[0],
        case.key[1],
        case.canvas[0],
        case.canvas[1],
        layers.join(",")
    )
}

/// Rust = JS for the float purge: every drawn [`renderer_cases`] tile (modes
/// 18, 17, 19, 20, 21, 22 and 23 at the origin, negative keys, near 10^6 px and
/// next to 2^23 px, transformed rasters over several tiles, the canvas edge and
/// level 1) equals `renderM5V3` in `tests/e2e/m5-v3-renderer-reference.mjs`,
/// and the opacity and color-adjust quantizers agree at the Q14 ties.
#[test]
fn renderer_rust_and_js_references_agree() {
    let cases: Vec<RendererCase> = renderer_cases()
        .into_iter()
        .filter(|c| c.refused.is_none())
        .collect();
    let mut rust = Vec::new();
    for case in &cases {
        rust.extend(renderer_case_cpu(case));
    }
    let mut values: Vec<u32> = vec![
        0,
        0x8000_0000,
        f32::NAN.to_bits(),
        f32::INFINITY.to_bits(),
        f32::NEG_INFINITY.to_bits(),
        1,
        (1.0f32 / 32768.0).to_bits() - 1,
    ];
    for q in (0..4 * 16384).step_by(7) {
        let tie = ((2 * q + 1) as f32 / 32768.0).to_bits();
        for b in [tie - 1, tie, tie + 1] {
            values.extend([b, b | 0x8000_0000]);
        }
    }
    for v in &values {
        let v = f32::from_bits(*v);
        rust.extend(crate::composite_opacity_q14(v).to_le_bytes());
        rust.extend(crate::renderer::color_adjust_q14(v).to_le_bytes());
    }
    let input = format!(
        "{{\"cases\":[{}],\"values\":[{}]}}",
        cases
            .iter()
            .map(renderer_case_json)
            .collect::<Vec<_>>()
            .join(","),
        values
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut child = std::process::Command::new("node")
        .arg(root.join("tests/e2e/m5-v3-renderer-reference.mjs"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("node is required to compare the Rust and JavaScript v3 references");
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.as_bytes()).unwrap();
    }
    let js = child.wait_with_output().unwrap();
    assert!(
        js.status.success(),
        "JS reference failed: {}",
        String::from_utf8_lossy(&js.stderr)
    );
    assert_eq!(js.stdout.len(), rust.len(), "JS produced a different count");
    if let Some(i) = rust.iter().zip(&js.stdout).position(|(a, b)| a != b) {
        let tiles = cases.len() * 256 * 256 * 4;
        if i < tiles {
            panic!(
                "Rust and JS differ first in {} at byte {}",
                cases[i / (256 * 256 * 4)].name,
                i % (256 * 256 * 4)
            );
        }
        let v = f32::from_bits(values[(i - tiles) / 8]);
        panic!("Rust and JS quantize {v:e} differently");
    }
}

/// The renderer cases draw something worth comparing: most pixels are not
/// transparent and not all alike, transformed rasters read several tiles, and
/// the canvas edge case is transparent exactly past the edge.
#[test]
fn renderer_cases_are_not_trivial() {
    for case in renderer_cases().iter().filter(|c| c.refused.is_none()) {
        let tile = renderer_case_cpu(case);
        let pixels: Vec<&[u8]> = tile.chunks(4).collect();
        let drawn = pixels.iter().filter(|p| p[3] != 0).count();
        let distinct: std::collections::HashSet<&[u8]> = pixels.iter().copied().collect();
        assert!(drawn > 10_000, "{}: {drawn} drawn pixels", case.name);
        assert!(
            distinct.len() > 1_000,
            "{}: {} colors",
            case.name,
            distinct.len()
        );
    }
    let edge = renderer_cases()
        .into_iter()
        .find(|c| c.name.contains("canvas edge"))
        .unwrap();
    let tile = renderer_case_cpu(&edge);
    let alpha = |x: usize, y: usize| tile[(y * 256 + x) * 4 + 3];
    assert!((0..211).any(|y| alpha(99, y) != 0));
    assert!((0..256).all(|y| alpha(100, y) == 0 && alpha(255, y) == 0));
    assert!((0..256).all(|x| alpha(x, 211) == 0));
    // A rotated raster from near -10^6 px reads two tile columns and rows of
    // its nine (the selection culls the rest).
    let rotated = renderer_cases()
        .into_iter()
        .find(|c| c.name == "renderer [3906, 5] from [-3907, 5] rotated raster, linked mask o=0.5")
        .unwrap();
    let RendererLayer::Raster {
        tiles, transform, ..
    } = &rotated.layers[1]
    else {
        panic!("not a raster");
    };
    let q = integer_inverse(crate::renderer::inverse(*transform).unwrap()).unwrap();
    let range = crate::renderer::exact_reach(&q, [3906 * 256, 5 * 256], 1, [0, 0, 256, 256], 0)
        .unwrap()
        .keys
        .map(|k| k as i32);
    assert_eq!(range, [-3907, 4, -3906, 5]);
    assert_eq!(tiles.len(), 9);
}

/// Mode 18 is nearest, which matches the CPU references (`q14_program_cpu`,
/// `renderM5V3`): a raster drawn as it is (`placement`) reads its own pixel at
/// every origin of [`purge_regions`]. The integer 4-tap of the plain
/// compositor would not: its Q8 position of pixel `i` is `i * 256 + 128`, a
/// half-texel weight that mixes two texels.
#[test]
fn v3_raster_lookup_is_nearest_and_copies_as_placed() {
    let image = raster_tile_bytes(3);
    let tiles = [([0, 0], &image[..])];
    for region in purge_regions() {
        for (x, y) in [(0, 0), (255, 0), (0, 255), (255, 255), (128, 77)] {
            let i = ((y * 256 + x) * 4) as usize;
            assert_eq!(
                tiles_texel_cpu(&tiles, placement(region), region, x, y),
                Some([image[i], image[i + 1], image[i + 2], image[i + 3]]),
                "{region:?} ({x}, {y})"
            );
            let inv =
                integer_inverse(crate::renderer::inverse(placement(region)).unwrap()).unwrap();
            let q = source_q8(
                &inv,
                pixel_q8(region[0], x, region[2]).unwrap(),
                pixel_q8(region[1], y, region[2]).unwrap(),
            )
            .unwrap();
            assert_eq!(q, [x * 256 + 128, y * 256 + 128], "{region:?}");
        }
    }
}
