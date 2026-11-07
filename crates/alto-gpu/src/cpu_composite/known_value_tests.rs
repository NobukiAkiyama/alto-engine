//! Known pixels, locked hashes, explicit gaps and measurement bookkeeping.
use super::*;

/// The CPU programs reproduce the memo's stated pixels and identities.
#[test]
fn mask_programs_known_values() {
    let images = mask_images();
    let programs = mask_programs();
    let by_name = |name: &str| programs.iter().find(|p| p.name == name).unwrap();
    let run = |name: &str| q14_program_cpu(&images, by_name(name));
    // The browser `mask` case: opaque red where x < 128, transparent elsewhere.
    let tile = run("v3-mask");
    for (i, px) in tile.as_chunks::<4>().0.iter().enumerate() {
        let want = if i % 256 < 128 {
            [255, 0, 0, 255]
        } else {
            [0; 4]
        };
        assert_eq!(*px, want, "pixel {i}");
    }
    // Opacity 0.5 on an opaque pixel through a full mask is F = 2^27; the Q14
    // value 8192 resolves to the tie 127.5, which goes up.
    let half = pixel(&run("m5-mask-white o=0.5"), 255, 200);
    assert_eq!((half[0], half[3]), (128, 128));
    // A full mask equals the v3 composite of the unmasked layer at that opacity.
    for opacity in [1.0, 0.5, 0.35] {
        let case = Case {
            name: format!("white mask {opacity}"),
            w: 256,
            h: 256,
            level: 0,
            tx: 0,
            ty: 0,
            lock: false,
            measure: false,
            nodes: vec![raster(
                Image {
                    w: 256,
                    h: 256,
                    pixels: color_adjust_gradient(),
                },
                0,
                opacity,
                true,
                ident(),
            )],
        };
        assert_eq!(
            run(&format!("m5-mask-white o={opacity}")),
            render_at(&case, 3),
            "{}",
            case.name
        );
    }
    // The feathered selection has every byte.
    let mut seen = [false; 256];
    for px in images[3].as_chunks::<4>().0 {
        seen[px[0] as usize] = true;
    }
    assert!(seen.iter().all(|s| *s), "feathered selection misses a byte");
    // Opacity 0 removes the layer; the mask ramp shows every coverage.
    assert!(run("m5-mask-coverage o=0").iter().all(|v| *v == 0));
    let full = run("m5-mask-coverage o=1");
    assert_eq!(pixel(&full, 0, 200), [0; 4]);
    assert_eq!(
        pixel(&full, 255, 200),
        pixel(&run("m5-mask-white o=1"), 255, 200)
    );
    // Every v3 mask takes the Q16 lookup, axis-aligned unit moves included. A
    // non-integer ROI, a level scale below 1, a singular transform or an inverse
    // outside the Q16 format has no lookup, and the host refuses it (DQ3-4).
    let sampler = crate::renderer::mask_sampler;
    for (t, origin, scale) in [
        (rotated_mask_transform(), [0.0, 0.0], 1.0),
        ([1.0, 0.0, 0.0, 1.0, 3.0, -7.0], [256.0, 0.0], 2.0),
        (ident(), [-512.0, 256.0], 4.0),
    ] {
        assert!(
            sampler(t, origin, scale).is_some(),
            "{t:?} at {origin:?} {scale}"
        );
    }
    for (t, origin, scale) in [
        (rotated_mask_transform(), [0.0, 0.0], 0.5),
        (rotated_mask_transform(), [0.5, 0.0], 1.0),
        (ident(), [0.0, 0.0], 0.5),
        ([0.0, 0.0, 0.0, 0.0, 0.0, 0.0], [0.0, 0.0], 1.0),
        ([1.0, 0.0, 0.0, 1.0, 1e12, 0.0], [0.0, 0.0], 1.0),
        ([1e-6, 0.0, 0.0, 1e-6, 0.0, 0.0], [0.0, 0.0], 1.0),
    ] {
        assert!(
            sampler(t, origin, scale).is_none(),
            "{t:?} at {origin:?} {scale} has no Q16 lookup"
        );
        assert_eq!(
            mask_byte_cpu(&images[5], t, [0, 0, 1], 0, 0).is_none(),
            origin == [0.0, 0.0] && scale == 1.0
        );
    }
    // The tie: the Q8 sum is exactly half a step below a texel edge. Half away
    // from zero sends x = 3 (+) up to texel 1, and x = 2 (-) down to texel -1,
    // which is the tile at key -1 (not in the map, byte 0). Rows move up by one
    // texel; the last row lands on key 1.
    let pattern = |x: i32, y: i32| ((x * 7 + y * 13) & 255) as u8;
    let tie = tie_mask_transform();
    let byte = |x, y| mask_byte_cpu(&images[5], tie, [0, 0, 1], x, y).unwrap();
    for y in [0, 1, 254] {
        assert_eq!(byte(2, y), 0, "x = 2, y = {y}");
        assert_eq!(byte(3, y), pattern(1, y + 1), "x = 3, y = {y}");
        assert_eq!(byte(255, y), pattern(253, y + 1), "x = 255, y = {y}");
    }
    assert_eq!(byte(3, 255), 0);
    assert_ne!(run("m5-mask-tie"), run("m5-mask-coverage o=1"));
    assert_ne!(run("m5-mask-rotated"), run("m5-mask-coverage o=1"));
    // The selection keeps the input where the selection is 0 and is the adjusted
    // copy where it is 255.
    let selected = run("m5-selection-color-adjust");
    let adjusted = color_adjust_image_v3(&images[0], 0.0, 1.7, 1.6);
    let plain = run("m5-mask-white o=1");
    assert_eq!(pixel(&selected, 250, 128), pixel(&plain, 250, 128));
    assert_ne!(pixel(&adjusted, 250, 128), pixel(&plain, 250, 128));
    assert_eq!(pixel(&selected, 128, 128), pixel(&adjusted, 128, 128));
    // Multiply and overlay are the same v2 product; screen is not.
    let mul = run("m5-adjustment-mask-multiply o=1");
    assert_eq!(mul, run("m5-adjustment-mask-overlay o=1"));
    assert_ne!(mul, run("m5-adjustment-mask-screen o=1"));
    assert_ne!(mul, run("m5-adjustment-mask-normal o=1"));
    // Every program draws something other than the empty tile.
    for program in &programs {
        if !program.name.ends_with("o=0") {
            assert!(
                q14_program_cpu(&images, program).iter().any(|v| *v != 0),
                "{}",
                program.name
            );
        }
    }
}

#[test]
fn color_adjust_solid_differs_from_the_source_and_is_uniform() {
    let tile = color_adjust_tile([40, 80, 120, 255], 0.6f64 as f32, 1.0, 1.0);
    assert_eq!(tile.len(), 262_144);
    assert_ne!(&tile[..4], &[40, 80, 120, 255]);
    assert_eq!(&tile[..4], &[193, 233, 255, 255]);
    assert!(tile.chunks(4).all(|pixel| pixel == &tile[..4]));
}

#[test]
fn color_adjust_gradient_covers_every_channel_and_the_alpha_anchors() {
    let src = color_adjust_gradient();
    assert_eq!(src.len(), 262_144);
    let mut seen = [0u32; 4];
    let mut hit = [[false; 256]; 4];
    for px in src.as_chunks::<4>().0 {
        for channel in 0..4 {
            let value = px[channel] as usize;
            if !hit[channel][value] {
                hit[channel][value] = true;
                seen[channel] += 1;
            }
        }
    }
    assert_eq!(seen, [256, 256, 256, 256]);
    assert_eq!(pixel(&src, 0, 0)[3], 0);
    assert_eq!(pixel(&src, 0, 1)[3], 1);
    assert_eq!(pixel(&src, 0, 2)[3], 128);
    assert_eq!(pixel(&src, 0, 3)[3], 254);
    assert_eq!(pixel(&src, 255, 4)[3], 255);
    let anchor = [200, 3, 19, 254];
    let flat = color_adjust_texel(anchor, 0.0, 1.0, 1.0);
    let gray = color_adjust_texel(anchor, -0.35f64 as f32, 1.7f64 as f32, 0.0);
    let vivid = color_adjust_texel(anchor, 0.45f64 as f32, 0.5f64 as f32, 1.6f64 as f32);
    assert_ne!(flat, gray);
    assert_ne!(flat, vivid);
    assert_ne!(gray, vivid);
    assert_eq!(
        color_adjust_texel([40, 80, 120, 0], 0.45, 1.7, 1.6),
        [0, 0, 0, 0]
    );
    assert_eq!(color_adjust_grid().len(), 20);
    // Same FNV-1a bytes as tests/e2e/cpu-composite.js for the original 12.
    // The endpoint settings are render version 3 only.
    let expected = [
        "09de5f49e7068fbe",
        "d296346794223ecf",
        "0a0b6c96d748999c",
        "e80976ac9139b99d",
        "5926c4243a96f79f",
        "6e6fc79ca84727ab",
        "beb375818b446960",
        "8dd8cd53b2a57140",
        "54e47915c5a04b18",
        "9be25cf48c61da5a",
        "c0cc9de8851d97d2",
        "eaae1596e5e99a2b",
    ];
    let src = color_adjust_gradient();
    for (expected, (contrast, saturation, brightness)) in
        expected.into_iter().zip(color_adjust_grid())
    {
        let tile = color_adjust_image(&src, brightness, contrast, saturation);
        assert_eq!(
            format!("{:016x}", fnv64(&tile)),
            expected,
            "c={contrast} s={saturation} b={brightness}"
        );
    }
    // Render version 3, integer M5 stages. Same bytes as renderColorAdjustV3 in tests/e2e/cpu-composite.js.
    let exact = [
        "7653b3dacf9f4a5d",
        "74bbfcc000f4b9bb",
        "628362e24b9f31df",
        "cd2fba24007d3e58",
        "cc0d355c44d9b665",
        "3f1281ebe63782d5",
        "1181326aa3c6b1fd",
        "2193ceed938b774e",
        "784796ff684f97ba",
        "f8dd1fdfc273cc83",
        "e1a6f921d39e706d",
        "22c67fdcd3f3045e",
        // Contrast 0 makes every channel the same, so saturation does not move a pixel.
        "204b8da3774c1b25",
        "455fe98699531725",
        "204b8da3774c1b25",
        "455fe98699531725",
        "1de207d9c6ef6ab5",
        "33c42326a62c7eb0",
        "e401eecb14e94b56",
        "63a34d861b27424e",
    ];
    for (expected, (contrast, saturation, brightness)) in exact.into_iter().zip(color_adjust_grid())
    {
        let tile = color_adjust_image_v3(&src, brightness, contrast, saturation);
        assert_eq!(
            format!("{:016x}", fnv64(&tile)),
            expected,
            "v3 c={contrast} s={saturation} b={brightness}"
        );
    }
}

#[test]
fn normal_blend_of_opaque_blue_and_half_red_is_the_known_pixel() {
    let case = cases()
        .into_iter()
        .find(|c| c.name == "blend-normal")
        .unwrap();
    let tile = render(&case);
    assert_eq!(tile.len(), 262_144);
    assert_eq!(pixel(&tile, 0, 0), [128, 0, 127, 255]);
    assert_eq!(pixel(&tile, 255, 255), [128, 0, 127, 255]);
}

#[test]
fn level1_samples_pixel_centers() {
    let case = cases()
        .into_iter()
        .find(|c| c.name == "level1-centers")
        .unwrap();
    // p = (0.5) * 2 = 1, so the corner texel is source pixel (1, 1) of the ramp.
    assert_eq!(pixel(&render(&case), 0, 0), [1, 1, 8, 255]);
}

#[test]
fn integer_filter_recovers_a_texel_and_rounds_half_up() {
    use crate::sample::{quantize_coeff, quantize_translation};
    assert_eq!(quantize_coeff(0.5), Some(32_768));
    assert_eq!(quantize_coeff(-0.5), Some(-32_768));
    assert_eq!(quantize_coeff(1.0), Some(65_536));
    assert_eq!(quantize_translation(-40_000.0), Some(-40_000 * 65_536));
    assert!(quantize_coeff(-40_000.0).is_none());
    assert!(quantize_translation(1.0e16).is_none());
    assert_eq!(bilinear_byte(7, 9, 7, 9, 0, 0), 7);
    assert_eq!(bilinear_byte(0, 1, 0, 1, 128, 0), 1);
    assert_eq!(floor_frac_q8(-1), (-1, 255));
    for byte in 0..=255u8 {
        let decoded = f32::from(byte) / 255.0;
        let restored = (decoded * 255.0).round_ties_even() as u8;
        assert_eq!(restored, byte, "unorm byte {byte} did not round-trip");
    }
}

/// Exact `n+0.5` is f32 equality, not an epsilon. `0.5` in byte space is a tie
/// where round-ties-even and round-half-up disagree. `127.5` is a tie where
/// they agree. `10` is an integer byte.
#[test]
fn unorm_boundary_split_is_exact_f32_equality() {
    let half_up_tie = f32::from_bits(989_888_641);
    let agreed = classify_boundary(half_up_tie, half_up_tie, 1, 0);
    assert!(agreed.exact_half && agreed.tie_even_vs_half_up);
    assert!(!agreed.exact_half_not_half_up && !agreed.not_exact_half);
    assert!(agreed.gpu_ties_odd);
    assert_eq!(agreed.class, "tie-even-vs-half-up");

    let rules_agree = f32::from_bits(1_056_964_608);
    let other = classify_boundary(rules_agree, rules_agree, 127, 128);
    assert!(other.exact_half && other.exact_half_not_half_up);
    assert!(!other.tie_even_vs_half_up && !other.not_exact_half);
    assert!(other.gpu_ties_odd);
    assert_eq!(other.class, "exact-half-not-half-up");

    let integer = 10.0f32 / 255.0;
    let off = classify_boundary(integer, integer, 11, 10);
    assert!(off.not_exact_half && off.bits_equal);
    assert!(!off.exact_half);
    assert_eq!(off.class, "not-exact-half");

    let drifted = classify_boundary(half_up_tie, rules_agree, 1, 0);
    assert!(!drifted.bits_equal && !drifted.not_exact_half);
    assert_eq!(drifted.class, "floats-differ");

    let dir = std::env::temp_dir().join(format!(
        "alto-boundary-{}-{}",
        std::process::id(),
        half_up_tie.to_bits()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let pixel = MeasuredPixel {
        x: 1,
        y: 2,
        sources: Vec::new(),
        gpu: [1, 127, 11, 9],
        cpu: [0, 128, 10, 8],
        gpu_pre: [half_up_tie, rules_agree, integer, half_up_tie],
        cpu_pre: [half_up_tie, rules_agree, integer, rules_agree],
    };
    write_prequant(
        &dir,
        "unit",
        &[MeasuredCase {
            render_path: None,
            name: "boundary".into(),
            count: 1,
            max_abs: [1, 1, 1, 1],
            mismatches: vec![pixel],
            length_note: String::new(),
            ops: "unit".into(),
        }],
    );
    let json = std::fs::read_to_string(dir.join("prequant.json")).unwrap();
    assert!(json.contains("\"bitIdenticalExactHalf\": 2"));
    assert!(json.contains("\"bitIdenticalTieEvenVsHalfUp\": 1"));
    assert!(json.contains("\"bitIdenticalExactHalfNotHalfUp\": 1"));
    assert!(json.contains("\"bitIdenticalNotExactHalf\": 1"));
    assert!(json.contains("\"bitIdenticalExactHalfGpuTiesToOdd\": 2"));
    assert!(json.contains("\"boundaryClass\": \"tie-even-vs-half-up\""));
    assert!(json.contains("\"boundaryClass\": \"exact-half-not-half-up\""));
    assert!(json.contains("\"boundaryClass\": \"not-exact-half\""));
    assert!(json.contains("\"boundaryClass\": \"floats-differ\""));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A 0.1× layer moved 4000px inverts to a -40000px translation. That Q16 value
/// does not fit in `i32`. The integer path keeps it, and the sampled ramp texel
/// is the one at source (5, 5). A translation that is not an exact `f64` integer
/// leaves the integer path instead of being clamped.
#[test]
fn large_translation_is_exact_on_the_integer_path() {
    use crate::sample::{SAMPLE_F32, SAMPLE_INTEGER, pack_inverse};
    let forward = [0.1, 0.0, 0.0, 0.1, 4000.0, 0.0];
    let inv = Affine2(forward).inverse().unwrap().0;
    assert!(
        (inv[4] + 40_000.0).abs() < 1e-6,
        "inverse translation {inv:?}"
    );
    let packed = pack_inverse(inv, RENDER_VERSION);
    assert_eq!(packed.sample_mode, SAMPLE_INTEGER);
    let q = crate::sample::integer_inverse_v2(inv).unwrap();
    assert_eq!(q.tx, -40_000 * 65_536);
    let case = cases()
        .into_iter()
        .find(|c| c.name == "large-translation")
        .unwrap();
    assert_eq!(pixel(&render(&case), 160, 0), [5, 5, 40, 255]);
    let overflow = pack_inverse([0.1, 0.0, 0.0, 0.1, 1.0e16, 0.0], RENDER_VERSION);
    assert_eq!(overflow.sample_mode, SAMPLE_F32);

    // 0.01× moved 84000px. The inverse translation is −8400000px. Tile 983's
    // source Q8 does not fit in i32; wrapping the low 32 bits would read the
    // ramp. Every pixel is transparent, and a tile origin past ±2^23 px is too.
    let far = [0.01, 0.0, 0.0, 0.01, 84_000.0, 0.0];
    let far_inv = Affine2(far).inverse().unwrap().0;
    assert!((far_inv[4] + 8_400_000.0).abs() < 1e-3, "{far_inv:?}");
    assert_eq!(
        pack_inverse(far_inv, RENDER_VERSION).sample_mode,
        SAMPLE_INTEGER
    );
    let far_q = crate::sample::integer_inverse_v2(far_inv).unwrap();
    let px = pixel_q8(0, 0, 1).unwrap();
    let py = pixel_q8(0, 0, 1).unwrap();
    assert!(
        source_q8(&far_q, px, py).is_none(),
        "source Q8 past ±2^23 px must not wrap into i32"
    );
    let far_case = cases()
        .into_iter()
        .find(|c| c.name == "large-translation-out-of-range")
        .unwrap();
    let mut ops = Vec::new();
    walk(&far_case.nodes[0], ident(), &mut ops);
    let Op::Draw { tiles, .. } = &ops[0] else {
        panic!("expected one draw");
    };
    let planted = lookup(tiles, (32_723, 0)).expect("probe tile");
    let probe_i = (50 * 256 + 177) * 4;
    assert_eq!(
        &planted[probe_i..probe_i + 4],
        &[255, 0, 0, 255],
        "the texel a truncated i32 would fetch has to be ink, or this test does not catch a wrap"
    );
    let tile = render(&far_case);
    assert!(
        tile.iter().all(|&b| b == 0),
        "out-of-range source must be transparent, not the planted texel"
    );
    assert!(pixel_q8(8_388_608, 0, 1).is_none());
    assert_eq!(pixel_q8(0, 0, 1), Some(128));
}

#[test]
fn cpu_reference_hashes_match_the_locked_file() {
    let path = reference_path();
    let text = reference_text();
    if std::env::var("ALTO_WRITE_PARITY").ok().as_deref() == Some("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| String::new());
    assert_eq!(
        existing, text,
        "CPU reference changed; regenerate with ALTO_WRITE_PARITY=1 cargo test -p alto-gpu cpu_reference"
    );
}

#[test]
fn plain_scene_gaps_are_explicit() {
    let tiles = scene(RContent::Tiles(Vec::new()), RFeatures::default());
    assert!(plain_scene(&tiles) && gap(&tiles).is_none());
    let mask = scene(
        RContent::Tiles(Vec::new()),
        RFeatures {
            mask: Some(RMask {
                tiles: Vec::new(),
                linked: true,
                transform: ident(),
            }),
            ..RFeatures::default()
        },
    );
    let vector_mask = scene(
        RContent::Tiles(Vec::new()),
        RFeatures {
            vector_mask: Some(VectorMask {
                path: PathData::default(),
                fill_rule: FillRule::NonZero,
                feather: 0.0,
                enabled: true,
                transform: Affine2::default(),
            }),
            ..RFeatures::default()
        },
    );
    let clip_path = scene(
        RContent::Tiles(Vec::new()),
        RFeatures {
            clip_path: Some(ClipPath {
                path: PathData::default(),
                fill_rule: FillRule::NonZero,
                enabled: true,
                transform: Affine2::default(),
            }),
            ..RFeatures::default()
        },
    );
    let clip = scene(
        RContent::Tiles(Vec::new()),
        RFeatures {
            clip: true,
            ..RFeatures::default()
        },
    );
    // The plain compositor cannot draw effects; v3 sends them to integer M5.
    let effects = scene(
        RContent::Tiles(Vec::new()),
        RFeatures {
            effects: vec![REffect {
                kind: REffectKind::Invert,
                selection: None,
            }],
            ..RFeatures::default()
        },
    );
    let vector = scene(
        RContent::Mesh(std::sync::Arc::new(Vec::new())),
        RFeatures::default(),
    );
    let text = scene(
        RContent::Text(std::sync::Arc::new(Vec::new())),
        RFeatures::default(),
    );
    let adjustment = scene(
        RContent::Adjustment(REffect {
            kind: REffectKind::ColorAdjust {
                brightness: 0.4,
                contrast: 0.0,
                saturation: 0.0,
            },
            selection: None,
        }),
        RFeatures::default(),
    );
    let unsupported = scene(RContent::Unsupported, RFeatures::default());
    for (scene, reason) in [
        (&mask, "mask"),
        (&vector_mask, "vector mask"),
        (&clip_path, "clip path"),
        (&clip, "clip"),
        (&effects, "effects"),
        (&vector, "vector"),
        (&text, "text"),
        (&adjustment, "adjustment"),
    ] {
        assert!(!plain_scene(scene), "{reason} was treated as a plain scene");
        assert_eq!(gap(scene), Some(reason));
        let mut v3 = scene.clone();
        v3.render_version = 3;
        // All document effects, masks, clipping, adjustments and vectors have
        // integer stages. Direct layout text remains a refused internal input.
        let integer = matches!(
            reason,
            "effects" | "adjustment" | "mask" | "vector" | "vector mask" | "clip path" | "clip"
        );
        let expected = if integer {
            "v3-m5-integer"
        } else {
            "v3-unsupported"
        };
        assert_eq!(gap(&v3), Some(expected), "v3 {reason}");
        assert_eq!(crate::renderer::v3_gap(&v3), (!integer).then_some(reason));
    }
    // The plain compositor skips an unsupported node, so the tile stays exact.
    assert!(plain_scene(&unsupported));
    assert_eq!(gap(&unsupported), Some("unsupported"));
    let mut v3 = unsupported.clone();
    v3.render_version = 3;
    assert_eq!(gap(&v3), None);

    // Layer effects and selection-limited effects use integer M5 stages.
    let color_adjust = REffect {
        kind: REffectKind::ColorAdjust {
            brightness: 0.2,
            contrast: 1.0,
            saturation: 1.0,
        },
        selection: None,
    };
    let mut layer = scene(
        RContent::Tiles(Vec::new()),
        RFeatures {
            effects: vec![color_adjust.clone()],
            ..RFeatures::default()
        },
    );
    layer.render_version = 3;
    layer.nodes[0].opacity = 0.5;
    assert_eq!(gap(&layer), Some("v3-m5-integer"));
    layer.nodes[0].features.effects[0].selection = Some(RMask {
        tiles: Vec::new(),
        linked: true,
        transform: ident(),
    });
    assert_eq!(crate::renderer::v3_gap(&layer), None);
    assert_eq!(gap(&layer), Some("v3-m5-integer"));
    let mut blur = layer.clone();
    blur.nodes[0].features.effects[0].kind = REffectKind::GaussianBlur {
        radius: 8.0,
        radius_q8: 2048,
    };
    assert_eq!(crate::renderer::v3_gap(&blur), None);
    assert_eq!(gap(&blur), Some("v3-m5-integer"));
    // A visible stroke now takes the integer outline and coverage path.
    let mut stroked = vector.clone();
    stroked.render_version = 3;
    stroked.nodes[0].content = RContent::Mesh(std::sync::Arc::new(vec![alto_vector::StyledPath {
        data: PathData::default(),
        fill: None,
        stroke: Some(alto_vector::Stroke {
            color: [1.0; 4],
            width: 1.0,
            cap: Default::default(),
            join: Default::default(),
            miter_limit: 4.0,
        }),
        fill_rule: FillRule::NonZero,
    }]));
    assert_eq!(crate::renderer::v3_gap(&stroked), None);
    // Visibility still follows Q14 alpha and a nonzero stored width.
    for (width, alpha, visible) in [
        (0.0f32, 1.0f32, false),
        (-0.0, 1.0, false),
        (1.0, 0.0, false),
        (1.0, f32::from_bits(2f32.powi(-15).to_bits() - 1), false),
        (1.0, 2f32.powi(-15), true),
        (f32::from_bits(1), 1.0, true),
    ] {
        let mut s = stroked.clone();
        let RContent::Mesh(paths) = &mut s.nodes[0].content else {
            unreachable!()
        };
        let stroke = std::sync::Arc::make_mut(paths)[0].stroke.as_mut().unwrap();
        stroke.width = width;
        stroke.color[3] = alpha;
        assert_eq!(crate::raster_q8::stroke_visible(stroke), visible);
        assert_eq!(crate::renderer::v3_gap(&s), None, "{width} {alpha:e}");
    }
    let mut multiply = adjustment.clone();
    multiply.render_version = 3;
    multiply.nodes[0].blend = RenderBlend::Multiply;
    assert_eq!(crate::renderer::v3_gap(&multiply), None);
    assert_eq!(gap(&multiply), Some("v3-m5-integer"));
    multiply.nodes[0].features.mask = mask.nodes[0].features.mask.clone();
    assert_eq!(crate::renderer::v3_gap(&multiply), None);
    assert_eq!(gap(&multiply), Some("v3-m5-integer"));
    let mut invert = multiply.clone();
    invert.nodes[0].content = RContent::Adjustment(REffect {
        kind: REffectKind::Invert,
        selection: None,
    });
    assert_eq!(crate::renderer::v3_gap(&invert), None);
    // Integer vector coverage and effects compose with pixel masks.
    let mut masked = mask.clone();
    masked.render_version = 3;
    for (reason, add) in [
        ("vector mask", &vector_mask),
        ("clip path", &clip_path),
        ("clip", &clip),
        ("effects", &effects),
    ] {
        let mut v3 = masked.clone();
        let f = &add.nodes[0].features;
        let n = &mut v3.nodes[0].features;
        n.vector_mask = f.vector_mask.clone();
        n.clip_path = f.clip_path.clone();
        n.clip = f.clip;
        n.effects = f.effects.clone();
        assert_eq!(crate::renderer::v3_gap(&v3), None, "mask + {reason}");
    }
}

pub(super) fn is_lavapipe_adapter(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("llvmpipe") || name.contains("lavapipe")
}

#[test]
fn lavapipe_adapter_names() {
    assert!(is_lavapipe_adapter("llvmpipe (LLVM 20.1.2, 256 bits)"));
    assert!(is_lavapipe_adapter("Lavapipe"));
    assert!(!is_lavapipe_adapter("Apple M4"));
}

#[test]
fn atlas_read_uses_effects_unorm_byte() {
    let q14 = include_str!("../../shaders/q14.wgsl");
    assert!(q14.contains(UNORM_BYTE_WGSL));
    let mut seen = [[false; 256]; 4];
    for i in 0..256 {
        for (ch, &b) in atlas_read_texel(i).iter().enumerate() {
            seen[ch][b as usize] = true;
        }
    }
    assert!(seen.iter().all(|c| c.iter().all(|&v| v)));
}

#[test]
fn alpha_measure_cases_are_not_enforced() {
    let all = cases();
    let measured: Vec<_> = all.iter().filter(|case| case.measure).collect();
    let enforced: Vec<_> = all.iter().filter(|case| !case.measure).collect();
    assert_eq!(measured.len(), enforced.len());
    assert!(measured.iter().all(|case| !case.lock));
    assert!(
        measured
            .iter()
            .all(|case| case.name.ends_with("-alpha-measure"))
    );
    let case = measured
        .iter()
        .find(|case| case.name == "extremes-alpha-measure")
        .expect("extremes variant");
    let image = match &case.nodes[0] {
        Node::Raster { image, .. } => image,
        Node::Group { .. } => panic!("extremes variant is a raster"),
    };
    let mut alphas = [false; 256];
    let mut reds = [false; 256];
    let mut greens = [false; 256];
    let mut blues = [false; 256];
    for y in 0..image.h {
        for x in 0..image.w {
            let i = ((y * image.w + x) * 4) as usize;
            reds[image.pixels[i] as usize] = true;
            greens[image.pixels[i + 1] as usize] = true;
            blues[image.pixels[i + 2] as usize] = true;
            alphas[image.pixels[i + 3] as usize] = true;
        }
    }
    assert!(alphas[0] && alphas[1] && alphas[128] && alphas[254]);
    assert!(
        reds.iter().all(|hit| *hit)
            && greens.iter().all(|hit| *hit)
            && blues.iter().all(|hit| *hit)
    );
    // Version 2 never locks them; the v3 lines are the integer composite.
    let locked = std::fs::read_to_string(reference_path()).unwrap();
    assert!(
        locked
            .lines()
            .all(|line| !line.contains("-alpha-measure") || line.starts_with("v3-"))
    );
    assert_blend_layers_cover_alpha_pairs(&measured);
}

pub(super) fn rasters_in_paint_order<'a>(nodes: &'a [Node], out: &mut Vec<&'a Image>) {
    for node in nodes {
        match node {
            Node::Raster { image, .. } => out.push(image),
            Node::Group { children, .. } => rasters_in_paint_order(children, out),
        }
    }
}

/// Blend, two-partial, and eight-layer sweeps must not paint src as a copy of
/// dst. Every consecutive pair covers all 16 alpha combinations, and RGB differs
/// at every pixel of that pair.
pub(super) fn assert_blend_layers_cover_alpha_pairs(measured: &[&Case]) {
    const ALPHAS: [u8; 4] = [0, 1, 128, 254];
    let mut checked = 0u32;
    for case in measured {
        let stacked = case.name.starts_with("blend-")
            || case.name.starts_with("two-partial")
            || case.name.starts_with("eight-layers")
            || case.name.starts_with("alpha-extremes");
        if !stacked {
            continue;
        }
        let mut images = Vec::new();
        rasters_in_paint_order(&case.nodes, &mut images);
        assert!(
            images.len() >= 2,
            "{} should stack at least two layers",
            case.name
        );
        for pair in images.windows(2) {
            let dst = pair[0];
            let src = pair[1];
            assert_eq!(dst.w, src.w, "{}", case.name);
            assert_eq!(dst.h, src.h, "{}", case.name);
            let mut pairs = std::collections::HashSet::new();
            let mut rgb_same = 0u32;
            for i in (0..dst.pixels.len()).step_by(4) {
                pairs.insert((dst.pixels[i + 3], src.pixels[i + 3]));
                if dst.pixels[i..i + 3] == src.pixels[i..i + 3] {
                    rgb_same += 1;
                }
            }
            assert_eq!(
                rgb_same, 0,
                "{} has pixels where src RGB equals dst",
                case.name
            );
            assert_eq!(pairs.len(), 16, "{} alpha pairs", case.name);
            for &dst_a in &ALPHAS {
                for &src_a in &ALPHAS {
                    assert!(
                        pairs.contains(&(dst_a, src_a)),
                        "{} missing dst alpha {dst_a}, src alpha {src_a}",
                        case.name
                    );
                }
            }
        }
        checked += 1;
    }
    assert!(checked >= 16, "blend sweeps were not checked");
}

#[test]
fn existing_v1_and_v2_hashes_are_unchanged() {
    let v2 = std::fs::read_to_string(reference_path()).unwrap();
    let v2_before = include_str!("../fixtures/cpu-reference-v2-before-alpha.txt");
    assert!(
        v2.starts_with(v2_before),
        "an existing v2 hash line changed by at least one character"
    );
    for line in v2[v2_before.len()..]
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        assert!(
            line.contains("-alpha-measure ")
                || line.starts_with("text-v3-")
                || line.starts_with("v3-"),
            "new hash line is not an alpha-measure, text-v3 or v3 case: {line}"
        );
    }
    let v1 = std::fs::read_to_string(render_v1_path()).unwrap();
    let v1_before = include_str!("../fixtures/render-v1-before-alpha.txt");
    assert_eq!(
        v1, v1_before,
        "an existing v1 hash changed by at least one character"
    );
}
