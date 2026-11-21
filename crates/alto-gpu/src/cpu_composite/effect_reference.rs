//! Reference blend for a Q14 input and its Gaussian-smoothed plane.
use super::q14::{Q, straight};

const CORDIC_ATAN_DEG_Q16: [i64; 16] = [
    2949120, 1740967, 919879, 466945, 234379, 117304, 58666, 29335, 14668, 7334, 3667, 1833, 917,
    458, 229, 115,
];

fn hue_sincos_q14(angle: i32) -> [i64; 2] {
    let angle = i64::from(angle);
    match angle {
        0 => return [Q, 0],
        5_898_240 => return [0, Q],
        -5_898_240 => return [0, -Q],
        11_796_480 | -11_796_480 => return [-Q, 0],
        _ => {}
    }
    let mut z = angle;
    let mut x = 9949i64;
    let mut y = 0i64;
    if z > 5_898_240 {
        z -= 11_796_480;
        x = -x;
        y = -y;
    }
    if z < -5_898_240 {
        z += 11_796_480;
        x = -x;
        y = -y;
    }
    for (i, atan) in CORDIC_ATAN_DEG_Q16.iter().enumerate() {
        let step_x = y >> i;
        let step_y = x >> i;
        if z >= 0 {
            x -= step_x;
            y += step_y;
            z -= atan;
        } else {
            x += step_x;
            y -= step_y;
            z += atan;
        }
    }
    [x.clamp(-Q, Q), y.clamp(-Q, Q)]
}

fn motion_blur_q14(
    source: &[[i64; 4]],
    size: [usize; 2],
    x: i32,
    y: i32,
    radius: i32,
    spacing_q8: i32,
    angle_q16: i32,
) -> [i64; 4] {
    if radius == 0 {
        return if x >= 0 && y >= 0 && (x as usize) < size[0] && (y as usize) < size[1] {
            source[y as usize * size[0] + x as usize]
        } else {
            [0; 4]
        };
    }
    let [cos, sin] = hue_sincos_q14(angle_q16);
    let step = [
        (cos * i64::from(spacing_q8) + 128) >> 8,
        (sin * i64::from(spacing_q8) + 128) >> 8,
    ];
    let mut sum = [0i64; 4];
    for i in -radius..=radius {
        let offset = [
            (i64::from(i) * step[0] + 8192) >> 14,
            (i64::from(i) * step[1] + 8192) >> 14,
        ];
        let sx = i64::from(x) + offset[0];
        let sy = i64::from(y) + offset[1];
        if sx >= 0 && sy >= 0 && sx < size[0] as i64 && sy < size[1] as i64 {
            let pixel = source[sy as usize * size[0] + sx as usize];
            for (sum, value) in sum.iter_mut().zip(pixel) {
                *sum += value;
            }
        }
    }
    let taps = i64::from(radius * 2 + 1);
    sum.map(|value| (value + taps / 2) / taps)
}

fn pcg32(input: u32) -> u32 {
    let state = input.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let shift = (state >> 28) + 4;
    let word = ((state >> shift) ^ state).wrapping_mul(277_803_737);
    (word >> 22) ^ word
}

fn noise_delta_q14(hash: u32, amount_q14: i32) -> i32 {
    let centered = 2 * ((hash >> 16) as i32) - 65_535;
    (centered * amount_q14 + 32_768).div_euclid(65_536)
}

fn noise_q14(
    pixel: [i64; 4],
    x_doc: i64,
    y_doc: i64,
    seed: u32,
    amount_q14: i32,
    monochrome: bool,
) -> [i64; 4] {
    if amount_q14 == 0 || pixel[3] == 0 {
        return pixel;
    }
    let alpha = pixel[3];
    let src = straight(pixel);
    let h = pcg32((x_doc as u32) ^ pcg32((y_doc as u32) ^ seed));
    let mut output = [0; 4];
    for channel in 0..3 {
        let sample = if monochrome {
            h
        } else {
            pcg32(h ^ (channel as u32 + 1))
        };
        let value = (src[channel] + i64::from(noise_delta_q14(sample, amount_q14))).clamp(0, Q);
        output[channel] = (value * alpha + Q / 2) / Q;
    }
    output[3] = alpha;
    output
}

#[test]
fn pcg32_matches_known_vectors() {
    for (input, expected) in [
        (0, 129_708_002),
        (1, 2_831_084_092),
        (u32::MAX, 3_861_530_882),
        (1_234_567, 3_336_649_100),
    ] {
        assert_eq!(pcg32(input), expected, "input {input}");
    }
}

#[test]
fn pcg32_matches_the_shader_known_vectors() {
    use super::{Gpu, map_readback};

    let gpu = match Gpu::new() {
        Ok(Some(gpu)) => gpu,
        Ok(None) if !cfg!(target_os = "macos") => {
            eprintln!("PCG32 GPU comparison skipped: no adapter");
            return;
        }
        Ok(None) => panic!("PCG32 GPU comparison requires an adapter"),
        Err(error) => panic!("PCG32 GPU setup failed: {error}"),
    };
    let production = include_str!("../../shaders/effects-q14.wgsl");
    let start = production.find("fn pcg32(v: u32) -> u32").unwrap();
    let end = start + production[start..].find("fn noise_doc_coord").unwrap();
    let helper = &production[start..end];
    let shader = format!(
        r#"
{helper}
@group(0) @binding(0) var<storage, read_write> output: array<u32>;
@compute @workgroup_size(4)
fn main(@builtin(global_invocation_id) id: vec3u) {{
    let inputs = array<u32, 4>(0u, 1u, 0xffffffffu, 1234567u);
    output[id.x] = pcg32(inputs[id.x]);
}}
"#
    );
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PCG32 known vectors"),
            source: wgpu::ShaderSource::Wgsl(shader.into()),
        });
    let pipeline = gpu
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("PCG32 known vectors"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
    let size = (4 * std::mem::size_of::<u32>()) as u64;
    let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("PCG32 results"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("PCG32 readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("PCG32 results"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: output.as_entire_binding(),
        }],
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("PCG32 comparison"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("PCG32 comparison"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, size);
    gpu.queue.submit(Some(encoder.finish()));
    map_readback(&gpu.device, &readback).unwrap();
    let mapped = readback.slice(..).get_mapped_range().unwrap();
    let actual: &[u32] = bytemuck::cast_slice(&mapped);
    assert_eq!(
        actual,
        &[129_708_002, 2_831_084_092, 3_861_530_882, 3_336_649_100]
    );
    drop(mapped);
    readback.unmap();
}

#[test]
fn noise_zero_amount_is_identity() {
    let pixel = [1024, 4096, 8192, 12_000];
    assert_eq!(noise_q14(pixel, -17, 32, u32::MAX, 0, false), pixel);
}

#[test]
fn noise_keeps_alpha_and_transparent_pixels_zero() {
    let pixel = [1000, 2000, 3000, 8000];
    let result = noise_q14(pixel, 3, -9, 11, 4096, false);
    assert_eq!(result[3], pixel[3]);
    assert_eq!(noise_q14([0; 4], 3, -9, 11, 4096, false), [0; 4]);
}

#[test]
fn noise_monochrome_adds_the_same_value_to_all_channels() {
    let pixel = [4096, 4096, 4096, Q];
    let result = noise_q14(pixel, 21, 7, 0xdead_beef, 4096, true);
    assert_eq!(result[0], result[1]);
    assert_eq!(result[1], result[2]);
}

#[test]
fn noise_is_independent_of_tile_origin_and_repeats_at_zoom_in() {
    let hash_at = |ox: i64, oy: i64, ix: i64, iy: i64, shift: u32, seed: u32| {
        let x_doc = (ox + ix).div_euclid(1_i64 << shift);
        let y_doc = (oy + iy).div_euclid(1_i64 << shift);
        pcg32(x_doc as u32 ^ pcg32(y_doc as u32 ^ seed))
    };
    assert_eq!(
        hash_at(-256, -256, 300, 280, 0, 123),
        hash_at(0, 0, 44, 24, 0, 123)
    );
    assert_eq!(hash_at(32, 20, 0, 0, 2, 123), hash_at(32, 20, 1, 1, 2, 123));
    assert_eq!(hash_at(32, 20, 1, 1, 2, 123), hash_at(32, 20, 2, 2, 2, 123));
    assert_eq!(hash_at(32, 20, 2, 2, 2, 123), hash_at(32, 20, 3, 3, 2, 123));
}

#[test]
fn noise_reference_matches_renderer_at_level_zero_and_zoom_in() {
    use super::{byte_q14, premul_byte_q14, scene, v3_test_render_with};
    use alto_core::{PixelFormat, TileKey};
    use alto_scene::{RContent, REffect, REffectKind, RFeatures};

    const W: usize = 256;
    const SEED: u32 = 0xdead_beef;
    const AMOUNT_Q14: i32 = 8192;
    let mut bytes = vec![0u8; W * W * 4];
    for y in 0..W {
        for x in 0..W {
            let offset = (y * W + x) * 4;
            bytes[offset..offset + 4].copy_from_slice(&[
                (x & 255) as u8,
                (y & 255) as u8,
                ((x * 17 + y * 3) & 255) as u8,
                if (x + y) % 11 == 0 {
                    0
                } else {
                    ((x + 128) & 255) as u8
                },
            ]);
        }
    }
    let source: Vec<[i64; 4]> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let alpha = u32::from(p[3]);
            [
                i64::from(premul_byte_q14(u32::from(p[0]), alpha)),
                i64::from(premul_byte_q14(u32::from(p[1]), alpha)),
                i64::from(premul_byte_q14(u32::from(p[2]), alpha)),
                i64::from(byte_q14(alpha)),
            ]
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
                kind: REffectKind::Noise {
                    seed: SEED,
                    amount_q14: AMOUNT_Q14 as u32,
                    monochrome: false,
                },
                selection: None,
            }],
            ..RFeatures::default()
        },
    );
    document.render_version = alto_core::RENDER_VERSION_EXACT;

    let level = 0i32;
    let shift = (-level) as u32;
    let reference: Vec<u8> = (0..W * W)
        .flat_map(|i| {
            let x = ((i % W) as i64).div_euclid(1_i64 << shift);
            let y = ((i / W) as i64).div_euclid(1_i64 << shift);
            noise_q14(
                source[y as usize * W + x as usize],
                x,
                y,
                SEED,
                AMOUNT_Q14,
                false,
            )
            .map(|v| ((v * 255 + Q / 2) / Q) as u8)
        })
        .collect();
    if let Some(gpu) = v3_test_render_with(&document, &store, (level, 0, 0)) {
        assert_eq!(gpu.len(), reference.len());
        if let Some(i) = gpu
            .as_chunks::<4>()
            .0
            .iter()
            .zip(reference.as_chunks::<4>().0)
            .position(|(actual, expected)| actual != expected)
        {
            panic!(
                "noise pixel at level {level} ({}, {}): GPU {:?}, reference {:?}",
                i % W,
                i / W,
                &gpu[i * 4..i * 4 + 4],
                &reference[i * 4..i * 4 + 4],
            );
        }
    }

    // At level -2, each 4x4 display block maps to the same document pixel.
    // Check this through the real renderer; the standalone coordinate vectors
    // above check the exact floor-divided PCG inputs.
    if let Some(gpu) = v3_test_render_with(&document, &store, (-2, 0, 0)) {
        let pixels = gpu.as_chunks::<4>().0;
        for y in 0..W {
            for x in 0..W {
                let first = pixels[(y / 4 * 4) * W + (x / 4 * 4)];
                assert_eq!(pixels[y * W + x], first, "zoom-in block at ({x}, {y})");
            }
        }
    }
}

#[test]
fn hue_sincos_q14_reference_covers_the_full_angle_domain() {
    for angle in (-11_796_480..=11_796_480).step_by(4096) {
        let [cos, sin] = hue_sincos_q14(angle);
        assert!((-Q..=Q).contains(&cos));
        assert!((-Q..=Q).contains(&sin));
        assert!((cos * cos + sin * sin - Q * Q).abs() < 262_144);
    }
    assert_eq!(hue_sincos_q14(0), [Q, 0]);
    assert_eq!(hue_sincos_q14(5_898_240), [0, Q]);
    assert_eq!(hue_sincos_q14(-5_898_240), [0, -Q]);
    assert_eq!(hue_sincos_q14(11_796_480), [-Q, 0]);
}

#[test]
fn hue_sincos_q14_reference_matches_the_shader_on_all_angles() {
    use super::{Gpu, map_readback};

    let gpu = match Gpu::new() {
        Ok(Some(gpu)) => gpu,
        Ok(None) if !cfg!(target_os = "macos") => {
            eprintln!("hue CORDIC GPU comparison skipped: no adapter");
            return;
        }
        Ok(None) => panic!("hue CORDIC GPU comparison requires an adapter"),
        Err(error) => panic!("hue CORDIC GPU setup failed: {error}"),
    };
    let production = include_str!("../../shaders/effects-q14.wgsl");
    let start = production.find("const CORDIC_ATAN_DEG_Q16").unwrap();
    let end = production.find("fn hue_saturation_q14").unwrap();
    let helper = &production[start..end];
    let shader = format!(
        r#"
const Q14_ONE: i32 = 16384;
{helper}
@group(0) @binding(0) var<storage, read_write> output: array<vec2i>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3u) {{
    if id.x >= 5761u {{ return; }}
    let angle = -11796480 + i32(id.x) * 4096;
    output[id.x] = hue_sincos_q14(angle);
}}
"#
    );
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("exhaustive hue CORDIC comparison"),
            source: wgpu::ShaderSource::Wgsl(shader.into()),
        });
    let pipeline = gpu
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("exhaustive hue CORDIC comparison"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
    let size = (5761 * std::mem::size_of::<[i32; 2]>()) as u64;
    let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("hue CORDIC results"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("hue CORDIC readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("hue CORDIC results"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: output.as_entire_binding(),
        }],
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hue CORDIC comparison"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("hue CORDIC comparison"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(91, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, size);
    gpu.queue.submit(Some(encoder.finish()));
    map_readback(&gpu.device, &readback).unwrap();
    let mapped = readback.slice(..).get_mapped_range().unwrap();
    let actual: &[[i32; 2]] = bytemuck::cast_slice(&mapped);
    let mut hash = 0xcbf29ce484222325u64;
    for (index, angle) in (-11_796_480..=11_796_480).step_by(4096).enumerate() {
        let expected = hue_sincos_q14(angle);
        assert_eq!(
            actual[index],
            [expected[0] as i32, expected[1] as i32],
            "angle {angle}"
        );
        for value in actual[index] {
            for byte in value.to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        }
    }
    eprintln!("hue CORDIC exhaustive FNV64: {hash:016x}");
    drop(mapped);
    readback.unmap();
}

#[test]
fn motion_blur_zero_distance_is_identity() {
    let source = [[234, 117, 200, 280], [100, 200, 300, 400]];
    assert_eq!(motion_blur_q14(&source, [2, 1], 0, 0, 0, 0, 0), source[0]);
    assert_eq!(motion_blur_q14(&source, [2, 1], -1, 0, 0, 0, 0), [0; 4]);
}

#[test]
fn motion_blur_of_flat_color_is_flat_inside_the_roi() {
    let pixel = [4096, 2048, 1024, Q];
    let source = [pixel; 25];
    for y in 1..4 {
        for x in 1..4 {
            assert_eq!(motion_blur_q14(&source, [5, 5], x, y, 1, 256, 0), pixel);
        }
    }
}

#[test]
fn motion_blur_axis_aligned_is_a_box_average() {
    let source: Vec<_> = (0..25)
        .map(|i| {
            let value = ((i / 5 * 10 + i % 5) as i64) * 1000;
            [value, value, value, Q]
        })
        .collect();
    assert_eq!(
        motion_blur_q14(&source, [5, 5], 2, 2, 1, 256, 0),
        [22_000, 22_000, 22_000, Q]
    );
    assert_eq!(
        motion_blur_q14(&source, [5, 5], 2, 2, 1, 256, 5_898_240),
        [22_000, 22_000, 22_000, Q]
    );
}

#[test]
fn motion_blur_reference_matches_renderer() {
    use super::{byte_q14, premul_byte_q14, scene, v3_test_render_with};
    use alto_core::{PixelFormat, TileKey};
    use alto_scene::{RContent, REffect, REffectKind, RFeatures};

    const W: usize = 256;
    const DISTANCE_Q8: u32 = 8 * 256;
    const RADIUS: i32 = 4;
    let mut bytes = vec![0u8; W * W * 4];
    for y in 0..W {
        for x in 0..W {
            let offset = (y * W + x) * 4;
            bytes[offset..offset + 4].copy_from_slice(&[
                (x & 255) as u8,
                (y & 255) as u8,
                ((x + y) & 255) as u8,
                128,
            ]);
        }
    }
    let source: Vec<[i64; 4]> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let alpha = u32::from(p[3]);
            [
                i64::from(premul_byte_q14(u32::from(p[0]), alpha)),
                i64::from(premul_byte_q14(u32::from(p[1]), alpha)),
                i64::from(premul_byte_q14(u32::from(p[2]), alpha)),
                i64::from(byte_q14(alpha)),
            ]
        })
        .collect();
    let reference: Vec<u8> = (0..W * W)
        .flat_map(|i| {
            motion_blur_q14(
                &source,
                [W, W],
                (i % W) as i32,
                (i / W) as i32,
                RADIUS,
                256,
                45 * 65536,
            )
            .map(|v| ((v * 255 + Q / 2) / Q) as u8)
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
                kind: REffectKind::MotionBlur {
                    angle_q16: 45 * 65536,
                    distance_q8: DISTANCE_Q8,
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
            panic!(
                "motion-blur pixel ({}, {}): GPU {:?}, reference {:?}",
                i % W,
                i / W,
                &gpu[i * 4..i * 4 + 4],
                &reference[i * 4..i * 4 + 4],
            );
        }
    }
}

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

fn unsharp_mask_q14(
    source: [i64; 4],
    blurred: [i64; 4],
    amount_q8: i64,
    threshold_q14: i64,
) -> [i64; 4] {
    let a = straight(source);
    let b = straight(blurred);
    let alpha = source[3];
    let mut output = source;
    for channel in 0..3 {
        let difference = a[channel] - b[channel];
        if difference.abs() > threshold_q14 && amount_q8 != 0 {
            let delta = (amount_q8 * difference + 128).div_euclid(256);
            let sharp = (a[channel] + delta).clamp(0, Q);
            output[channel] = (sharp * alpha + Q / 2) / Q;
        }
    }
    output
}

#[test]
fn unsharp_zero_amount_is_identity() {
    let source = [234, 117, 200, 280];
    assert_eq!(unsharp_mask_q14(source, [0, 0, 0, Q], 0, 0), source);
    assert_eq!(unsharp_mask_q14([0; 4], [Q; 4], 1280, 0), [0; 4]);
}

#[test]
fn unsharp_threshold_keeps_small_differences() {
    let source = [4000, 7000, 10000, Q];
    let blurred = [3999, 7005, Q, Q];
    assert_eq!(
        unsharp_mask_q14(source, blurred, 128, 5),
        [4000, 7000, 6808, Q]
    );
}

#[test]
fn unsharp_negative_difference_rounds_by_floor() {
    let source = [1000, 1000, 1000, Q];
    let blurred = [1003, 1002, 997, Q];
    assert_eq!(
        unsharp_mask_q14(source, blurred, 64, 0),
        [999, 1000, 1001, Q]
    );
}

#[test]
fn unsharp_clamps_at_both_ends() {
    let source = [2048, 6144, 4096, Q / 2];
    let blurred = [6144, 2048, 4096, Q / 2];
    assert_eq!(
        unsharp_mask_q14(source, blurred, 1280, 0),
        [0, Q / 2, 4096, Q / 2]
    );
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
