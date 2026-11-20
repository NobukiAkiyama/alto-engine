//! Reference blend for a Q14 input and its Gaussian-smoothed plane.
use super::q14::{Q, straight};

fn high_pass_q14(source: [i64; 4], blurred: [i64; 4]) -> [i64; 4] {
    let a = straight(source);
    let b = straight(blurred);
    let alpha = source[3];
    let mut output = [0, 0, 0, alpha];
    for channel in 0..3 {
        let straight = (a[channel] - b[channel] + Q / 2).clamp(0, Q);
        output[channel] = (straight * alpha + Q / 2) / Q;
    }
    output
}

#[test]
fn high_pass_of_flat_color_is_mid_gray() {
    let color = [1024, 5120, 3072, 8192];
    assert_eq!(high_pass_q14(color, color), [4096, 4096, 4096, 8192]);
}

#[test]
fn high_pass_keeps_alpha_and_transparent_pixels() {
    let source = [4096, 1024, 0, 8192];
    // Both planes have the same straight color, despite their differing alpha.
    let blurred = [2048, 512, 0, 4096];
    assert_eq!(high_pass_q14(source, blurred), [4096, 4096, 4096, 8192]);
    assert_eq!(high_pass_q14([0; 4], blurred), [0; 4]);
    assert_eq!(high_pass_q14(source, [0; 4]), [8192, 5120, 4096, 8192]);
}

#[test]
fn high_pass_of_straight_ramp_has_known_values() {
    let blurred = [8192, 8192, 8192, Q];
    assert_eq!(
        high_pass_q14([4096, 8192, 12288, Q], blurred),
        [4096, 8192, 12288, Q]
    );
    assert_eq!(
        high_pass_q14([0, 4096, Q, Q], [Q, 0, 0, Q]),
        [0, 12288, Q, Q]
    );
}

#[test]
fn high_pass_of_transparent_ramp_matches_renderer() {
    use super::{blur_pass_reference, scene, v3_test_render_with};
    use alto_core::{PixelFormat, TileKey};
    use alto_scene::{RContent, REffect, REffectKind, RFeatures};

    const W: usize = 256;
    const RADIUS: usize = 8;
    const PADDED: usize = W + 2 * RADIUS;
    let mut bytes = vec![0u8; W * W * 4];
    for y in 48..206 {
        for x in 48..206 {
            let offset = (y * W + x) * 4;
            bytes[offset..offset + 4].copy_from_slice(&[
                x as u8,
                y as u8,
                if x < 128 { 255 } else { 0 },
                if y < 128 { 255 } else { 127 },
            ]);
        }
    }
    let mut padded = vec![[0u16; 4]; PADDED * PADDED];
    for (i, rgba) in bytes.as_chunks::<4>().0.iter().enumerate() {
        let q = |value: u32, max: u32| ((value * 16384 + max / 2) / max) as u16;
        let alpha = u32::from(rgba[3]);
        padded[(i / W + RADIUS) * PADDED + (i % W + RADIUS)] = [
            q(u32::from(rgba[0]) * alpha, 255 * 255),
            q(u32::from(rgba[1]) * alpha, 255 * 255),
            q(u32::from(rgba[2]) * alpha, 255 * 255),
            q(alpha, 255),
        ];
    }
    let weights = crate::renderer::blur_kernel_q16(RADIUS as u32, (RADIUS * 256) as u32).unwrap();
    let horizontal = blur_pass_reference(
        &padded,
        [PADDED, PADDED],
        [W, PADDED],
        [RADIUS as i32, 0],
        false,
        &weights,
    );
    let blurred = blur_pass_reference(
        &horizontal,
        [W, PADDED],
        [W, W],
        [0, RADIUS as i32],
        true,
        &weights,
    );
    let reference: Vec<u8> = blurred
        .iter()
        .enumerate()
        .flat_map(|(i, blurred)| {
            let source = padded[(i / W + RADIUS) * PADDED + (i % W + RADIUS)];
            high_pass_q14(source.map(i64::from), blurred.map(i64::from))
                .map(|channel| ((channel * 255 + Q / 2) / Q) as u8)
        })
        .collect();

    let mut store = alto_raster::ObjectStore::default();
    let hash = store.insert_tile(&alto_raster::TileData::new(PixelFormat::Rgba8, bytes).unwrap());
    let mut document = scene(
        RContent::Tiles(vec![crate::RenderTile {
            key: TileKey { tx: 0, ty: 0 },
            hash,
        }]),
        RFeatures {
            effects: vec![REffect {
                kind: REffectKind::HighPass {
                    radius: RADIUS as f32,
                    radius_q8: (RADIUS * 256) as u32,
                },
                selection: None,
            }],
            ..RFeatures::default()
        },
    );
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    if let Some(gpu) = v3_test_render_with(&document, &store, (0, 0, 0)) {
        assert_eq!(gpu.len(), reference.len());
        if let Some(i) = gpu
            .as_chunks::<4>()
            .0
            .iter()
            .zip(reference.as_chunks::<4>().0)
            .position(|(actual, expected)| actual != expected)
        {
            let pixel = i * 4..i * 4 + 4;
            panic!(
                "high-pass pixel ({}, {}): GPU {:?}, reference {:?}",
                i % W,
                i / W,
                &gpu[pixel.clone()],
                &reference[pixel],
            );
        }
    }
}

#[test]
fn high_pass_coarsened_selection_keeps_fine_source_and_alpha() {
    use super::{scene, v3_test_render_with};
    use alto_core::{PixelFormat, TileKey};
    use alto_scene::{RContent, REffect, REffectKind, RFeatures, RMask};

    let mut store = alto_raster::ObjectStore::default();
    let key = TileKey { tx: 0, ty: 0 };
    let pixels = store.insert_tile(
        &alto_raster::TileData::new(PixelFormat::Rgba8, [72, 231, 17, 128].repeat(256 * 256))
            .unwrap(),
    );
    let mut mask = vec![0; 256 * 256];
    for row in mask.as_chunks_mut::<256>().0 {
        row[56..].fill(255);
    }
    let mask = store.insert_tile(&alto_raster::TileData::new(PixelFormat::R8, mask).unwrap());
    let mut document = scene(
        RContent::Tiles(vec![crate::RenderTile { key, hash: pixels }]),
        RFeatures {
            effects: vec![REffect {
                kind: REffectKind::HighPass {
                    radius: 8.0,
                    radius_q8: 2048,
                },
                selection: Some(RMask {
                    tiles: vec![crate::RenderTile { key, hash: mask }],
                    linked: true,
                    transform: super::ident(),
                }),
            }],
            ..RFeatures::default()
        },
    );
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    // This tile spans [48, 64) at 16x magnification. Radius 8 needs a coarser
    // blur grid, but the original and selection still use the fine grid.
    if let Some(bytes) = v3_test_render_with(&document, &store, (-4, 3, 3)) {
        for row in bytes.as_chunks::<{ 256 * 4 }>().0 {
            assert!(
                row[..128 * 4]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| *p == [36, 116, 9, 128])
            );
            assert!(
                row[128 * 4..]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| *p == [64, 64, 64, 128])
            );
        }
    }
}

#[test]
fn high_pass_keeps_nested_raster_and_adjustment_stacks_separate() {
    use super::{scene, v3_test_render_with};
    use alto_core::{PixelFormat, TileKey};
    use alto_scene::{RContent, REffect, REffectKind, RFeatures, RMask};

    let mut store = alto_raster::ObjectStore::default();
    let key = TileKey { tx: 0, ty: 0 };
    let mut add_tile = |format, bytes| crate::RenderTile {
        key,
        hash: store.insert_tile(&alto_raster::TileData::new(format, bytes).unwrap()),
    };
    let half_raster = |color: [u8; 4]| {
        let mut bytes = vec![0; 256 * 256 * 4];
        for row in bytes.as_chunks_mut::<{ 256 * 4 }>().0 {
            for pixel in row[..128 * 4].as_chunks_mut::<4>().0 {
                pixel.copy_from_slice(&color);
            }
        }
        bytes
    };
    let left = add_tile(PixelFormat::Rgba8, half_raster([255, 0, 0, 255]));
    let right = add_tile(PixelFormat::Rgba8, half_raster([0, 0, 255, 255]));
    let upper = add_tile(
        PixelFormat::R8,
        (0..256 * 256)
            .map(|i| if i / 256 < 128 { 255 } else { 0 })
            .collect(),
    );
    let lower_right = add_tile(
        PixelFormat::R8,
        (0..256 * 256)
            .map(|i| {
                if i / 256 >= 128 && i % 256 >= 128 {
                    255
                } else {
                    0
                }
            })
            .collect(),
    );
    let effect = |tile| REffect {
        kind: REffectKind::HighPass {
            radius: 8.0,
            radius_q8: 2048,
        },
        selection: Some(RMask {
            tiles: vec![tile],
            linked: true,
            transform: super::ident(),
        }),
    };
    let node = |content, effects| {
        scene(
            content,
            RFeatures {
                effects,
                ..RFeatures::default()
            },
        )
        .nodes
        .remove(0)
    };
    let group = |children| RContent::Group {
        children,
        isolated: true,
    };
    let mut document = scene(group(vec![1, 3, 4]), RFeatures::default());
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    let mut left_group = node(group(vec![2]), vec![]);
    left_group.transform[4] = -8.0;
    let left = node(RContent::Tiles(vec![left]), vec![effect(upper.clone())]);
    let mut right = node(RContent::Tiles(vec![right]), vec![effect(upper)]);
    right.transform[4] = 128.0;
    let adjustment = node(RContent::Adjustment(effect(lower_right)), vec![]);
    document.nodes.extend([left_group, left, right, adjustment]);

    if let Some(bytes) = v3_test_render_with(&document, &store, (0, 0, 0)) {
        let pixel = |x: usize, y: usize| &bytes[(y * 256 + x) * 4..][..4];
        assert_eq!(pixel(64, 64), &[128, 128, 128, 255]);
        assert_eq!(pixel(192, 64), &[128, 128, 128, 255]);
        assert_eq!(pixel(64, 192), &[255, 0, 0, 255]);
        assert_eq!(pixel(192, 192), &[128, 128, 128, 255]);
    }
}

#[test]
fn selected_blur_stack_renders_32_effects_at_mixed_sampling_levels() {
    use super::{scene, v3_test_try_render, v3_test_try_render_with_budget};
    use alto_core::{PixelFormat, TileKey, blur::blur_radius_q8_f32};
    use alto_scene::{RContent, REffect, REffectKind, RFeatures, RMask};

    const W: usize = 256;
    const EDGE: usize = 56;
    let mut rgba = Vec::with_capacity(W * W * 4);
    let mut mask = Vec::with_capacity(W * W);
    for _ in 0..W {
        for x in 0..W {
            let selected = x < EDGE;
            rgba.extend(if selected {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            });
            mask.push(if selected { 192 } else { 0 });
        }
    }
    let mut store = alto_raster::ObjectStore::default();
    let key = TileKey { tx: 0, ty: 0 };
    let pixel_hash =
        store.insert_tile(&alto_raster::TileData::new(PixelFormat::Rgba8, rgba).unwrap());
    let mask_hash = store.insert_tile(&alto_raster::TileData::new(PixelFormat::R8, mask).unwrap());
    let selection = RMask {
        tiles: vec![crate::RenderTile {
            key,
            hash: mask_hash,
        }],
        linked: true,
        transform: super::ident(),
    };
    // At scale 1/16 the largest radius is sampled one level coarser. Selected
    // blurs also need the original at the fine level.
    let effects = [0.0625, 0.125, 0.1875, 0.25, 0.375, 0.5, 0.75, 4.0625]
        .into_iter()
        .cycle()
        .take(32)
        .map(|radius| REffect {
            kind: REffectKind::GaussianBlur {
                radius,
                radius_q8: blur_radius_q8_f32(radius).unwrap(),
            },
            selection: Some(selection.clone()),
        })
        .collect();
    let mut document = scene(
        RContent::Tiles(vec![crate::RenderTile {
            key,
            hash: pixel_hash,
        }]),
        RFeatures {
            effects,
            ..RFeatures::default()
        },
    );
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    // The tile covers [48, 64) in both axes, crossing the edge at x = 56.
    let tile = (-4, 3, 3);
    if let Some(rendered) = v3_test_try_render(&document, &store, tile) {
        let bytes = rendered.expect("selected blurs should render with a 256 MiB GPU budget");
        let at = |x, y| (y * W + x) * 4;
        let filtered = at(127, 128);
        assert!(
            bytes[filtered] < 255 && bytes[filtered + 2] > 0,
            "selected edge should blur"
        );
        let untouched = at(128, 128);
        assert_eq!(&bytes[untouched..untouched + 4], &[0, 0, 255, 255]);
        let roomy = v3_test_try_render_with_budget(&document, &store, tile, 512)
            .expect("same headless adapter available")
            .expect("selected blurs should render with a 512 MiB GPU budget");
        assert!(
            bytes == roomy,
            "output depends on the GPU allocation budget"
        );
        match v3_test_try_render_with_budget(&document, &store, tile, 64)
            .expect("same headless adapter available")
        {
            Ok(restricted) => assert!(
                bytes == restricted,
                "output depends on the GPU allocation budget"
            ),
            Err(crate::RenderError::Budget(_)) => {}
            Err(other) => panic!("unexpected 64 MiB rendering error: {other}"),
        }
    }
}
