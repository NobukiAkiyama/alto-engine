//! CPU reference for `shaders/composite.wgsl`, and a headless run of that shader.
//!
//! The reference is `#[cfg(test)]`, so the shipped WASM does not contain it. It follows
//! the shader: f64 world matrices, straight source texels, premultiplied accumulation,
//! and `rgba8unorm` ties-to-even quantization. Render version 2 (the default here)
//! filters bilinear and rotated samples with the integer 4-tap in `sample.rs`.
//! Render version 1 keeps the f32 `mix` path. It does not cull opaque layers; a
//! correct cull leaves the same pixels, and a wrong one fails the browser suite,
//! which reads production tiles.
//!
//! Masks, clips, effects, vectors and text are not on this path. `composite.wgsl` never
//! sees them (`plain_scene` is false, and the effect pass uses rgba16float). Those cases
//! are explicit xfails in the browser suite.

use crate::blend_ref::blend;
use crate::damage::plain_scene;
use crate::sample::{
    IntegerInverse, axis_unit, bilinear_byte, floor_frac_q8, integer_inverse, pixel_q8, source_q8,
};
use crate::shader_with_blend;
use alto_core::{Affine2, ContentHash, IRect, NodeId, RENDER_VERSION, RENDER_VERSION_F32};
use alto_scene::{
    RContent, REffect, REffectKind, RFeatures, RMask, RNode, RenderBlend, RenderScene,
};
use alto_vector::{ClipPath, FillRule, PathData, VectorMask};
use bytemuck::{Pod, Zeroable};
use std::path::PathBuf;

const BLENDS: [&str; 17] = [
    "normal",
    "multiply",
    "screen",
    "overlay",
    "darken",
    "lighten",
    "color_dodge",
    "color_burn",
    "hard_light",
    "soft_light",
    "difference",
    "exclusion",
    "hue",
    "saturation",
    "color",
    "luminosity",
    "add",
];

#[derive(Clone)]
struct Image {
    w: u32,
    h: u32,
    pixels: Vec<u8>,
}

struct Tile {
    tx: i32,
    ty: i32,
    pixels: Vec<u8>,
}

enum Op {
    Push,
    Pop {
        opacity: f32,
        mode: u32,
    },
    Draw {
        inv: [f64; 6],
        opacity: f32,
        mode: u32,
        tiles: Vec<Tile>,
    },
}

#[derive(Clone)]
enum Node {
    Raster {
        image: Image,
        opacity: f32,
        mode: u32,
        visible: bool,
        transform: [f64; 6],
    },
    Group {
        isolated: bool,
        opacity: f32,
        mode: u32,
        visible: bool,
        transform: [f64; 6],
        children: Vec<Node>,
    },
}

struct Case {
    name: String,
    w: u32,
    h: u32,
    level: i32,
    tx: i32,
    ty: i32,
    /// Written to `tests/parity/cpu-reference.txt` (render version 2).
    lock: bool,
    nodes: Vec<Node>,
}

fn ident() -> [f64; 6] {
    [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
}

fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Image {
    Image {
        w,
        h,
        pixels: rgba.repeat((w * h) as usize),
    }
}

fn ramp(w: u32, h: u32) -> Image {
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            pixels[i] = (x & 255) as u8;
            pixels[i + 1] = (y & 255) as u8;
            pixels[i + 2] = ((x * 3 + y * 5) & 255) as u8;
            pixels[i + 3] = 255;
        }
    }
    Image { w, h, pixels }
}

fn checker(w: u32, h: u32, a: [u8; 4], b: [u8; 4]) -> Image {
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            pixels[i..i + 4].copy_from_slice(if (x + y) % 2 == 0 { &a } else { &b });
        }
    }
    Image { w, h, pixels }
}

fn raster(image: Image, mode: u32, opacity: f32, visible: bool, transform: [f64; 6]) -> Node {
    Node::Raster {
        image,
        opacity,
        mode,
        visible,
        transform,
    }
}

fn group(isolated: bool, opacity: f32, children: Vec<Node>) -> Node {
    Node::Group {
        isolated,
        opacity,
        mode: 0,
        visible: true,
        transform: ident(),
        children,
    }
}

fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    let blue = solid(256, 256, [0, 0, 255, 255]);
    let red_half = solid(256, 256, [255, 0, 0, 128]);
    for (mode, name) in BLENDS.iter().enumerate() {
        out.push(Case {
            name: format!("blend-{name}"),
            w: 256,
            h: 256,
            level: 0,
            tx: 0,
            ty: 0,
            lock: true,
            nodes: vec![
                raster(blue.clone(), 0, 1.0, true, ident()),
                raster(red_half.clone(), mode as u32, 1.0, true, ident()),
            ],
        });
    }
    out.push(Case {
        name: "opacity-half".into(),
        w: 256,
        h: 256,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: vec![
            raster(blue.clone(), 0, 1.0, true, ident()),
            raster(solid(256, 256, [255, 0, 0, 255]), 0, 0.5, true, ident()),
        ],
    });
    out.push(Case {
        name: "opacity-zero".into(),
        w: 256,
        h: 256,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: vec![
            raster(blue.clone(), 0, 1.0, true, ident()),
            raster(solid(256, 256, [255, 0, 0, 255]), 0, 0.0, true, ident()),
        ],
    });
    out.push(Case {
        name: "hidden-layer".into(),
        w: 256,
        h: 256,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: vec![
            raster(blue.clone(), 0, 1.0, true, ident()),
            raster(solid(256, 256, [255, 0, 0, 255]), 0, 1.0, false, ident()),
        ],
    });
    let stacked = vec![
        raster(solid(256, 256, [0, 255, 0, 128]), 0, 1.0, true, ident()),
        raster(red_half.clone(), 0, 1.0, true, ident()),
    ];
    out.push(Case {
        name: "isolated-group".into(),
        w: 256,
        h: 256,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: vec![
            raster(blue.clone(), 0, 1.0, true, ident()),
            group(true, 1.0, stacked.clone()),
        ],
    });
    out.push(Case {
        name: "pass-through-group".into(),
        w: 256,
        h: 256,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: vec![
            raster(blue.clone(), 0, 1.0, true, ident()),
            group(false, 1.0, stacked),
        ],
    });
    out.push(Case {
        name: "nested-groups".into(),
        w: 256,
        h: 256,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: vec![
            raster(blue.clone(), 0, 1.0, true, ident()),
            group(
                true,
                0.5,
                vec![group(
                    false,
                    1.0,
                    vec![raster(
                        solid(256, 256, [255, 0, 0, 255]),
                        0,
                        1.0,
                        true,
                        ident(),
                    )],
                )],
            ),
        ],
    });
    let moved = vec![raster(
        ramp(256, 256),
        0,
        1.0,
        true,
        [1.0, 0.0, 0.0, 1.0, 200.0, 0.0],
    )];
    for (name, tx) in [("translate-across-tile", 0), ("translate-across-tile-1", 1)] {
        out.push(Case {
            name: name.into(),
            w: 512,
            h: 256,
            level: 0,
            tx,
            ty: 0,
            lock: true,
            nodes: moved.clone(),
        });
    }
    out.push(Case {
        name: "half-pixel-translate".into(),
        w: 256,
        h: 256,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: vec![raster(
            ramp(256, 256),
            0,
            1.0,
            true,
            [1.0, 0.0, 0.0, 1.0, 0.5, 0.0],
        )],
    });
    out.push(Case {
        name: "scale-bilinear".into(),
        w: 512,
        h: 512,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: vec![raster(
            ramp(256, 256),
            0,
            1.0,
            true,
            [2.0, 0.0, 0.0, 2.0, 0.0, 0.0],
        )],
    });
    out.push(Case {
        name: "extremes".into(),
        w: 256,
        h: 256,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: vec![raster(
            checker(256, 256, [0, 0, 0, 255], [255, 255, 255, 255]),
            0,
            1.0,
            true,
            ident(),
        )],
    });
    out.push(Case {
        name: "alpha-extremes".into(),
        w: 256,
        h: 256,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: vec![
            raster(blue.clone(), 0, 1.0, true, ident()),
            raster(
                checker(256, 256, [255, 0, 0, 0], [255, 0, 0, 255]),
                0,
                1.0,
                true,
                ident(),
            ),
        ],
    });
    out.push(Case {
        name: "two-partial".into(),
        w: 256,
        h: 256,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: vec![
            raster(solid(256, 256, [0, 0, 255, 200]), 0, 1.0, true, ident()),
            raster(solid(256, 256, [255, 0, 0, 100]), 0, 1.0, true, ident()),
        ],
    });
    let mut layers = Vec::new();
    for i in 0..8u32 {
        layers.push(raster(
            solid(
                256,
                256,
                [
                    ((i * 40) & 255) as u8,
                    ((i * 70) & 255) as u8,
                    ((i * 110) & 255) as u8,
                    80,
                ],
            ),
            0,
            1.0,
            true,
            ident(),
        ));
    }
    out.push(Case {
        name: "eight-layers".into(),
        w: 256,
        h: 256,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: layers,
    });
    let odd = vec![raster(
        solid(300, 100, [255, 0, 0, 255]),
        0,
        1.0,
        true,
        ident(),
    )];
    for (name, tx) in [("odd-canvas-0", 0), ("odd-canvas-1", 1)] {
        out.push(Case {
            name: name.into(),
            w: 300,
            h: 100,
            level: 0,
            tx,
            ty: 0,
            lock: true,
            nodes: odd.clone(),
        });
    }
    out.push(Case {
        name: "level1-centers".into(),
        w: 512,
        h: 512,
        level: 1,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: vec![raster(ramp(512, 512), 0, 1.0, true, ident())],
    });
    let angle = 0.3f64;
    let (c, s) = (angle.cos(), angle.sin());
    out.push(Case {
        name: "rotation".into(),
        w: 512,
        h: 512,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        nodes: vec![raster(
            ramp(256, 256),
            0,
            1.0,
            true,
            [c, s, -s, c, 32.0, 24.0],
        )],
    });
    // Scale 0.1 and a 4000px move invert to a -40000px translation. That Q16
    // value does not fit in i32; the integer path keeps it in i64.
    out.push(Case {
        name: "large-translation".into(),
        w: 4096,
        h: 256,
        level: 0,
        tx: 15,
        ty: 0,
        lock: true,
        nodes: vec![raster(
            ramp(256, 256),
            0,
            1.0,
            true,
            [0.1, 0.0, 0.0, 0.1, 4000.0, 0.0],
        )],
    });
    // Scale 0.01 and a 84000px move invert to −8400000px. At tile 983 the
    // rounded source Q8 is past ±2^23 px. Truncating it to i32 would wrap
    // pixel (124, 0) onto ramp texel (34, 50). The integer path returns
    // transparent instead, for every pixel of the tile.
    out.push(Case {
        name: "large-translation-out-of-range".into(),
        w: 251_904,
        h: 256,
        level: 0,
        tx: 983,
        ty: 0,
        lock: true,
        nodes: vec![raster(
            ramp(256, 256),
            0,
            1.0,
            true,
            [0.01, 0.0, 0.0, 0.01, 84_000.0, 0.0],
        )],
    });
    out
}

fn split_tiles(image: &Image) -> Vec<Tile> {
    let mut tiles = Vec::new();
    let nx = image.w.div_ceil(256);
    let ny = image.h.div_ceil(256);
    for ty in 0..ny {
        for tx in 0..nx {
            let mut pixels = vec![0u8; 256 * 256 * 4];
            let mut ink = false;
            for y in 0..256u32 {
                let sy = ty * 256 + y;
                if sy >= image.h {
                    break;
                }
                for x in 0..256u32 {
                    let sx = tx * 256 + x;
                    if sx >= image.w {
                        break;
                    }
                    let s = ((sy * image.w + sx) * 4) as usize;
                    let d = ((y * 256 + x) * 4) as usize;
                    pixels[d..d + 4].copy_from_slice(&image.pixels[s..s + 4]);
                    ink |= image.pixels[s + 3] != 0;
                }
            }
            if ink {
                tiles.push(Tile {
                    tx: tx as i32,
                    ty: ty as i32,
                    pixels,
                });
            }
        }
    }
    tiles.sort_by_key(|t| (t.tx, t.ty));
    tiles
}

fn walk(node: &Node, parent: [f64; 6], ops: &mut Vec<Op>) {
    let (visible, opacity, transform) = match node {
        Node::Raster {
            visible,
            opacity,
            transform,
            ..
        }
        | Node::Group {
            visible,
            opacity,
            transform,
            ..
        } => (*visible, *opacity, *transform),
    };
    if !visible || opacity <= 0.0 {
        return;
    }
    let world = Affine2(parent).concat(Affine2(transform)).0;
    match node {
        Node::Group {
            isolated,
            mode,
            children,
            ..
        } => {
            let isolate = *isolated || opacity < 1.0 || *mode != 0;
            if isolate {
                ops.push(Op::Push);
            }
            for child in children {
                walk(child, world, ops);
            }
            if isolate {
                ops.push(Op::Pop {
                    opacity,
                    mode: *mode,
                });
            }
        }
        Node::Raster { image, mode, .. } => {
            let Some(inv) = Affine2(world).inverse() else {
                return;
            };
            ops.push(Op::Draw {
                inv: inv.0,
                opacity,
                mode: *mode,
                tiles: split_tiles(image),
            });
        }
    }
}

fn lookup(tiles: &[Tile], key: (i32, i32)) -> Option<&[u8]> {
    let mut lo = 0usize;
    let mut hi = tiles.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        let tile = &tiles[mid];
        if tile.tx < key.0 || (tile.tx == key.0 && tile.ty < key.1) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let tile = tiles.get(lo)?;
    if tile.tx == key.0 && tile.ty == key.1 {
        Some(&tile.pixels)
    } else {
        None
    }
}

fn texel(tiles: &[Tile], q: [f32; 2]) -> [f32; 4] {
    let key = ((q[0] / 256.0).floor() as i32, (q[1] / 256.0).floor() as i32);
    let Some(pixels) = lookup(tiles, key) else {
        return [0.0; 4];
    };
    let local_x = (q[0].floor() as i32 - key.0 * 256).clamp(0, 255);
    let local_y = (q[1].floor() as i32 - key.1 * 256).clamp(0, 255);
    let i = ((local_y as usize * 256) + local_x as usize) * 4;
    [
        f32::from(pixels[i]) / 255.0,
        f32::from(pixels[i + 1]) / 255.0,
        f32::from(pixels[i + 2]) / 255.0,
        f32::from(pixels[i + 3]) / 255.0,
    ]
}

fn fetch_f32(tiles: &[Tile], inv: [f32; 6], p: [f32; 2]) -> [f32; 4] {
    let q = [
        inv[0] * p[0] + inv[2] * p[1] + inv[4],
        inv[1] * p[0] + inv[3] * p[1] + inv[5],
    ];
    if axis_unit(inv) {
        return texel(tiles, q);
    }
    let base = [q[0].floor(), q[1].floor()];
    let f = [q[0] - base[0], q[1] - base[1]];
    let a = texel(tiles, base);
    let b = texel(tiles, [base[0] + 1.0, base[1]]);
    let d = texel(tiles, [base[0], base[1] + 1.0]);
    let e = texel(tiles, [base[0] + 1.0, base[1] + 1.0]);
    let mut out = [0.0; 4];
    for i in 0..4 {
        let ab = a[i] * (1.0 - f[0]) + b[i] * f[0];
        let de = d[i] * (1.0 - f[0]) + e[i] * f[0];
        out[i] = ab * (1.0 - f[1]) + de * f[1];
    }
    out
}

fn sample_u8(tiles: &[Tile], x: i32, y: i32) -> [u8; 4] {
    let key = (x >> 8, y >> 8);
    let Some(pixels) = lookup(tiles, key) else {
        return [0; 4];
    };
    let local_x = (x - key.0 * 256).clamp(0, 255) as usize;
    let local_y = (y - key.1 * 256).clamp(0, 255) as usize;
    let i = (local_y * 256 + local_x) * 4;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

fn fetch_integer(
    tiles: &[Tile],
    inv: &IntegerInverse,
    origin_px: [i32; 2],
    scale: i32,
    x: i32,
    y: i32,
) -> [f32; 4] {
    let Some(px) = pixel_q8(origin_px[0], x, scale) else {
        return [0.0; 4];
    };
    let Some(py) = pixel_q8(origin_px[1], y, scale) else {
        return [0.0; 4];
    };
    let Some(q) = source_q8(inv, px, py) else {
        return [0.0; 4];
    };
    let (ix, fx) = floor_frac_q8(q[0]);
    let (iy, fy) = floor_frac_q8(q[1]);
    let a = sample_u8(tiles, ix, iy);
    let b = sample_u8(tiles, ix + 1, iy);
    let d = sample_u8(tiles, ix, iy + 1);
    let e = sample_u8(tiles, ix + 1, iy + 1);
    let mut out = [0.0; 4];
    for channel in 0..4 {
        let byte = bilinear_byte(a[channel], b[channel], d[channel], e[channel], fx, fy);
        out[channel] = f32::from(byte) / 255.0;
    }
    out
}

/// Tile placement and which sampling path the document's render version selects.
struct Frame {
    origin: [f32; 2],
    scale: f32,
    origin_px: [i32; 2],
    scale_i: i32,
    version: u32,
}

fn fetch(tiles: &[Tile], inv: [f64; 6], p: [f32; 2], x: i32, y: i32, frame: &Frame) -> [f32; 4] {
    let inv_f32 = inv.map(|v| v as f32);
    if frame.version < RENDER_VERSION || axis_unit(inv_f32) {
        return fetch_f32(tiles, inv_f32, p);
    }
    match integer_inverse(inv) {
        Some(q) => fetch_integer(tiles, &q, frame.origin_px, frame.scale_i, x, y),
        None => fetch_f32(tiles, inv_f32, p),
    }
}

fn pop_src(accum: [f32; 4], opacity: f32) -> [f32; 4] {
    let rgb = if accum[3] > 0.0 {
        [
            accum[0] / accum[3].max(0.000001),
            accum[1] / accum[3].max(0.000001),
            accum[2] / accum[3].max(0.000001),
        ]
    } else {
        [0.0; 3]
    };
    [rgb[0], rgb[1], rgb[2], accum[3] * opacity]
}

fn quantize(pixel: [f32; 4]) -> [u8; 4] {
    pixel.map(|v| (v.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8)
}

fn composite(ops: &[Op], width: u32, height: u32, frame: &Frame) -> Vec<u8> {
    let mut out = vec![0u8; 256 * 256 * 4];
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
                        } => {
                            let s = fetch(tiles, *inv, p, x as i32, y as i32, frame);
                            accum = blend(accum, [s[0], s[1], s[2], s[3] * opacity], *mode);
                        }
                    }
                }
            }
            let q = quantize(accum);
            let i = ((y * 256 + x) * 4) as usize;
            out[i..i + 4].copy_from_slice(&q);
        }
    }
    out
}

fn render_at(case: &Case, version: u32) -> Vec<u8> {
    let mut ops = Vec::new();
    for node in &case.nodes {
        walk(node, ident(), &mut ops);
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

fn render(case: &Case) -> Vec<u8> {
    render_at(case, RENDER_VERSION)
}

fn pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * 256 + x) * 4) as usize;
    bytes[i..i + 4].try_into().unwrap()
}

fn fnv64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn reference_text() -> String {
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
    lines.join("\n") + "\n"
}

fn reference_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/parity/cpu-reference.txt")
}

fn render_v1_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/parity/render-v1.txt")
}

fn diff_report(cpu: &[u8], gpu: &[u8]) -> String {
    let mut count = 0u32;
    let mut at = None;
    let n = cpu.len().min(gpu.len()) / 4;
    for i in 0..n {
        let o = i * 4;
        if cpu[o..o + 4] != gpu[o..o + 4] {
            count += 1;
            if at.is_none() {
                at = Some((i % 256, i / 256, o));
            }
        }
    }
    match at {
        Some((x, y, o)) => format!(
            "{count} pixels differ; first {x},{y}; gpu {},{},{},{}; cpu {},{},{},{}; lengths cpu {} gpu {}",
            gpu[o],
            gpu[o + 1],
            gpu[o + 2],
            gpu[o + 3],
            cpu[o],
            cpu[o + 1],
            cpu[o + 2],
            cpu[o + 3],
            cpu.len(),
            gpu.len()
        ),
        None => format!(
            "0 pixels differ; first none; gpu none; cpu none; lengths cpu {} gpu {}",
            cpu.len(),
            gpu.len()
        ),
    }
}

fn parse_reference(text: &str) -> Vec<(String, String, String)> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut parts = line.split(' ');
            (
                parts.next().unwrap().to_string(),
                parts.next().unwrap().to_string(),
                parts.next().unwrap().to_string(),
            )
        })
        .collect()
}

fn scene(content: RContent, features: RFeatures) -> RenderScene {
    RenderScene {
        canvas: IRect {
            x0: 0,
            y0: 0,
            x1: 256,
            y1: 256,
        },
        root: 0,
        render_version: RENDER_VERSION,
        nodes: vec![RNode {
            key: NodeId::new(),
            version: 0,
            visible: true,
            transform: ident(),
            opacity: 1.0,
            blend: RenderBlend::Normal,
            effects: features.effects.len(),
            features,
            content,
        }],
    }
}

fn gap(scene: &RenderScene) -> Option<&'static str> {
    for n in &scene.nodes {
        if n.features.mask.is_some() {
            return Some("mask");
        }
        if n.features.vector_mask.is_some() {
            return Some("vector mask");
        }
        if n.features.clip_path.is_some() {
            return Some("clip path");
        }
        if n.features.clip {
            return Some("clip");
        }
        if !n.features.effects.is_empty() {
            return Some("effects");
        }
        match &n.content {
            RContent::Mesh(_) => return Some("vector"),
            RContent::Text(_) => return Some("text"),
            RContent::Adjustment(_) => return Some("adjustment"),
            RContent::Unsupported => return Some("unsupported"),
            RContent::Group { .. } | RContent::Tiles(_) => {}
        }
    }
    None
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuCommand {
    inv0: [f32; 4],
    inv1: [f32; 4],
    info: [u32; 4],
    trans: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuTile {
    xy: [i32; 2],
    slot: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuConfig {
    origin_scale: [f32; 4],
    info: [u32; 4],
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
}

impl Gpu {
    fn new() -> Result<Option<Self>, String> {
        pollster::block_on(async {
            let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
            descriptor.backends =
                wgpu::Backends::METAL | wgpu::Backends::VULKAN | wgpu::Backends::DX12;
            let instance = wgpu::Instance::new(descriptor);
            let adapter = match instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
            {
                Ok(adapter) => adapter,
                Err(_) => return Ok(None),
            };
            let info = adapter.get_info();
            println!(
                "gpu pixel parity adapter: {} ({:?})",
                info.name, info.backend
            );
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .map_err(|e| format!("request_device: {e}"))?;
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("composite parity"),
                source: wgpu::ShaderSource::Wgsl(
                    shader_with_blend(include_str!("../shaders/composite.wgsl")).into(),
                ),
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("composite parity"),
                layout: None,
                module: &shader,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
            Ok(Some(Self {
                device,
                queue,
                pipeline,
            }))
        })
    }

    fn render(&self, case: &Case) -> Result<Vec<u8>, String> {
        let mut ops = Vec::new();
        for node in &case.nodes {
            walk(node, ident(), &mut ops);
        }
        let mut commands = Vec::new();
        let mut entries = Vec::new();
        let mut atlas_tiles: Vec<Vec<u8>> = Vec::new();
        for op in &ops {
            match op {
                Op::Push => commands.push(GpuCommand {
                    inv0: [0.0; 4],
                    inv1: [0.0; 4],
                    info: [1, 0, 0, 0],
                    trans: [0; 4],
                }),
                Op::Pop { opacity, mode } => commands.push(GpuCommand {
                    inv0: [0.0; 4],
                    inv1: [0.0, 0.0, *opacity, *mode as f32],
                    info: [2, 0, 0, 0],
                    trans: [0; 4],
                }),
                Op::Draw {
                    inv,
                    opacity,
                    mode,
                    tiles,
                } => {
                    let start = entries.len() as u32;
                    for tile in tiles {
                        let slot = atlas_tiles.len() as u32;
                        atlas_tiles.push(tile.pixels.clone());
                        entries.push(GpuTile {
                            xy: [tile.tx, tile.ty],
                            slot,
                            pad: 0,
                        });
                    }
                    let packed = crate::sample::pack_inverse(*inv, RENDER_VERSION);
                    commands.push(GpuCommand {
                        inv0: packed.inv0,
                        inv1: [
                            packed.translation[0],
                            packed.translation[1],
                            *opacity,
                            *mode as f32,
                        ],
                        info: [0, start, entries.len() as u32 - start, packed.sample_mode],
                        trans: packed.trans,
                    });
                }
            }
        }
        let layers = (atlas_tiles.len() as u32).max(1);
        let device = &self.device;
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("parity atlas"),
            size: wgpu::Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (slot, pixels) in atlas_tiles.iter().enumerate() {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: slot as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(1024),
                    rows_per_image: Some(256),
                },
                wgpu::Extent3d {
                    width: 256,
                    height: 256,
                    depth_or_array_layers: 1,
                },
            );
        }
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("parity output"),
            size: wgpu::Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let scale = 2f32.powi(case.level);
        let config = [GpuConfig {
            origin_scale: [
                case.tx as f32 * 256.0 * scale,
                case.ty as f32 * 256.0 * scale,
                scale,
                case.w as f32,
            ],
            info: [commands.len() as u32, 0, case.h, 0],
        }];
        let commands_buf =
            upload_buffer(device, &self.queue, &commands, wgpu::BufferUsages::STORAGE);
        let tiles_buf = upload_buffer(device, &self.queue, &entries, wgpu::BufferUsages::STORAGE);
        let config_buf = upload_buffer(device, &self.queue, &config, wgpu::BufferUsages::UNIFORM);
        let atlas_view = atlas.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let output_view = output.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("parity composite"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: commands_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: tiles_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: config_buf.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("parity composite"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("parity composite"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(16, 16, 1);
        }
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("parity readback"),
            size: 256 * 1024,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &read,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(1024),
                    rows_per_image: Some(256),
                },
            },
            wgpu::Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: 1,
            },
        );
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        self.queue.submit(Some(encoder.finish()));
        map_readback(device, &read)?;
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(format!("validation: {err}"));
        }
        let bytes = read
            .slice(..)
            .get_mapped_range()
            .map_err(|e| e.to_string())?
            .to_vec();
        read.unmap();
        Ok(bytes)
    }
}

fn upload_buffer<T: Pod + Zeroable>(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    data: &[T],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let zero = T::zeroed();
    let bytes: &[u8] = if data.is_empty() {
        bytemuck::bytes_of(&zero)
    } else {
        bytemuck::cast_slice(data)
    };
    let size = (bytes.len() as u64).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("parity storage"),
        size,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut padded = bytes.to_vec();
    padded.resize(size as usize, 0);
    queue.write_buffer(&buffer, 0, &padded);
    buffer
}

fn map_readback(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    for _ in 0..1000 {
        match rx.try_recv() {
            Ok(result) => return result.map_err(|e| format!("map: {e}")),
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .map_err(|e| format!("poll: {e}"))?;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err("map callback dropped".into());
            }
        }
    }
    Err("GPU readback did not complete".into())
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
    let q = integer_inverse(inv).unwrap();
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
    let far_q = integer_inverse(far_inv).unwrap();
    let px = pixel_q8(983 * 256, 124, 1).unwrap();
    let py = pixel_q8(0, 0, 1).unwrap();
    assert!(
        source_q8(&far_q, px, py).is_none(),
        "source Q8 past ±2^23 px must not wrap into i32"
    );
    let far_case = cases()
        .into_iter()
        .find(|c| c.name == "large-translation-out-of-range")
        .unwrap();
    let tile = render(&far_case);
    assert!(
        tile.iter().all(|&b| b == 0),
        "out-of-range source must be transparent, not a wrapped texel"
    );
    assert!(pixel_q8(8_388_608, 0, 1).is_none());
    assert_eq!(pixel_q8(0, 0, 1), Some(128));
}

/// Documents saved on current main have no `render_version`. They load as version 1
/// and the float sampler reproduces `tests/parity/render-v1.txt`, which is the
/// hash file main recorded. Version 2 changes `scale-bilinear` and `rotation`
/// only; every other locked case stays bit-exact, and re-saving a version 1
/// document does not add the field.
#[test]
fn pre_bump_documents_match_the_recorded_main_hashes() {
    let mut meta = alto_doc::DocumentMeta::srgb_u8(512, 512);
    meta.render_version = RENDER_VERSION_F32;
    meta.format_version = 1;
    let doc = alto_doc::Document::new(meta).unwrap();
    let bytes = doc.snapshot_bytes().unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(
        !text.contains("render_version"),
        "version 1 snapshots omit the field, which is how main saved them"
    );
    let loaded = alto_doc::Document::from_snapshot_bytes(&bytes).unwrap();
    assert_eq!(loaded.meta.render_version, RENDER_VERSION_F32);
    assert_eq!(
        loaded.snapshot_bytes().unwrap(),
        bytes,
        "opening and saving a pre-bump document keeps its snapshot bytes"
    );
    assert!(
        text.contains("\"format_version\": 1"),
        "a version 1 document keeps format 1"
    );
    let fresh = alto_doc::Document::new(alto_doc::DocumentMeta::srgb_u8(8, 8)).unwrap();
    assert_eq!(fresh.meta.render_version, RENDER_VERSION);
    let fresh_text = String::from_utf8(fresh.snapshot_bytes().unwrap()).unwrap();
    assert!(fresh_text.contains("\"render_version\": 2"));
    assert!(
        fresh_text.contains("\"format_version\": 3"),
        "render version 2 is a new file format so a pre-#10 build refuses it"
    );
    assert!(!fresh_text.contains("\"format_version\": 1"));
    assert!(!fresh_text.contains("\"format_version\": 2"));

    let recorded = std::fs::read_to_string(render_v1_path()).unwrap();
    let expected = parse_reference(&recorded);
    assert_eq!(
        expected.len(),
        34,
        "main locked 34 cases and omitted rotation"
    );
    let catalog: std::collections::BTreeMap<_, _> = cases()
        .into_iter()
        .map(|case| (case.name.clone(), case))
        .collect();
    for (name, hash, fnv) in &expected {
        let case = &catalog[name];
        let tile = render_at(case, loaded.meta.render_version);
        assert_eq!(ContentHash::of(&tile).hex(), *hash, "{name}");
        assert_eq!(format!("{:016x}", fnv64(&tile)), *fnv, "{name}");
        let version2 = render_at(case, RENDER_VERSION);
        if name == "scale-bilinear" {
            assert_ne!(
                tile, version2,
                "scale-bilinear is the output render version 2 is expected to change"
            );
        } else {
            assert_eq!(
                tile, version2,
                "{name} stays bit-exact; only scale-bilinear and rotation change"
            );
        }
    }
    let rotation = catalog.get("rotation").unwrap();
    let old = render_at(rotation, RENDER_VERSION_F32);
    let new = render_at(rotation, RENDER_VERSION);
    assert_ne!(
        old, new,
        "rotation is the other output render version 2 is expected to change"
    );
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
    let effects = scene(
        RContent::Tiles(Vec::new()),
        RFeatures {
            effects: vec![REffect {
                kind: REffectKind::GaussianBlur { radius: 8.0 },
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
    }
    // The plain compositor skips an unsupported node, so the tile stays exact.
    assert!(plain_scene(&unsupported));
    assert_eq!(gap(&unsupported), Some("unsupported"));
}

#[test]
fn gpu_shader_matches_cpu_reference() {
    let gpu = match Gpu::new() {
        Ok(Some(gpu)) => gpu,
        Ok(None) => {
            if cfg!(target_os = "macos") {
                panic!(
                    "macOS CI must expose a Metal, Vulkan, or DX12 adapter for GPU pixel parity"
                );
            }
            eprintln!("gpu pixel parity skipped: no Metal, Vulkan, or DX12 adapter");
            return;
        }
        Err(err) => panic!("GPU pixel parity could not start: {err}"),
    };
    let mut failed = Vec::new();
    for case in cases() {
        let cpu = render(&case);
        let gpu_tile = match gpu.render(&case) {
            Ok(bytes) => bytes,
            Err(err) => {
                println!("{}: GPU render failed: {err}", case.name);
                failed.push(format!("{}: {err}", case.name));
                continue;
            }
        };
        let report = diff_report(&cpu, &gpu_tile);
        println!("{}: {report}", case.name);
        if cpu != gpu_tile {
            failed.push(format!("{}: {report}", case.name));
        }
    }
    assert!(
        failed.is_empty(),
        "GPU and CPU pixels differ:\n{}",
        failed.join("\n")
    );
}
