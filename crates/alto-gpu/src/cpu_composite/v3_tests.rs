//! Render version 3 vector, blur, clipping and adjustment parity through the Renderer.
use super::*;

pub(super) fn v3_vector_test_render_at(
    document: &RenderScene,
    key: (i32, i32, i32),
) -> Option<Vec<u8>> {
    v3_test_render_with(document, &alto_raster::ObjectStore::default(), key)
}
pub(super) fn v3_test_render_with(
    document: &RenderScene,
    store: &alto_raster::ObjectStore,
    key: (i32, i32, i32),
) -> Option<Vec<u8>> {
    v3_test_try_render(document, store, key).map(|r| r.expect("v3 vector tile"))
}
pub(super) fn v3_test_try_render(
    document: &RenderScene,
    store: &alto_raster::ObjectStore,
    key: (i32, i32, i32),
) -> Option<Result<Vec<u8>, crate::RenderError>> {
    let _gpu_lock = GPU_RENDERER_TEST_LOCK.lock().unwrap();
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::METAL | wgpu::Backends::VULKAN | wgpu::Backends::DX12;
    let instance = wgpu::Instance::new(descriptor);
    let mut renderer = match pollster::block_on(crate::renderer::Renderer::headless(&instance, 256))
    {
        Ok(renderer) => renderer,
        Err(err)
            if !cfg!(target_os = "macos")
                && std::env::var_os("ALTO_REQUIRE_GPU_PARITY").is_none() =>
        {
            eprintln!("v3 vector GPU parity skipped: {err}");
            return None;
        }
        Err(err) => panic!("v3 vector GPU parity could not start: {err}"),
    };
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let poller = {
        let (device, done) = (renderer.device.clone(), done.clone());
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                let _ = device.poll(wgpu::PollType::Poll);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    let rendered = pollster::block_on(renderer.read_tile(document, store, key));
    done.store(true, std::sync::atomic::Ordering::Release);
    poller.join().unwrap();
    Some(rendered)
}
pub(super) fn v3_vector_test_render(document: &RenderScene) -> Option<Vec<u8>> {
    v3_vector_test_render_at(document, (0, 0, 0))
}
pub(super) fn blur_pass_reference(
    input: &[[u16; 4]],
    input_size: [usize; 2],
    output_size: [usize; 2],
    offset: [i32; 2],
    vertical: bool,
    weights: &[u32],
) -> Vec<[u16; 4]> {
    let mut output = vec![[0u16; 4]; output_size[0] * output_size[1]];
    let radius = (weights.len() - 1) as i32;
    for y in 0..output_size[1] {
        for x in 0..output_size[0] {
            let mut sum = [0u32; 4];
            for i in -radius..=radius {
                let tx = x as i32 + offset[0] + if vertical { 0 } else { i };
                let ty = y as i32 + offset[1] + if vertical { i } else { 0 };
                if tx < 0 || ty < 0 || tx >= input_size[0] as i32 || ty >= input_size[1] as i32 {
                    continue;
                }
                let texel = input[ty as usize * input_size[0] + tx as usize];
                for channel in 0..4 {
                    sum[channel] += u32::from(texel[channel]) * weights[i.unsigned_abs() as usize];
                }
            }
            output[y * output_size[0] + x] = sum.map(|v| ((v + 32768) >> 16) as u16);
        }
    }
    output
}

pub(super) fn test_rectangle(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    fill: [f32; 4],
) -> alto_vector::StyledPath {
    use alto_vector::PathCommand as P;
    alto_vector::StyledPath {
        data: PathData {
            commands: vec![
                P::Move { x: x0, y: y0 },
                P::Line { x: x1, y: y0 },
                P::Line { x: x1, y: y1 },
                P::Line { x: x0, y: y1 },
                P::Close,
            ],
        },
        fill: Some(fill),
        stroke: None,
        fill_rule: FillRule::NonZero,
    }
}

pub(super) fn test_stroke(cap: alto_vector::LineCap) -> alto_vector::StyledPath {
    alto_vector::StyledPath {
        data: PathData {
            commands: vec![
                alto_vector::PathCommand::Move { x: 40.0, y: 64.0 },
                alto_vector::PathCommand::Line { x: 120.0, y: 64.0 },
            ],
        },
        fill: None,
        stroke: Some(alto_vector::Stroke {
            color: [1.0, 0.0, 0.0, 1.0],
            width: 8.0,
            cap,
            join: alto_vector::LineJoin::Miter,
            miter_limit: 4.0,
        }),
        fill_rule: FillRule::NonZero,
    }
}

#[test]
fn v3_stroke_caps_and_nonuniform_transform_reach_metal() {
    for (cap, end_inside) in [
        (alto_vector::LineCap::Butt, false),
        (alto_vector::LineCap::Round, true),
        (alto_vector::LineCap::Square, true),
    ] {
        let mut document = scene(
            RContent::Mesh(std::sync::Arc::new(vec![test_stroke(cap)])),
            RFeatures::default(),
        );
        document.render_version = alto_core::RENDER_VERSION_EXACT;
        document.nodes[0].transform = [1.0, 0.0, 0.0, 2.0, 0.0, 0.0];
        assert_eq!(crate::renderer::v3_gap(&document), None);
        let Some(image) = v3_vector_test_render(&document) else {
            return;
        };
        let pixel =
            |x: usize, y: usize| -> [u8; 4] { image[(y * 256 + x) * 4..][..4].try_into().unwrap() };
        assert_eq!(pixel(80, 128), [255, 0, 0, 255], "{cap:?}");
        assert_eq!(pixel(80, 122), [255, 0, 0, 255], "{cap:?}");
        assert_eq!(pixel(80, 119), [0, 0, 0, 0], "{cap:?}");
        assert_eq!(pixel(38, 128)[3] != 0, end_inside, "{cap:?}");
        assert_eq!(pixel(35, 128), [0, 0, 0, 0], "{cap:?}");
    }
}

#[test]
fn v3_stroke_join_styles_reach_metal() {
    for (join, round_pixel, miter_pixel) in [
        (alto_vector::LineJoin::Bevel, false, false),
        (alto_vector::LineJoin::Round, true, false),
        (alto_vector::LineJoin::Miter, true, true),
    ] {
        let mut path = test_stroke(alto_vector::LineCap::Butt);
        path.data.commands = vec![
            alto_vector::PathCommand::Move { x: 40.0, y: 80.0 },
            alto_vector::PathCommand::Line { x: 80.0, y: 80.0 },
            alto_vector::PathCommand::Line { x: 80.0, y: 40.0 },
        ];
        let stroke = path.stroke.as_mut().unwrap();
        stroke.width = 16.0;
        stroke.join = join;
        let mut document = scene(
            RContent::Mesh(std::sync::Arc::new(vec![path])),
            RFeatures::default(),
        );
        document.render_version = alto_core::RENDER_VERSION_EXACT;
        let Some(image) = v3_vector_test_render(&document) else {
            return;
        };
        let alpha = |x: usize, y: usize| image[(y * 256 + x) * 4 + 3];
        assert_eq!(alpha(84, 84) == 255, round_pixel, "{join:?}");
        assert_eq!(alpha(86, 86) == 255, miter_pixel, "{join:?}");
    }
}

#[test]
fn v3_stroke_paints_after_the_fill() {
    let mut path = test_rectangle(80.0, 80.0, 160.0, 160.0, [0.0, 0.0, 1.0, 1.0]);
    path.stroke = Some(alto_vector::Stroke {
        color: [1.0, 0.0, 0.0, 1.0],
        width: 8.0,
        cap: alto_vector::LineCap::Butt,
        join: alto_vector::LineJoin::Miter,
        miter_limit: 4.0,
    });
    let mut document = scene(
        RContent::Mesh(std::sync::Arc::new(vec![path])),
        RFeatures::default(),
    );
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    let Some(image) = v3_vector_test_render(&document) else {
        return;
    };
    let pixel =
        |x: usize, y: usize| -> [u8; 4] { image[(y * 256 + x) * 4..][..4].try_into().unwrap() };
    assert_eq!(pixel(120, 120), [0, 0, 255, 255]);
    assert_eq!(pixel(80, 120), [255, 0, 0, 255]);
    assert_eq!(pixel(70, 120), [0, 0, 0, 0]);
}

#[test]
fn v3_curved_stroke_matches_the_integer_reference() {
    let mut path = test_stroke(alto_vector::LineCap::Round);
    path.data.commands = vec![
        alto_vector::PathCommand::Move { x: 48.0, y: 128.0 },
        alto_vector::PathCommand::Cubic {
            c1x: 80.0,
            c1y: 16.0,
            c2x: 176.0,
            c2y: 240.0,
            x: 208.0,
            y: 128.0,
        },
    ];
    let stroke = path.stroke.as_mut().unwrap();
    stroke.color = [0.25, 0.5, 1.0, 0.5];
    stroke.width = 12.0;
    stroke.join = alto_vector::LineJoin::Round;
    let coverage = crate::stroke_q8::rasterize_stroke(
        &path.data,
        stroke,
        alto_core::FixedAffine::IDENTITY,
        0,
        [0, 0, 256, 256],
    )
    .unwrap();
    let color = crate::raster_q8::color_q14(stroke.color).unwrap();
    let expected: Vec<u8> = coverage
        .into_iter()
        .flat_map(|k| {
            crate::raster_q8::source_over_q14([0; 4], color, k)
                .map(|v| ((u32::from(v) * 255 + 8192) >> 14) as u8)
        })
        .collect();
    let mut document = scene(
        RContent::Mesh(std::sync::Arc::new(vec![path])),
        RFeatures::default(),
    );
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    let Some(image) = v3_vector_test_render(&document) else {
        return;
    };
    assert_eq!(image, expected);
}

#[test]
fn v3_vector_fill_reaches_metal_and_matches_q14_reference() {
    let rectangle = test_rectangle(32.5, 40.0, 96.5, 100.0, [1.0, 0.0, 0.0, 0.5]);
    let mut document = scene(
        RContent::Mesh(std::sync::Arc::new(vec![rectangle])),
        RFeatures::default(),
    );
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    assert_eq!(crate::renderer::v3_gap(&document), None);
    let Some(image) = v3_vector_test_render(&document) else {
        return;
    };
    let pixel = |x: usize, y: usize| -> [u8; 4] {
        let i = (y * 256 + x) * 4;
        image[i..i + 4].try_into().unwrap()
    };
    for (x, y, coverage) in [
        (0, 0, 0),
        (32, 50, 128),
        (33, 50, 255),
        (96, 50, 128),
        (97, 50, 0),
        (33, 101, 0),
    ] {
        let q14 = crate::raster_q8::source_over_q14([0; 4], [16384, 0, 0, 8192], coverage);
        let byte = q14.map(|v| ((u32::from(v) * 255 + 8192) >> 14) as u8);
        assert_eq!(pixel(x, y), byte, "pixel ({x},{y})");
    }
}

#[test]
fn v3_vector_mask_and_clip_multiply_before_masking_source() {
    let source = test_rectangle(0.0, 0.0, 256.0, 256.0, [1.0, 0.0, 0.0, 1.0]);
    let mut document = scene(
        RContent::Mesh(std::sync::Arc::new(vec![source])),
        RFeatures {
            vector_mask: Some(VectorMask {
                path: test_rectangle(32.5, 40.0, 96.5, 100.0, [1.0; 4]).data,
                fill_rule: FillRule::NonZero,
                feather: 0.0,
                enabled: true,
                transform: Affine2::default(),
            }),
            clip_path: Some(ClipPath {
                path: test_rectangle(64.5, 40.0, 128.5, 100.0, [1.0; 4]).data,
                fill_rule: FillRule::NonZero,
                enabled: true,
                transform: Affine2::default(),
            }),
            ..RFeatures::default()
        },
    );
    document.nodes[0].opacity = 0.5;
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    assert_eq!(crate::renderer::v3_gap(&document), None);
    let Some(image) = v3_vector_test_render(&document) else {
        return;
    };
    let pixel = |x: usize, y: usize| -> [u8; 4] {
        let i = (y * 256 + x) * 4;
        image[i..i + 4].try_into().unwrap()
    };
    for (x, mask, clip) in [
        (31, 0u8, 0u8),
        (32, 128, 0),
        (64, 255, 128),
        (65, 255, 255),
        (95, 255, 255),
        (96, 128, 255),
        (97, 0, 255),
    ] {
        let k_mask = (u32::from(mask) * 16384 + 127) / 255;
        let k_clip = (u32::from(clip) * 16384 + 127) / 255;
        let k = (k_mask * k_clip + 8192) >> 14;
        let q14 = ((8192 * k + 8192) >> 14) as u16;
        let byte = ((u32::from(q14) * 255 + 8192) >> 14) as u8;
        assert_eq!(pixel(x, 50), [byte, 0, 0, byte], "pixel {x}");
    }
}

#[test]
fn v3_vector_fill_rules_and_paint_order_match_all_cpu_pixels() {
    let red = test_rectangle(15.5, 17.5, 178.5, 190.5, [1.0, 0.0, 0.0, 0.5]);
    let green = test_rectangle(70.25, 35.75, 221.5, 205.25, [0.0, 1.0, 0.0, 0.7]);
    let mut hole = test_rectangle(100.5, 60.5, 240.5, 230.5, [0.0, 0.0, 1.0, 0.8]);
    hole.data.commands.extend(
        test_rectangle(140.5, 100.5, 200.5, 170.5, [0.0; 4])
            .data
            .commands,
    );
    hole.fill_rule = FillRule::EvenOdd;
    let paths = vec![red, green, hole];
    let mut document = scene(
        RContent::Mesh(std::sync::Arc::new(paths.clone())),
        RFeatures::default(),
    );
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    let Some(gpu) = v3_vector_test_render(&document) else {
        return;
    };
    let mut expected = vec![0u8; 256 * 256 * 4];
    let mut accum = vec![[0u16; 4]; 256 * 256];
    for path in &paths {
        let commands =
            crate::raster_q8::map_path(&path.data, alto_core::FixedAffine::IDENTITY, 0).unwrap();
        let rule = match path.fill_rule {
            FillRule::NonZero => crate::raster_q8::FillRule::NonZero,
            FillRule::EvenOdd => crate::raster_q8::FillRule::EvenOdd,
        };
        let coverage =
            crate::raster_q8::rasterize(&commands, rule, [0, 0, 256, 256], 4096).unwrap();
        let color = crate::raster_q8::color_q14(path.fill.unwrap()).unwrap();
        for (out, k) in accum.iter_mut().zip(coverage) {
            *out = crate::raster_q8::source_over_q14(*out, color, k);
        }
    }
    for (dst, q14) in expected.as_chunks_mut::<4>().0.iter_mut().zip(accum) {
        dst.copy_from_slice(&q14.map(|v| ((u32::from(v) * 255 + 8192) >> 14) as u8));
    }
    assert_eq!(gpu, expected);
}

#[test]
fn v3_blur_one_pixel_matches_integer_reference() {
    let path = test_rectangle(32.5, 40.0, 96.5, 100.0, [1.0, 0.0, 0.0, 1.0]);
    let mut document = scene(
        RContent::Mesh(std::sync::Arc::new(vec![path.clone()])),
        RFeatures {
            effects: vec![REffect {
                kind: REffectKind::GaussianBlur {
                    radius: 1.0,
                    radius_q8: 256,
                },
                selection: None,
            }],
            ..RFeatures::default()
        },
    );
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    assert_eq!(crate::renderer::v3_gap(&document), None);
    let Some(gpu) = v3_vector_test_render(&document) else {
        return;
    };
    let path_q =
        crate::raster_q8::map_path(&path.data, alto_core::FixedAffine::IDENTITY, 0).unwrap();
    let coverage = crate::raster_q8::rasterize(
        &path_q,
        crate::raster_q8::FillRule::NonZero,
        [-1, -1, 258, 258],
        4096,
    )
    .unwrap();
    let source: Vec<[u16; 4]> = coverage
        .into_iter()
        .map(|k| crate::raster_q8::source_over_q14([0; 4], [16384, 0, 0, 16384], k))
        .collect();
    let weight = crate::renderer::blur_kernel_q16(1, 256).unwrap();
    let mut middle = vec![[0u16; 4]; 256 * 258];
    for y in 0..258usize {
        for x in 0..256usize {
            for channel in 0..4 {
                let value = u32::from(source[y * 258 + x][channel]) * weight[1]
                    + u32::from(source[y * 258 + x + 1][channel]) * weight[0]
                    + u32::from(source[y * 258 + x + 2][channel]) * weight[1];
                middle[y * 256 + x][channel] = ((value + 32768) >> 16) as u16;
            }
        }
    }
    let mut expected = vec![0u8; 256 * 256 * 4];
    for y in 0..256usize {
        for x in 0..256usize {
            for channel in 0..4 {
                let value = u32::from(middle[y * 256 + x][channel]) * weight[1]
                    + u32::from(middle[(y + 1) * 256 + x][channel]) * weight[0]
                    + u32::from(middle[(y + 2) * 256 + x][channel]) * weight[1];
                let q14 = (value + 32768) >> 16;
                expected[(y * 256 + x) * 4 + channel] = ((q14 * 255 + 8192) >> 14) as u8;
            }
        }
    }
    assert_eq!(gpu, expected);
}

#[test]
fn v3_blur_coarsen_and_resample_match_integer_reference() {
    let path = test_rectangle(0.0, 0.0, 256.0, 256.0, [1.0, 0.0, 0.0, 1.0]);
    let mut document = scene(
        RContent::Mesh(std::sync::Arc::new(vec![path.clone()])),
        RFeatures {
            effects: vec![REffect {
                kind: REffectKind::GaussianBlur {
                    radius: 32.0,
                    radius_q8: 8192,
                },
                selection: None,
            }],
            ..RFeatures::default()
        },
    );
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    let Some(gpu) = v3_vector_test_render_at(&document, (-2, 0, 0)) else {
        return;
    };
    let commands =
        crate::raster_q8::map_path(&path.data, alto_core::FixedAffine::IDENTITY, -1).unwrap();
    let coverage = crate::raster_q8::rasterize(
        &commands,
        crate::raster_q8::FillRule::NonZero,
        [-65, -65, 258, 258],
        4096,
    )
    .unwrap();
    let source: Vec<[u16; 4]> = coverage
        .into_iter()
        .map(|k| crate::raster_q8::source_over_q14([0; 4], [16384, 0, 0, 16384], k))
        .collect();
    let weights = crate::renderer::blur_kernel_q16(64, 16384).unwrap();
    let horizontal = blur_pass_reference(&source, [258, 258], [130, 258], [64, 0], false, &weights);
    let blurred = blur_pass_reference(&horizontal, [130, 258], [130, 130], [0, 64], true, &weights);
    let mut expected = vec![0u8; 256 * 256 * 4];
    for y in 0..256usize {
        for x in 0..256usize {
            let ux = 2 * (x + 2) + 1 - 2;
            let uy = 2 * (y + 2) + 1 - 2;
            let (ix, iy) = (ux >> 2, uy >> 2);
            let (fx, fy) = ((ux & 3) << 6, (uy & 3) << 6);
            let a = blurred[iy * 130 + ix];
            let b = blurred[iy * 130 + ix + 1];
            let d = blurred[(iy + 1) * 130 + ix];
            let e = blurred[(iy + 1) * 130 + ix + 1];
            for channel in 0..4 {
                let ab =
                    u32::from(a[channel]) * (256 - fx as u32) + u32::from(b[channel]) * fx as u32;
                let de =
                    u32::from(d[channel]) * (256 - fx as u32) + u32::from(e[channel]) * fx as u32;
                let q14 = (ab * (256 - fy as u32) + de * fy as u32 + 32768) >> 16;
                expected[(y * 256 + x) * 4 + channel] = ((q14 * 255 + 8192) >> 14) as u8;
            }
        }
    }
    assert_eq!(gpu, expected);
}

#[test]
fn blur_pass_order_is_horizontal_first() {
    let w = crate::renderer::blur_kernel_q16(1, 256).unwrap();
    assert_eq!(w, vec![64112, 712]);
    let rows = [
        [13576u16, 5669, 12029],
        [12278, 2833, 14383],
        [3536, 5364, 12886],
    ];
    let input: Vec<[u16; 4]> = rows.as_flattened().iter().map(|&v| [v; 4]).collect();
    let transposed: Vec<[u16; 4]> = (0..9).map(|i| input[(i % 3) * 3 + i / 3]).collect();
    let pass = |input: &[[u16; 4]], vertical| {
        blur_pass_reference(input, [3, 3], [3, 3], [0, 0], vertical, &w)
    };
    let horizontal_first = pass(&pass(&input, false), true);
    let vertical_first = pass(&pass(&input, true), false);
    assert_eq!(horizontal_first[4][0], 3117);
    assert_eq!(vertical_first[4][0], 3116);
    // Transposing the input swaps the two orders.
    assert_eq!(pass(&pass(&transposed, true), false)[4][0], 3117);
}

#[test]
fn v3_raster_blur_square_matches_integer_reference() {
    use alto_core::{PixelFormat, TileKey};
    // `blur-v3-square`: opaque red on transparent, 80..180, radius 8, level 0.
    let mut bytes = vec![0u8; 256 * 256 * 4];
    for y in 80..180 {
        for x in 80..180 {
            bytes[(y * 256 + x) * 4..][..4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    let mut store = alto_raster::ObjectStore::default();
    let hash = store.insert_tile(&alto_raster::TileData::new(PixelFormat::Rgba8, bytes).unwrap());
    let mut document = scene(
        RContent::Tiles(vec![crate::RenderTile {
            key: TileKey { tx: 0, ty: 0 },
            hash,
        }]),
        RFeatures {
            effects: vec![REffect {
                kind: REffectKind::GaussianBlur {
                    radius: 8.0,
                    radius_q8: 2048,
                },
                selection: None,
            }],
            ..RFeatures::default()
        },
    );
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    assert_eq!(crate::renderer::v3_gap(&document), None);
    let Some(gpu) = v3_test_render_with(&document, &store, (0, 0, 0)) else {
        return;
    };
    // The layer as an infinite plane (transparent outside), blurred, cropped.
    let input: Vec<[u16; 4]> = (0..272 * 272)
        .map(|i| {
            let (x, y) = (i % 272 - 8, i / 272 - 8);
            if (80..180).contains(&x) && (80..180).contains(&y) {
                [16384, 0, 0, 16384]
            } else {
                [0; 4]
            }
        })
        .collect();
    let weights = crate::renderer::blur_kernel_q16(8, 2048).unwrap();
    let middle = blur_pass_reference(&input, [272, 272], [256, 272], [8, 0], false, &weights);
    let blurred = blur_pass_reference(&middle, [256, 272], [256, 256], [0, 8], true, &weights);
    let expected: Vec<u8> = blurred
        .iter()
        .flat_map(|q| q.map(|v| ((u32::from(v) * 255 + 8192) >> 14) as u8))
        .collect();
    assert_eq!(gpu, expected);
}

/// A v3 group of the opaque red square 80..180 (raster) and `top` above it.
pub(super) fn v3_square_under(top: RNode) -> (RenderScene, alto_raster::ObjectStore) {
    v3_colored_square_under(top, [255, 0, 0, 255])
}

pub(super) fn v3_colored_square_under(
    top: RNode,
    color: [u8; 4],
) -> (RenderScene, alto_raster::ObjectStore) {
    use alto_core::{PixelFormat, TileKey};
    let mut bytes = vec![0u8; 256 * 256 * 4];
    for y in 80..180 {
        for x in 80..180 {
            bytes[(y * 256 + x) * 4..][..4].copy_from_slice(&color);
        }
    }
    let mut store = alto_raster::ObjectStore::default();
    let hash = store.insert_tile(&alto_raster::TileData::new(PixelFormat::Rgba8, bytes).unwrap());
    let mut document = scene(
        RContent::Group {
            children: vec![1, 2],
            isolated: false,
        },
        RFeatures::default(),
    );
    let mut square = document.nodes[0].clone();
    square.key = NodeId::new();
    square.content = RContent::Tiles(vec![crate::RenderTile {
        key: TileKey { tx: 0, ty: 0 },
        hash,
    }]);
    document.nodes.push(square);
    document.nodes.push(top);
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    (document, store)
}

pub(super) fn v3_adjustment(kind: REffectKind, features: RFeatures) -> RNode {
    let mut node = scene(
        RContent::Adjustment(REffect {
            kind,
            selection: None,
        }),
        features,
    )
    .nodes
    .remove(0);
    node.key = NodeId::new();
    node
}

#[test]
fn v3_clipped_layer_keeps_base_coverage_and_applies_opacity() {
    let mut top = scene(
        RContent::Mesh(std::sync::Arc::new(vec![test_rectangle(
            0.0,
            0.0,
            256.0,
            256.0,
            [0.0, 1.0, 0.0, 0.5],
        )])),
        RFeatures {
            clip: true,
            ..RFeatures::default()
        },
    )
    .nodes
    .remove(0);
    top.key = NodeId::new();
    top.opacity = 0.5;
    let (document, store) = v3_square_under(top);
    assert_eq!(crate::renderer::v3_gap(&document), None);
    let Some(image) = v3_test_render_with(&document, &store, (0, 0, 0)) else {
        return;
    };
    let pixel =
        |x: usize, y: usize| -> [u8; 4] { image[(y * 256 + x) * 4..][..4].try_into().unwrap() };
    assert_eq!(pixel(50, 50), [0, 0, 0, 0]);
    assert_eq!(pixel(100, 100), [191, 64, 0, 255]);
}

#[test]
fn v3_clipping_chain_keeps_translucent_base_alpha() {
    let mut document = scene(
        RContent::Group {
            children: vec![1, 2, 3],
            isolated: false,
        },
        RFeatures::default(),
    );
    for (fill, clip) in [
        ([1.0, 0.0, 0.0, 0.5], false),
        ([0.0, 1.0, 0.0, 0.5], true),
        ([0.0, 0.0, 1.0, 1.0], true),
    ] {
        let mut node = scene(
            RContent::Mesh(std::sync::Arc::new(vec![test_rectangle(
                80.0, 80.0, 180.0, 180.0, fill,
            )])),
            RFeatures {
                clip,
                ..RFeatures::default()
            },
        )
        .nodes
        .remove(0);
        node.key = NodeId::new();
        document.nodes.push(node);
    }
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    assert_eq!(crate::renderer::v3_gap(&document), None);
    let Some(image) = v3_vector_test_render(&document) else {
        return;
    };
    let pixel =
        |x: usize, y: usize| -> [u8; 4] { image[(y * 256 + x) * 4..][..4].try_into().unwrap() };
    assert_eq!(pixel(50, 50), [0, 0, 0, 0]);
    assert_eq!(pixel(100, 100), [0, 0, 128, 128]);
}

#[test]
fn v3_clipped_adjustment_keeps_base_coverage() {
    let top = v3_adjustment(
        REffectKind::ColorAdjust {
            brightness: 0.2,
            contrast: 1.0,
            saturation: 1.0,
        },
        RFeatures {
            clip: true,
            ..RFeatures::default()
        },
    );
    let (document, store) = v3_square_under(top);
    assert_eq!(crate::renderer::v3_gap(&document), None);
    let Some(image) = v3_test_render_with(&document, &store, (0, 0, 0)) else {
        return;
    };
    let pixel =
        |x: usize, y: usize| -> [u8; 4] { image[(y * 256 + x) * 4..][..4].try_into().unwrap() };
    assert_eq!(pixel(50, 50), [0, 0, 0, 0]);
    assert_eq!(pixel(100, 100), [255, 51, 51, 255]);
}

#[test]
fn v3_backdrop_blur_matches_the_layer_blur() {
    let blur = REffectKind::GaussianBlur {
        radius: 8.0,
        radius_q8: 2048,
    };
    let (document, store) = v3_square_under(v3_adjustment(blur, RFeatures::default()));
    assert_eq!(crate::renderer::v3_gap(&document), None);
    let Some(gpu) = v3_test_render_with(&document, &store, (0, 0, 0)) else {
        return;
    };
    // An unmasked normal adjustment at opacity 1 replaces its backdrop.
    let input: Vec<[u16; 4]> = (0..272 * 272)
        .map(|i| {
            let (x, y) = (i % 272 - 8, i / 272 - 8);
            if (80..180).contains(&x) && (80..180).contains(&y) {
                [16384, 0, 0, 16384]
            } else {
                [0; 4]
            }
        })
        .collect();
    let weights = crate::renderer::blur_kernel_q16(8, 2048).unwrap();
    let middle = blur_pass_reference(&input, [272, 272], [256, 272], [8, 0], false, &weights);
    let blurred = blur_pass_reference(&middle, [256, 272], [256, 256], [0, 8], true, &weights);
    let expected: Vec<u8> = blurred
        .iter()
        .flat_map(|q| q.map(|v| ((u32::from(v) * 255 + 8192) >> 14) as u8))
        .collect();
    assert_eq!(gpu, expected);
}

#[test]
fn v3_adjustment_vector_mask_and_clip_path_mix_integers() {
    let adjust = REffectKind::ColorAdjust {
        brightness: 0.2,
        contrast: 1.0,
        saturation: 1.0,
    };
    let path = test_rectangle(32.5, 40.0, 96.5, 100.0, [1.0; 4]).data;
    for features in [
        RFeatures {
            vector_mask: Some(VectorMask {
                path: path.clone(),
                fill_rule: FillRule::NonZero,
                feather: 0.0,
                enabled: true,
                transform: Affine2::default(),
            }),
            ..RFeatures::default()
        },
        RFeatures {
            clip_path: Some(ClipPath {
                path: path.clone(),
                fill_rule: FillRule::NonZero,
                enabled: true,
                transform: Affine2::default(),
            }),
            ..RFeatures::default()
        },
    ] {
        let (document, store) = v3_square_under(v3_adjustment(adjust.clone(), features));
        assert_eq!(crate::renderer::v3_gap(&document), None);
        assert_eq!(gap(&document), Some("v3-m5-integer"));
        let Some(image) = v3_test_render_with(&document, &store, (0, 0, 0)) else {
            return;
        };
        let pixel =
            |x: usize, y: usize| -> [u8; 4] { image[(y * 256 + x) * 4..][..4].try_into().unwrap() };
        assert_eq!(pixel(85, 90), [255, 51, 51, 255]);
        assert_eq!(pixel(120, 120), [255, 0, 0, 255]);
        assert_eq!(pixel(50, 50), [0, 0, 0, 0]);
    }
}

#[test]
fn v3_unmasked_adjustment_blend_reaches_metal() {
    let mut top = v3_adjustment(
        REffectKind::ColorAdjust {
            brightness: 0.2,
            contrast: 1.0,
            saturation: 1.0,
        },
        RFeatures::default(),
    );
    top.blend = RenderBlend::Screen;
    top.opacity = 0.5;
    let (document, store) = v3_square_under(top);
    assert_eq!(crate::renderer::v3_gap(&document), None);
    let Some(image) = v3_test_render_with(&document, &store, (0, 0, 0)) else {
        return;
    };
    let pixel =
        |x: usize, y: usize| -> [u8; 4] { image[(y * 256 + x) * 4..][..4].try_into().unwrap() };
    assert_eq!(pixel(100, 100), [255, 26, 26, 255]);
    assert_eq!(pixel(50, 50), [0, 0, 0, 0]);
}

#[test]
fn v3_remaining_adjustment_effects_render_on_metal() {
    let cases = [
        (REffectKind::Invert, [255, 0, 0, 255], [0, 255, 255, 255], 0),
        (
            REffectKind::Curves {
                points: vec![(0.0, 1.0), (1.0, 0.0)],
            },
            [255, 0, 0, 255],
            [0, 255, 255, 255],
            0,
        ),
        (
            REffectKind::Levels {
                in_black: 0.0,
                in_white: 1.0,
                gamma: 2.0,
                out_black: 0.0,
                out_white: 1.0,
            },
            [128, 128, 128, 255],
            [181, 181, 181, 255],
            2,
        ),
        // Above 4, where the color-adjust clamp would have drawn gamma 4 (215).
        (
            REffectKind::Levels {
                in_black: 0.0,
                in_white: 1.0,
                gamma: 10.0,
                out_black: 0.0,
                out_white: 1.0,
            },
            [128, 128, 128, 255],
            [238, 238, 238, 255],
            2,
        ),
        (
            REffectKind::HueSaturation {
                hue: 90.0,
                saturation: 1.0,
                lightness: 0.0,
            },
            [255, 0, 0, 255],
            [108, 0, 255, 255],
            2,
        ),
    ];
    for (kind, color, expected, tolerance) in cases {
        for adjustment in [true, false] {
            let top = if adjustment {
                v3_adjustment(kind.clone(), RFeatures::default())
            } else {
                scene(RContent::Tiles(Vec::new()), RFeatures::default())
                    .nodes
                    .remove(0)
            };
            let (mut document, store) = v3_colored_square_under(top, color);
            if !adjustment {
                document.nodes[1].features.effects.push(REffect {
                    kind: kind.clone(),
                    selection: None,
                });
            }
            assert_eq!(crate::renderer::v3_gap(&document), None, "{kind:?}");
            let Some(image) = v3_test_render_with(&document, &store, (0, 0, 0)) else {
                return;
            };
            let pixel: [u8; 4] = image[(100 * 256 + 100) * 4..][..4].try_into().unwrap();
            for i in 0..4 {
                assert!(
                    pixel[i].abs_diff(expected[i]) <= tolerance,
                    "{kind:?} adjustment={adjustment} pixel={pixel:?} expected={expected:?}"
                );
            }
            assert_eq!(&image[(50 * 256 + 50) * 4..][..4], &[0, 0, 0, 0]);
        }
    }
}

#[test]
fn v3_subpixel_level_reads_raster_and_adjustment_on_metal() {
    assert_eq!(
        crate::renderer::input_map([288.0, 248.0, 1.0 / 64.0], [256, 256], [288.0, 248.0, 1.0]),
        Some([1, 1, 2, 128])
    );
    let (document, store) = v3_square_under(v3_adjustment(
        REffectKind::ColorAdjust {
            brightness: 0.2,
            contrast: 1.0,
            saturation: 1.0,
        },
        RFeatures::default(),
    ));
    assert_eq!(crate::renderer::v3_gap(&document), None);
    let Some(image) = v3_test_render_with(&document, &store, (-6, 25, 25)) else {
        return;
    };
    assert_eq!(&image[..4], &[255, 51, 51, 255]);
    assert_eq!(&image[(255 * 256 + 255) * 4..][..4], &[255, 51, 51, 255]);

    let path = test_rectangle(100.0, 100.0, 102.0, 104.0, [1.0; 4]).data;
    let (masked, store) = v3_square_under(v3_adjustment(
        REffectKind::Invert,
        RFeatures {
            vector_mask: Some(VectorMask {
                path,
                fill_rule: FillRule::NonZero,
                feather: 0.0,
                enabled: true,
                transform: Affine2::default(),
            }),
            ..RFeatures::default()
        },
    ));
    let Some(image) = v3_test_render_with(&masked, &store, (-6, 25, 25)) else {
        return;
    };
    assert_eq!(&image[..4], &[0, 255, 255, 255]);
    assert_eq!(&image[(255 * 256 + 255) * 4..][..4], &[255, 0, 0, 255]);
}

#[test]
fn v3_vector_transform_range_makes_the_node_transparent() {
    // Validation keeps document points within 2^20, so this is unreachable from
    // a document; a scene built directly still must not refuse the tile.
    let rectangle = test_rectangle(32.5, 40.0, 96.5, 100.0, [1.0, 0.0, 0.0, 0.5]);
    let mut far = test_rectangle(0.0, 0.0, 1.0, 1.0, [0.0, 0.0, 1.0, 1.0]);
    far.data.commands[1] = alto_vector::PathCommand::Line { x: 1e30, y: 0.0 };
    let mut document = scene(
        RContent::Mesh(std::sync::Arc::new(vec![rectangle, far])),
        RFeatures::default(),
    );
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    let Some(gpu) = v3_vector_test_render(&document) else {
        return;
    };
    assert!(gpu.iter().all(|&b| b == 0));
}

#[test]
fn v3_vector_curve_range_skips_only_that_path() {
    use alto_vector::PathCommand as P;
    let rectangle = test_rectangle(32.5, 40.0, 96.5, 100.0, [1.0, 0.0, 0.0, 0.5]);
    // Its second difference is far past the 559_241 level pixels NMAX covers.
    let far = alto_vector::StyledPath {
        data: PathData {
            commands: vec![
                P::Move { x: 0.0, y: 0.0 },
                P::Cubic {
                    c1x: 600_000.0,
                    c1y: 0.0,
                    c2x: -600_000.0,
                    c2y: 600_000.0,
                    x: 1.0,
                    y: 1.0,
                },
                P::Close,
            ],
        },
        fill: Some([0.0, 0.0, 1.0, 1.0]),
        stroke: None,
        fill_rule: FillRule::NonZero,
    };
    let mut alone = scene(
        RContent::Mesh(std::sync::Arc::new(vec![rectangle.clone()])),
        RFeatures::default(),
    );
    alone.render_version = alto_core::RENDER_VERSION_EXACT;
    let mut both = alone.clone();
    both.nodes[0].content = RContent::Mesh(std::sync::Arc::new(vec![far, rectangle]));
    let Some(expected) = v3_vector_test_render(&alone) else {
        return;
    };
    let gpu = v3_vector_test_render(&both).unwrap();
    assert_eq!(gpu, expected);
    assert!(gpu.iter().any(|&b| b != 0));
}

#[test]
fn v3_feathered_vector_mask_blurs_integer_coverage() {
    let source = test_rectangle(0.0, 0.0, 256.0, 256.0, [1.0, 0.0, 0.0, 0.5]);
    let mask = test_rectangle(40.5, 50.0, 180.25, 200.0, [1.0; 4]).data;
    let mut document = scene(
        RContent::Mesh(std::sync::Arc::new(vec![source])),
        RFeatures {
            vector_mask: Some(VectorMask {
                path: mask.clone(),
                fill_rule: FillRule::NonZero,
                feather: 4.0,
                enabled: true,
                transform: Affine2::default(),
            }),
            ..RFeatures::default()
        },
    );
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    assert_eq!(crate::renderer::v3_gap(&document), None);
    let Some(gpu) = v3_vector_test_render(&document) else {
        return;
    };
    // R = 1024 at level 0: k = -4, no coarsening, r = 4, rho8 = 1024.
    let commands = crate::raster_q8::map_path(&mask, alto_core::FixedAffine::IDENTITY, 0).unwrap();
    let coverage = crate::raster_q8::rasterize(
        &commands,
        crate::raster_q8::FillRule::NonZero,
        [-4, -4, 264, 264],
        crate::raster_q8::CURVE_LIMIT,
    )
    .unwrap();
    let white: Vec<[u16; 4]> = coverage
        .into_iter()
        .map(|k| crate::raster_q8::source_over_q14([0; 4], [16384; 4], k))
        .collect();
    let weights = crate::renderer::blur_kernel_q16(4, 1024).unwrap();
    let middle = blur_pass_reference(&white, [264, 264], [256, 264], [4, 0], false, &weights);
    let blurred = blur_pass_reference(&middle, [256, 264], [256, 256], [0, 4], true, &weights);
    let mut expected = vec![0u8; 256 * 256 * 4];
    for (out, k) in expected.as_chunks_mut::<4>().0.iter_mut().zip(&blurred) {
        // Source (8192, 0, 0, 8192) times the coverage k, rounded once.
        let q14 = (8192 * u32::from(k[3]) + 8192) >> 14;
        let byte = ((q14 * 255 + 8192) >> 14) as u8;
        out.copy_from_slice(&[byte, 0, 0, byte]);
    }
    assert_eq!(gpu, expected);
    // The feather is a real ramp, not a hard edge.
    let row: Vec<u8> = (36..46).map(|x| gpu[(120 * 256 + x) * 4 + 3]).collect();
    assert!(
        row.windows(2).all(|p| p[0] <= p[1]) && row[0] < row[9],
        "{row:?}"
    );
}
