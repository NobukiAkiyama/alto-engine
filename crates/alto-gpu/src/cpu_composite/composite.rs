//! Composite entry points: plain and integer composites, pyramids and render helpers.
use super::*;

/// Render version 3 nearest (`SAMPLE_NEAREST`): the texel under the Q8 position.
pub(super) fn fetch_nearest_bytes(
    tiles: &[Tile],
    inv: &IntegerInverse,
    origin_px: [i32; 2],
    scale: i32,
    x: i32,
    y: i32,
) -> [u8; 4] {
    let (Some(px), Some(py)) = (
        pixel_q8(origin_px[0], x, scale),
        pixel_q8(origin_px[1], y, scale),
    ) else {
        return [0; 4];
    };
    match source_q8(inv, px, py) {
        Some(q) => sample_u8(tiles, q[0] >> 8, q[1] >> 8),
        None => [0; 4],
    }
}

/// Render version 3 CPU reference of the integer tile compositor. A pixel is
/// on the canvas when its doubled center `2 origin + (2 i + 1) scale` is in
/// `[0, 2 end)`, exactly. Every draw is the Q16 lookup: nearest when the Q16
/// inverse is axis-aligned and unit (`axis_unit_q16`), else the integer 4-tap.
pub(super) fn composite_integer(
    ops: &[Op],
    width: u32,
    height: u32,
    frame: &Frame,
) -> (Vec<u8>, Vec<f32>) {
    let mut out = vec![0; 256 * 256 * 4];
    let mut prequant = vec![0.0; 256 * 256 * 4];
    let on = |o: i32, i: i32, end: u32| {
        (0..2 * i64::from(end))
            .contains(&(2 * i64::from(o) + (2 * i64::from(i) + 1) * i64::from(frame.scale_i)))
    };
    for y in 0..256 {
        for x in 0..256 {
            let mut accum = [0i64; 4];
            let mut stack = Vec::new();
            if on(frame.origin_px[0], x, width) && on(frame.origin_px[1], y, height) {
                for op in ops {
                    match op {
                        Op::Push => {
                            stack.push(accum);
                            accum = [0; 4];
                        }
                        Op::Pop { opacity, mode } => {
                            accum = q14::pop(
                                stack.pop().unwrap(),
                                accum,
                                q14::opacity(*opacity),
                                *mode,
                            );
                        }
                        Op::Draw {
                            q,
                            opacity,
                            mode,
                            tiles,
                            ..
                        } => {
                            let q = q.expect("v3 refuses a draw without a Q16 inverse");
                            let bytes = if crate::sample::axis_unit_q16(q.m) {
                                fetch_nearest_bytes(tiles, &q, frame.origin_px, frame.scale_i, x, y)
                            } else {
                                fetch_integer_bytes(tiles, &q, frame.origin_px, frame.scale_i, x, y)
                            };
                            accum = q14::draw(accum, bytes, q14::opacity(*opacity), *mode);
                        }
                    }
                }
            }
            let i = ((y * 256 + x) * 4) as usize;
            out[i..i + 4].copy_from_slice(&q14::bytes(accum));
            prequant[i..i + 4].copy_from_slice(&accum.map(|v| v as f32 / 16384.0));
        }
    }
    (out, prequant)
}

pub(super) fn composite(ops: &[Op], width: u32, height: u32, frame: &Frame) -> (Vec<u8>, Vec<f32>) {
    if frame.version == 3 {
        return composite_integer(ops, width, height, frame);
    }
    let mut out = vec![0u8; 256 * 256 * 4];
    let mut prequant = vec![0f32; 256 * 256 * 4];
    let width = width as f32;
    let height = height as f32;
    for y in 0..256u32 {
        for x in 0..256u32 {
            let p = [
                frame.origin[0] + (x as f32 + 0.5) * frame.scale,
                frame.origin[1] + (y as f32 + 0.5) * frame.scale,
            ];
            let mut stack = [[0.0f32; 4]; 32];
            let mut depth = 0usize;
            let mut accum = [0.0f32; 4];
            if p[0] >= 0.0 && p[1] >= 0.0 && p[0] < width && p[1] < height {
                for op in ops {
                    match op {
                        Op::Push => {
                            stack[depth] = accum;
                            depth += 1;
                            accum = [0.0; 4];
                        }
                        Op::Pop { opacity, mode } => {
                            let src = pop_src(accum, *opacity);
                            depth -= 1;
                            accum = blend(stack[depth], src, *mode);
                        }
                        Op::Draw {
                            inv,
                            opacity,
                            mode,
                            tiles,
                            ..
                        } => {
                            let s = fetch(tiles, *inv, p, x as i32, y as i32, frame);
                            accum = blend(accum, [s[0], s[1], s[2], s[3] * opacity], *mode);
                        }
                    }
                }
            }
            let i = ((y * 256 + x) * 4) as usize;
            prequant[i..i + 4].copy_from_slice(&accum);
            let q = quantize(accum);
            out[i..i + 4].copy_from_slice(&q);
        }
    }
    (out, prequant)
}

pub(super) fn render_pair(case: &Case, version: u32) -> (Vec<u8>, Vec<f32>) {
    let mut ops = Vec::new();
    for node in &case.nodes {
        if version == 3 {
            walk_v3(node, alto_core::FixedAffine::IDENTITY, &mut ops);
        } else {
            walk(node, ident(), &mut ops);
        }
    }
    let scale = 2f32.powi(case.level);
    let scale_i = 2i32.pow(case.level as u32);
    composite(
        &ops,
        case.w,
        case.h,
        &Frame {
            origin: [
                case.tx as f32 * 256.0 * scale,
                case.ty as f32 * 256.0 * scale,
            ],
            scale,
            origin_px: [case.tx * 256 * scale_i, case.ty * 256 * scale_i],
            scale_i,
            version,
        },
    )
}

/// Byte reference for the v3 cached pyramid; children are TL, TR, BL, BR.
pub(super) fn downsample_q14(children: &[Vec<u8>; 4]) -> Vec<u8> {
    let mut out = vec![0; 256 * 256 * 4];
    for y in 0..256 {
        for x in 0..256 {
            for c in 0..4 {
                let child = &children[(y / 128) * 2 + x / 128];
                let i = ((y % 128 * 2) * 256 + x % 128 * 2) * 4 + c;
                let sum = u32::from(child[i])
                    + u32::from(child[i + 4])
                    + u32::from(child[i + 1024])
                    + u32::from(child[i + 1028]);
                out[(y * 256 + x) * 4 + c] = ((sum + 2) / 4) as u8;
            }
        }
    }
    out
}
pub(super) fn pyramid_children(case: &Case) -> [Case; 4] {
    std::array::from_fn(|i| {
        let mut child = case.clone();
        child.level -= 1;
        child.tx = case.tx * 2 + (i % 2) as i32;
        child.ty = case.ty * 2 + (i / 2) as i32;
        child
    })
}
pub(super) fn render_pyramid(case: &Case) -> Vec<u8> {
    if case.level <= 0 {
        return render_at(case, 3);
    }
    downsample_q14(&pyramid_children(case).map(|c| render_pyramid(&c)))
}

pub(super) fn render_at(case: &Case, version: u32) -> Vec<u8> {
    render_pair(case, version).0
}

pub(super) fn render(case: &Case) -> Vec<u8> {
    render_at(case, RENDER_VERSION)
}

/// CPU bytes plus the f32 accumulator immediately before `quantize`.
pub(super) fn render_prequant(case: &Case) -> (Vec<u8>, Vec<f32>) {
    render_pair(case, RENDER_VERSION)
}

pub(super) fn pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * 256 + x) * 4) as usize;
    bytes[i..i + 4].try_into().unwrap()
}

pub(super) fn fnv64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub(super) fn reference_text() -> String {
    let mut lines = vec![
        "# CPU reference for composite.wgsl render version 2 (integer 4-tap). Columns: name, blake3, fnv-1a 64. Regenerate with ALTO_WRITE_PARITY=1."
            .to_string(),
    ];
    for case in cases().into_iter().filter(|c| c.lock) {
        let bytes = render(&case);
        lines.push(format!(
            "{} {} {:016x}",
            case.name,
            ContentHash::of(&bytes).hex(),
            fnv64(&bytes)
        ));
    }
    lines.push("# Render version 3 text: the ten enforced browser scenes at level 0.".to_string());
    for (name, bake, transform, tile) in text_v3_hash_cases() {
        let world = alto_core::FixedAffine::from_affine(Affine2(transform)).unwrap();
        let q = text_q14(
            &bake,
            world,
            0,
            [i64::from(tile.0) * 256, i64::from(tile.1) * 256, 256, 256],
        );
        let bytes: Vec<u8> = q
            .iter()
            .flat_map(|p| p.map(|v| ((u32::from(v) * 255 + 8192) >> 14) as u8))
            .collect();
        lines.push(format!(
            "{name} {} {:016x}",
            ContentHash::of(&bytes).hex(),
            fnv64(&bytes)
        ));
    }
    lines.push(
        "# Render version 3 composite: the measured catalog at level 0 and its level 1 pyramid."
            .to_string(),
    );
    for case in cases().into_iter().filter(|c| c.measure) {
        // As on the GPU, a case above level 0 is the integer pyramid.
        let bytes = if case.level > 0 {
            render_pyramid(&case)
        } else {
            render_at(&case, 3)
        };
        lines.push(format!(
            "v3-{} {} {:016x}",
            case.name,
            ContentHash::of(&bytes).hex(),
            fnv64(&bytes)
        ));
    }
    for mut case in cases().into_iter().filter(|c| c.measure) {
        case.level = 1;
        case.tx = 0;
        case.ty = 0;
        let bytes = render_pyramid(&case);
        lines.push(format!(
            "v3-{}-level1 {} {:016x}",
            case.name,
            ContentHash::of(&bytes).hex(),
            fnv64(&bytes)
        ));
    }
    lines.join("\n") + "\n"
}

pub(super) fn reference_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/parity/cpu-reference.txt")
}

pub(super) fn render_v1_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/parity/render-v1.txt")
}
