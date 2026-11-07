//! Mask, mask-origin and purge programs for the v3 pixel-mask stages.
use super::*;

/// An R8 mask as the atlas holds it (`rgba_pixels`: `[v, v, v, 255]`).
pub(super) fn mask_image(byte: impl Fn(u32, u32) -> u8) -> Vec<u8> {
    let mut pixels = vec![0u8; 256 * 256 * 4];
    for y in 0..256u32 {
        for x in 0..256u32 {
            let v = byte(x, y);
            let i = ((y * 256 + x) * 4) as usize;
            pixels[i..i + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    pixels
}

/// The bytes `selection_ops` writes for a feathered rectangle `[x0, y0, x1, y1]`:
/// f64 distance to the nearest edge, `(d / feather + 0.5).clamp(0, 1)`, then
/// `(coverage * 255).round()`. Every operation is IEEE-exact or correctly rounded.
pub(super) fn feathered_rectangle(rect: [f64; 4], feather: f64) -> Vec<u8> {
    mask_image(|x, y| {
        let p = [f64::from(x) + 0.5, f64::from(y) + 0.5];
        let d = (p[0] - rect[0])
            .min(rect[2] - p[0])
            .min(p[1] - rect[1])
            .min(rect[3] - p[1]);
        ((d / feather + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8
    })
}

/// The feathered selection of the mask cases (`selectionSpec` in the browser
/// suite). Feather 64 is the engine's limit, so one edge has only 65 bytes; the
/// edges sit at fractions 0.25, 0.75, 0.5 and 0 of a pixel so that the four
/// together give every byte.
pub(super) const FEATHER_RECT: [f64; 4] = [40.25, 40.75, 216.5, 216.0];
pub(super) const FEATHER: f64 = 64.0;

/// The mask rotated by 0.3 rad about the tile center, as an unlinked mask transform.
pub(super) fn rotated_mask_transform() -> [f64; 6] {
    let (s, c) = 0.3f64.sin_cos();
    [
        c,
        s,
        -s,
        c,
        128.0 - (c - s) * 128.0,
        128.0 - (s + c) * 128.0,
    ]
}

/// A unit mask moved by (2.501953125, -0.498046875) px. Both inverse
/// translations are Q16 exact (-163968 and 32640), and with Q8 pixel centers
/// `x * 256 + 128` the Q8 sums land exactly on `.5`: `x * 256 - 512.5` and
/// `y * 256 + 255.5`, the ties of `wide_round_shr16`. At x = 2 the negative tie
/// crosses into the tile at key -1. An f32 floor would pick the other texel at
/// every tie.
pub(super) fn tie_mask_transform() -> [f64; 6] {
    [1.0, 0.0, 0.0, 1.0, 2.501953125, -0.498046875]
}

/// Atlas layers for [`mask_programs`].
pub(super) fn mask_images() -> Vec<Vec<u8>> {
    let mut opaque = color_adjust_gradient();
    for px in opaque.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    vec![
        // 0: the color-adjust gradient (every channel value, alpha 0, 1, 128, 254).
        color_adjust_gradient(),
        // 1: every mask byte, x.
        mask_image(|x, _| x as u8),
        // 2: all 255.
        mask_image(|_, _| 255),
        // 3: a feathered rectangle selection with every byte 0..=255 at its edges.
        feathered_rectangle(FEATHER_RECT, FEATHER),
        // 4: the gradient with opaque alpha (an opaque backdrop).
        opaque,
        // 5: a pattern that shows where a rotated lookup lands.
        mask_image(|x, y| ((x * 7 + y * 13) & 255) as u8),
        // 6: half-alpha red, 7: opaque blue, 8: opaque red.
        [255, 0, 0, 128].repeat(256 * 256),
        [0, 0, 255, 255].repeat(256 * 256),
        [255, 0, 0, 255].repeat(256 * 256),
        // 9: the browser `mask` case: a rectangle selection of x < 128.
        mask_image(|x, _| if x < 128 { 255 } else { 0 }),
    ]
}

/// The memo's enforced mask cases as `renderer/tile_renderer/` dispatches them for
/// render version 3. A masked raster: mode 18, mode 21 (mask and opacity), then
/// mode 19 at opacity 1 onto the backdrop.
pub(super) fn mask_programs() -> Vec<Q14Program> {
    let vivid = (1.7, 1.6, 0.0);
    let masked = |name: String, image: u32, mask: u32, opacity: f32, transform| Q14Program {
        name,
        targets: 4,
        regions: vec![],
        stages: vec![
            Q14Stage::ingress(image, 1),
            Q14Stage::mask(1, mask, opacity, transform, 2),
            Q14Stage::layer(0, 2, 1.0, 0, 3),
        ],
        result: 3,
    };
    let mut out = vec![masked("v3-mask".into(), 8, 9, 1.0, ident())];
    for opacity in [1.0, 0.5, 0.35, 0.0] {
        out.push(masked(
            format!("m5-mask-coverage o={opacity}"),
            0,
            1,
            opacity,
            ident(),
        ));
    }
    for opacity in [1.0, 0.5, 0.35] {
        out.push(masked(
            format!("m5-mask-white o={opacity}"),
            0,
            2,
            opacity,
            ident(),
        ));
    }
    out.push(masked(
        "m5-mask-rotated".into(),
        0,
        5,
        1.0,
        rotated_mask_transform(),
    ));
    // The Q8 rounding tie, positive and negative, with the negative tie crossing
    // a tile boundary (`tie_mask_transform`).
    out.push(masked(
        "m5-mask-tie".into(),
        0,
        5,
        1.0,
        tie_mask_transform(),
    ));
    // A selection-limited color-adjust layer effect: mode 17, then mode 22.
    out.push(Q14Program {
        name: "m5-selection-color-adjust".into(),
        targets: 5,
        regions: vec![],
        stages: vec![
            Q14Stage::ingress(0, 1),
            Q14Stage::color_adjust(1, vivid, 2),
            Q14Stage::select(1, 2, 3, 3),
            Q14Stage::layer(0, 3, 1.0, 0, 4),
        ],
        result: 4,
    });
    // A masked color-adjust adjustment layer over an opaque backdrop (mode 23).
    for (blend, name) in [
        (0, "normal"),
        (1, "multiply"),
        (2, "screen"),
        (3, "overlay"),
    ] {
        for opacity in [1.0, 0.5] {
            out.push(Q14Program {
                name: format!("m5-adjustment-mask-{name} o={opacity}"),
                targets: 5,
                regions: vec![],
                stages: vec![
                    Q14Stage::ingress(4, 1),
                    Q14Stage::layer(0, 1, 1.0, 0, 2),
                    Q14Stage::color_adjust(2, vivid, 3),
                    Q14Stage::adjust(2, 3, Some(3), opacity, blend, 4),
                ],
                result: 4,
            });
        }
    }
    // Mode 23 without a mask, at the shader only: the host refuses this scene
    // (`v3_adjustment_blend`, `v3_gap`), so no renderer draws it. It keeps the
    // `settings.y == 0` branch of `mask_q` exact.
    out.push(Q14Program {
        name: "m5-adjustment-screen-unmasked".into(),
        targets: 5,
        regions: vec![],
        stages: vec![
            Q14Stage::ingress(4, 1),
            Q14Stage::layer(0, 1, 1.0, 0, 2),
            Q14Stage::color_adjust(2, vivid, 3),
            Q14Stage::adjust(2, 3, None, 0.5, 2, 4),
        ],
        result: 4,
    });
    // A masked group of semi-transparent children over opaque blue: the children
    // composite onto an empty target, mode 21 applies mask and opacity once, and
    // the group composites at opacity 1.
    out.push(Q14Program {
        name: "m5-mask-group".into(),
        targets: 9,
        regions: vec![],
        stages: vec![
            Q14Stage::ingress(7, 1),
            Q14Stage::layer(0, 1, 1.0, 0, 2),
            Q14Stage::ingress(6, 3),
            Q14Stage::layer(0, 3, 1.0, 0, 4),
            Q14Stage::ingress(0, 5),
            Q14Stage::layer(4, 5, 1.0, 1, 6),
            Q14Stage::mask(6, 1, 0.5, ident(), 7),
            Q14Stage::layer(2, 7, 1.0, 0, 8),
        ],
        result: 8,
    });
    out
}

/// Output ROIs `[x, y, level scale]` away from the origin, three of them
/// negative, each with a mask transform that puts the mask partly under the
/// tile, for [`mask_origin_programs`] and `tests/e2e/mask-v3-origin-reference.mjs`.
pub(super) fn mask_origin_cases() -> Vec<([i32; 3], [f64; 6])> {
    let r = rotated_mask_transform();
    vec![
        ([-300, 37, 1], [1.0, 0.0, 0.0, 1.0, -400.0, 0.0]),
        ([-256, -512, 2], [2.0, 0.0, 0.0, 2.0, -512.0, -600.0]),
        (
            [-37, -91, 1],
            [r[0], r[1], r[2], r[3], r[4] - 100.0, r[5] - 100.0],
        ),
        ([300, 200, 1], [1.0, 0.0, 0.0, 1.0, 250.0, 150.0]),
    ]
}

/// The input ROIs (`a`, `b`) of the offset programs for each case in
/// [`mask_origin_cases`]: shifted by whole pixels, some at another level
/// scale, so that `input_map` is not the identity and its floor division sees
/// negative numerators.
pub(super) fn mask_offset_regions() -> Vec<([i32; 3], [i32; 3])> {
    vec![
        ([-295, 34, 1], [-307, 40, 1]),
        ([-251, -509, 1], [-260, -520, 4]),
        ([-40, -88, 2], [-33, -95, 1]),
        ([303, 197, 1], [290, 210, 2]),
    ]
}

/// Modes 21, 22 and 23 at the ROIs of [`mask_origin_cases`]. For each case,
/// three programs whose targets all share the output ROI (`origin ...`, the
/// graphs `renderM5V3` draws at that origin) and three whose inputs sit at the
/// ROIs of [`mask_offset_regions`] (`offset ...`). The mask is the pattern in
/// atlas layer 5.
pub(super) fn mask_origin_programs() -> Vec<Q14Program> {
    let vivid = (1.7, 1.6, 0.0);
    let mut out = vec![];
    for ((region, t), (a, b)) in mask_origin_cases().into_iter().zip(mask_offset_regions()) {
        let at = |targets: usize| vec![region; targets];
        let tag = format!("{region:?}");
        out.push(Q14Program {
            name: format!("origin {tag} mask o=0.5"),
            targets: 4,
            regions: at(4),
            stages: vec![
                Q14Stage::ingress(0, 1),
                Q14Stage::mask(1, 5, 0.5, t, 2),
                Q14Stage::layer(0, 2, 1.0, 0, 3),
            ],
            result: 3,
        });
        out.push(Q14Program {
            name: format!("origin {tag} selection"),
            targets: 5,
            regions: at(5),
            stages: vec![
                Q14Stage::ingress(0, 1),
                Q14Stage::color_adjust(1, vivid, 2),
                Q14Stage {
                    transform: t,
                    ..Q14Stage::select(1, 2, 5, 3)
                },
                Q14Stage::layer(0, 3, 1.0, 0, 4),
            ],
            result: 4,
        });
        out.push(Q14Program {
            name: format!("origin {tag} adjustment screen o=0.5"),
            targets: 5,
            regions: at(5),
            stages: vec![
                Q14Stage::ingress(4, 1),
                Q14Stage::layer(0, 1, 1.0, 0, 2),
                Q14Stage::color_adjust(2, vivid, 3),
                Q14Stage {
                    transform: t,
                    ..Q14Stage::adjust(2, 3, Some(5), 0.5, 2, 4)
                },
            ],
            result: 4,
        });
        out.push(Q14Program {
            name: format!("offset {tag} mask o=0.5"),
            targets: 4,
            regions: vec![region, a, region, region],
            stages: vec![
                Q14Stage::ingress(0, 1),
                Q14Stage::mask(1, 5, 0.5, t, 2),
                Q14Stage::layer(0, 2, 1.0, 0, 3),
            ],
            result: 3,
        });
        out.push(Q14Program {
            name: format!("offset {tag} selection"),
            targets: 6,
            regions: vec![region, a, b, b, region, region],
            stages: vec![
                Q14Stage::ingress(0, 1),
                Q14Stage::ingress(0, 2),
                Q14Stage::color_adjust(2, vivid, 3),
                Q14Stage {
                    transform: t,
                    ..Q14Stage::select(1, 3, 5, 4)
                },
                Q14Stage::layer(0, 4, 1.0, 0, 5),
            ],
            result: 5,
        });
        out.push(Q14Program {
            name: format!("offset {tag} adjustment screen o=0.5"),
            targets: 5,
            regions: vec![region, a, b, b, region],
            stages: vec![
                Q14Stage::ingress(4, 1),
                Q14Stage::ingress(4, 2),
                Q14Stage::color_adjust(2, vivid, 3),
                Q14Stage {
                    transform: t,
                    ..Q14Stage::adjust(1, 3, Some(5), 0.5, 2, 4)
                },
            ],
            result: 4,
        });
    }
    out
}

/// Output ROIs `[x, y, level scale]` for [`purge_programs`]: the origin, a
/// negative one, near 10^6 px, the corners next to ±2^23 px (the last Q8 pixel
/// centers that fit i32, once the inputs are moved by up to -7 px) and a
/// level scale of 4.
pub(super) fn purge_regions() -> Vec<[i32; 3]> {
    vec![
        [0, 0, 1],
        [-300, 37, 1],
        [999_936, -1_024, 1],
        [8_388_352, -8_388_592, 1],
        [1_048_576, -2_048, 4],
    ]
}

/// The float purge (items 9 and 10) on the Q14 harness: modes 18, 17, 19, 8
/// and 20 at each of [`purge_regions`], with every input at another ROI (the
/// integer texel map) and rasters placed as they are or through a rotation, a
/// 0.8x scale and the Q8 rounding tie (the Q16 nearest lookup of mode 18).
pub(super) fn purge_programs() -> Vec<Q14Program> {
    let vivid = (1.7, 1.6, 0.0);
    let (s, c) = 0.3f64.sin_cos();
    let rotated = [
        c,
        s,
        -s,
        c,
        128.0 - (c - s) * 128.0,
        128.0 - (s + c) * 128.0,
    ];
    let scaled = [0.8, 0.0, 0.0, 0.8, 25.6, 25.6];
    let mut out = vec![];
    for region in purge_regions() {
        let moved = |dx: i32, dy: i32| {
            [
                region[0] + dx * region[2],
                region[1] + dy * region[2],
                region[2],
            ]
        };
        // The raster's document transform: `local` in the tile's own pixels.
        let world = |local| crate::renderer::multiply(placement(region), local);
        let tag = format!("{region:?}");
        out.push(Q14Program {
            name: format!("purge {tag} raster, color-adjust, multiply o=0.35, crop, mix o=0.5"),
            targets: 7,
            regions: vec![
                region,
                region,
                moved(-3, -5),
                region,
                moved(-7, -2),
                region,
                moved(-1, -1),
            ],
            stages: vec![
                Q14Stage::ingress(4, 1),
                Q14Stage::raster(0, world(rotated), 2),
                Q14Stage::color_adjust(2, vivid, 3),
                Q14Stage::layer(1, 3, 0.35, 1, 4),
                Q14Stage {
                    mode: 8,
                    ..Q14Stage::color_adjust(4, vivid, 5)
                },
                Q14Stage::adjustment_mix(1, 5, 0.5, 6),
            ],
            result: 6,
        });
        for (name, local) in [("scaled", scaled), ("tie", tie_mask_transform())] {
            out.push(Q14Program {
                name: format!("purge {tag} raster {name}, color-adjust o=0.5"),
                targets: 4,
                regions: vec![region, moved(-2, -1), region, region],
                stages: vec![
                    Q14Stage::raster(0, world(local), 1),
                    Q14Stage::color_adjust(1, vivid, 2),
                    Q14Stage::layer(0, 2, 0.5, 0, 3),
                ],
                result: 3,
            });
        }
    }
    out
}
