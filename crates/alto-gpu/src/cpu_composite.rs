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
//! Masks, clips, blur, vectors and text are not on the composite path. `composite.wgsl`
//! never sees them (`plain_scene` is false, and the effect pass uses rgba16float).
//! Color adjust is the exception: the reference repeats `effects.wgsl` mode 0, the
//! normal blend onto clear, mode 3, and `effects-resolve.wgsl`, with an f16 round trip
//! after every store. The shader is unchanged, so render version 2 pixels stay put.
//! The parity image is a 256² gradient (every R, G, B and alpha, including alpha
//! 0, 1, 128 and 254) run through the contrast × saturation × brightness grid.
//! The other four stay explicit xfails in the browser suite.

use crate::blend_ref::{blend, effects_blend};
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

#[derive(Clone)]
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
        /// Tiles merged after the image. The out-of-range case plants one at the
        /// coordinate a truncated i32 would wrap to, which an imported image
        /// cannot reach (width is at most 32768).
        extra_tiles: Vec<Tile>,
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

#[derive(Clone)]
struct Case {
    name: String,
    w: u32,
    h: u32,
    level: i32,
    tx: i32,
    ty: i32,
    /// Written to `tests/parity/cpu-reference.txt` (render version 2).
    lock: bool,
    /// Semi-transparent coverage. Printed and saved, and a mismatch does not fail.
    measure: bool,
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
        extra_tiles: Vec::new(),
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
            measure: false,
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
        measure: false,
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
        measure: false,
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
        measure: false,
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
        measure: false,
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
        measure: false,
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
        measure: false,
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
            measure: false,
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
        measure: false,
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
        measure: false,
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
        measure: false,
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
        measure: false,
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
        measure: false,
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
        measure: false,
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
            measure: false,
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
        measure: false,
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
        measure: false,
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
        measure: false,
        nodes: vec![raster(
            ramp(256, 256),
            0,
            1.0,
            true,
            [0.1, 0.0, 0.0, 0.1, 4000.0, 0.0],
        )],
    });
    // Scale 0.01 and a 84000px move invert to −8400000px. On the origin tile
    // the rounded source Q8 of the left pixels is past −2^23 px. Truncating
    // it to i32 would wrap pixel (0, 0) onto source (8377265, 50), tile
    // (32723, 0) local (177, 50). That texel is planted below. The canvas
    // stays 256px (documents are limited to 65536). The sample is transparent.
    let mut probe = vec![0u8; 256 * 256 * 4];
    let probe_i = (50 * 256 + 177) * 4;
    probe[probe_i..probe_i + 4].copy_from_slice(&[255, 0, 0, 255]);
    let mut far = raster(
        ramp(256, 256),
        0,
        1.0,
        true,
        [0.01, 0.0, 0.0, 0.01, 84_000.0, 0.0],
    );
    if let Node::Raster { extra_tiles, .. } = &mut far {
        extra_tiles.push(Tile {
            tx: 32_723,
            ty: 0,
            pixels: probe,
        });
    }
    out.push(Case {
        name: "large-translation-out-of-range".into(),
        w: 256,
        h: 256,
        level: 0,
        tx: 0,
        ty: 0,
        lock: true,
        measure: false,
        nodes: vec![far],
    });
    // Measured, not enforced, and omitted from the locked CPU reference.
    // Each layer gets its own straight sweep: alpha phase offset by the layer
    // index, and RGB rotated and shifted so neighboring layers are not copies.
    let enforced = out.len();
    for index in 0..enforced {
        let mut variant = out[index].clone();
        variant.name = format!("{}-alpha-measure", variant.name);
        variant.lock = false;
        variant.measure = true;
        paint_alpha_sweep(&mut variant.nodes);
        out.push(variant);
    }
    out
}

/// Straight RGBA for one layer in paint order.
///
/// Layer 0 keeps R = `x & 255`, G = `y & 255`, B = `(3x+5y) & 255`. Layer `i`
/// rotates those channels by `i % 3` and adds `17*i`, so two layers never share
/// an RGB triple. Alpha is one of 0, 1, 128, 254. Even layers step with `x+y`,
/// odd layers step with `x` only, and both add the layer index as a phase.
/// Layers 0 and 1 therefore hit every pair of those four alphas.
fn alpha_sweep(w: u32, h: u32, layer: u32) -> Image {
    const ALPHAS: [u8; 4] = [0, 1, 128, 254];
    let rot = (layer % 3) as usize;
    let shift = ((layer * 17) & 255) as u8;
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let base = [
                (x & 255) as u8,
                (y & 255) as u8,
                (x.wrapping_mul(3).wrapping_add(y.wrapping_mul(5)) & 255) as u8,
            ];
            let i = ((y * w + x) * 4) as usize;
            pixels[i] = base[rot].wrapping_add(shift);
            pixels[i + 1] = base[(1 + rot) % 3].wrapping_add(shift);
            pixels[i + 2] = base[(2 + rot) % 3].wrapping_add(shift);
            let phase = if layer.is_multiple_of(2) {
                x + y + layer
            } else {
                x + layer
            };
            pixels[i + 3] = ALPHAS[(phase % 4) as usize];
        }
    }
    Image { w, h, pixels }
}

fn paint_alpha_sweep(nodes: &mut [Node]) {
    let mut layer = 0u32;
    paint_alpha_sweep_at(nodes, &mut layer);
}

fn paint_alpha_sweep_at(nodes: &mut [Node], layer: &mut u32) {
    for node in nodes {
        match node {
            Node::Raster {
                image, extra_tiles, ..
            } => {
                let index = *layer;
                *layer += 1;
                *image = alpha_sweep(image.w, image.h, index);
                for tile in extra_tiles {
                    tile.pixels = alpha_sweep(256, 256, index).pixels;
                }
            }
            Node::Group { children, .. } => paint_alpha_sweep_at(children, layer),
        }
    }
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
        Node::Raster {
            image,
            mode,
            extra_tiles,
            ..
        } => {
            let Some(inv) = Affine2(world).inverse() else {
                return;
            };
            let mut tiles = split_tiles(image);
            tiles.extend(extra_tiles.iter().cloned());
            tiles.sort_by_key(|t| (t.tx, t.ty));
            ops.push(Op::Draw {
                inv: inv.0,
                opacity,
                mode: *mode,
                tiles,
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

/// Round-ties-to-even f32→f16. Matches the `rgba16float` stores in `effects.wgsl`.
fn f32_to_f16_bits(value: f32) -> u16 {
    let x = value.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let mantissa = x & 0x007f_ffff;
    let exp = (x & 0x7f80_0000) >> 23;
    if exp == 0xff {
        return if mantissa == 0 {
            sign | 0x7c00
        } else {
            let payload = (mantissa >> 13) as u16;
            sign | 0x7c00 | payload | u16::from(payload == 0)
        };
    }
    let e = exp as i32 - 127 + 15;
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = mantissa | 0x0080_0000;
        let shift = (14 - e) as u32;
        let mut half = (m >> shift) as u16;
        let rem_mask = (1u32 << shift) - 1;
        let rem = m & rem_mask;
        let halfway = 1u32 << (shift - 1);
        if rem > halfway || (rem == halfway && (half & 1) == 1) {
            half += 1;
        }
        return sign | half;
    }
    if e >= 31 {
        return sign | 0x7c00;
    }
    let mut half = ((mantissa >> 13) as u16) | ((e as u16) << 10);
    let rem = mantissa & 0x1fff;
    if rem > 0x1000 || (rem == 0x1000 && (half & 1) == 1) {
        half += 1;
    }
    sign | half
}

fn f16_to_f32(half: u16) -> f32 {
    let sign = ((half as u32) & 0x8000) << 16;
    let exp = ((half >> 10) & 0x1f) as u32;
    let mant = (half & 0x3ff) as u32;
    let bits = if exp == 0 {
        if mant == 0 {
            sign
        } else {
            let mut m = mant;
            let mut e = 113u32;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            m &= 0x3ff;
            sign | (e << 23) | (m << 13)
        }
    } else if exp == 31 {
        sign | 0x7f80_0000 | (mant << 13)
    } else {
        sign | ((exp + (127 - 15)) << 23) | (mant << 13)
    };
    f32::from_bits(bits)
}

fn f16_round(value: f32) -> f32 {
    f16_to_f32(f32_to_f16_bits(value))
}

/// 256² straight RGBA. R is x, G is y, B is `(x * 3 + y * 5) & 255` (every row
/// hits every blue). Alpha is 0, 1, 128, 254 on the first four rows and x after that,
/// so each channel, including those four alpha anchors, appears.
fn color_adjust_gradient() -> Vec<u8> {
    let mut pixels = vec![0u8; 256 * 256 * 4];
    for y in 0..256u32 {
        for x in 0..256u32 {
            let i = ((y * 256 + x) * 4) as usize;
            pixels[i] = x as u8;
            pixels[i + 1] = y as u8;
            pixels[i + 2] = ((x * 3 + y * 5) & 255) as u8;
            pixels[i + 3] = match y {
                0 => 0,
                1 => 1,
                2 => 128,
                3 => 254,
                _ => x as u8,
            };
        }
    }
    pixels
}

/// Contrast, saturation, brightness. The cast is `f64 as f32`, the same one
/// `render_effect` uses when a document number becomes the mode 3 uniform.
/// The first 12 entries are the interior cross product. The last 8 are the
/// document-range endpoints.
fn color_adjust_grid() -> [(f32, f32, f32); 20] {
    const INTERIOR_CONTRAST: [f32; 2] = [0.5f64 as f32, 1.7f64 as f32];
    const INTERIOR_SATURATION: [f32; 3] = [0.0f64 as f32, 0.35f64 as f32, 1.6f64 as f32];
    const INTERIOR_BRIGHTNESS: [f32; 2] = [(-0.35f64) as f32, 0.45f64 as f32];
    const END_CONTRAST: [f32; 2] = [0.0f64 as f32, 4.0f64 as f32];
    const END_SATURATION: [f32; 2] = [0.0f64 as f32, 4.0f64 as f32];
    const END_BRIGHTNESS: [f32; 2] = [(-1.0f64) as f32, 1.0f64 as f32];
    let mut out = [(0.0f32, 0.0, 0.0); 20];
    let mut i = 0;
    for contrast in INTERIOR_CONTRAST {
        for saturation in INTERIOR_SATURATION {
            for brightness in INTERIOR_BRIGHTNESS {
                out[i] = (contrast, saturation, brightness);
                i += 1;
            }
        }
    }
    for contrast in END_CONTRAST {
        for saturation in END_SATURATION {
            for brightness in END_BRIGHTNESS {
                out[i] = (contrast, saturation, brightness);
                i += 1;
            }
        }
    }
    assert_eq!(i, 20);
    out
}

fn color_adjust_case_name(contrast: f32, saturation: f32, brightness: f32) -> String {
    format!("color-adjust c={contrast} s={saturation} b={brightness}")
}

const Q14_ONE: i32 = 16384;
const Q14_HALF: i32 = 8192;
/// Luma weights in Q14. They sum to [`Q14_ONE`].
const Q14_LUMA: [i32; 3] = [3483, 11718, 1183];

/// f16 bits to Q14, truncated toward zero and clamped to 0..1. Negatives become
/// 0, inf becomes 1, NaN becomes 0, and subnormals become 0.
fn f16_bits_to_q14(half: u16) -> u32 {
    let sign = half & 0x8000;
    let exp = ((half >> 10) & 0x1f) as u32;
    let mant = (half & 0x3ff) as u32;
    if sign != 0 {
        return 0;
    }
    if exp == 31 {
        return if mant == 0 { Q14_ONE as u32 } else { 0 };
    }
    if exp == 0 {
        return 0;
    }
    let sig = 1024 + mant;
    let q = if exp >= 11 {
        sig << (exp - 11)
    } else {
        sig >> (11 - exp)
    };
    q.min(Q14_ONE as u32)
}

fn floor_log2(mut value: u32) -> u32 {
    let mut p = 0u32;
    if value >= 256 {
        p += 8;
        value >>= 8;
    }
    if value >= 16 {
        p += 4;
        value >>= 4;
    }
    if value >= 4 {
        p += 2;
        value >>= 2;
    }
    if value >= 2 {
        p += 1;
    }
    p
}

/// Q14 in 0..1 to f16 bits, round ties to even.
fn q14_to_f16_bits(q: u32) -> u16 {
    if q == 0 {
        return 0;
    }
    let q = q.min(Q14_ONE as u32);
    if q == Q14_ONE as u32 {
        return 0x3c00;
    }
    let p = floor_log2(q);
    let (sig, exp_p) = if p >= 10 {
        let cut = p - 10;
        let mut sig = q;
        if cut > 0 {
            let half = 1u32 << (cut - 1);
            let rem = sig & ((1u32 << cut) - 1);
            sig >>= cut;
            if rem > half || (rem == half && (sig & 1) == 1) {
                sig += 1;
            }
        }
        if sig >= 2048 {
            (1024u32, p + 1)
        } else {
            (sig, p)
        }
    } else {
        (q << (10 - p), p)
    };
    let mant = (sig & 1023) as u16;
    let exp = (exp_p + 1) as u16;
    (exp << 10) | mant
}

/// `(n + d/2) / d` for `d > 0`, with the sign taken so a tie moves away from zero.
fn div_q_i64(n: i64, d: i64) -> Option<i64> {
    let half = d / 2;
    if n >= 0 {
        n.checked_add(half)?.checked_div(d)
    } else {
        n.checked_neg()?
            .checked_add(half)?
            .checked_div(d)?
            .checked_neg()
    }
}

/// `luma * (16384 - saturation) + channel * saturation`.
fn saturation_mix_sum(luma: i64, channel: i64, saturation: i64) -> Option<i64> {
    let gap = i64::from(Q14_ONE).checked_sub(saturation)?;
    luma.checked_mul(gap)?
        .checked_add(channel.checked_mul(saturation)?)
}

/// Mode 17. `q` is premultiplied Q14 plus alpha. Settings are already Q14.
/// The returned channels are premultiplied Q14; alpha is not part of the result
/// because the stored alpha keeps the input f16 bits.
///
/// Straight channels are 0..=16384. `color_adjust_q14` quantizes a setting into
/// −16384..=65536 (the f32 is clamped to [−1, 4] first). Brightness in the
/// document is −1..=1, so its Q14 value is −16384..=16384. The contrast stage
/// is not clamped: `(straight - 8192) * contrast / 16384 + 8192 + brightness`
/// lands in −40960..=57344, and luma stays inside that span. The saturation
/// sum, treating luma and the channel as independent inside the span and
/// saturation inside −16384..=65536, is −5502926848..=5771362304. That fits
/// i64 and does not fit i32. The only clamp is 0..=16384 after the mix, which
/// matches `clamp(rgb, 0, 1)`.
fn color_adjust_exact_q14_checked(
    q: [u32; 4],
    brightness: i32,
    contrast: i32,
    saturation: i32,
) -> Option<[u32; 3]> {
    let alpha = i64::from(q[3]);
    let one = i64::from(Q14_ONE);
    let half = i64::from(Q14_HALF);
    let brightness = i64::from(brightness);
    let contrast = i64::from(contrast);
    let saturation = i64::from(saturation);
    let mut straight = [0i64; 3];
    if alpha != 0 {
        for i in 0..3 {
            let s = i64::from(q[i]).checked_shl(14)?.checked_div(alpha)?;
            straight[i] = s.clamp(0, one);
        }
    }
    let mut rgb = [0i64; 3];
    for i in 0..3 {
        rgb[i] = div_q_i64(straight[i].checked_sub(half)?.checked_mul(contrast)?, one)?
            .checked_add(half)?
            .checked_add(brightness)?;
    }
    let mut acc = 0i64;
    for i in 0..3 {
        acc = acc.checked_add(rgb[i].checked_mul(i64::from(Q14_LUMA[i]))?)?;
    }
    let luma = div_q_i64(acc, one)?;
    let mut out = [0u32; 3];
    for i in 0..3 {
        let mixed = div_q_i64(saturation_mix_sum(luma, rgb[i], saturation)?, one)?;
        let premul = div_q_i64(mixed.clamp(0, one).checked_mul(alpha)?, one)?;
        out[i] = u32::try_from(premul).ok()?;
    }
    Some(out)
}

fn color_adjust_exact_q14(
    q: [u32; 4],
    brightness: i32,
    contrast: i32,
    saturation: i32,
) -> [u32; 3] {
    color_adjust_exact_q14_checked(q, brightness, contrast, saturation)
        .expect("color-adjust Q14 products fit in i64")
}

/// One texel through the color-adjust passes: mode 0 premultiply, normal blend
/// onto clear, mode 3, f16 between stores, unorm8 resolve.
fn color_adjust_texel(rgba: [u8; 4], brightness: f32, contrast: f32, saturation: f32) -> [u8; 4] {
    let straight = rgba.map(|b| f32::from(b) / 255.0);
    let premul = [
        f16_round(straight[0] * straight[3]),
        f16_round(straight[1] * straight[3]),
        f16_round(straight[2] * straight[3]),
        f16_round(straight[3]),
    ];
    let blended = effects_blend([0.0; 4], premul, 0).map(f16_round);
    let eps = 0.000001f32;
    let alpha = blended[3];
    let scale = alpha.max(eps);
    let mut rgb = [blended[0] / scale, blended[1] / scale, blended[2] / scale];
    for channel in &mut rgb {
        *channel = (*channel - 0.5) * contrast + 0.5 + brightness;
    }
    // `dot` as a left fold of f32 products. Saturation 1 multiplies this by zero.
    let luma = (rgb[0] * 0.2126 + rgb[1] * 0.7152) + rgb[2] * 0.0722;
    for channel in &mut rgb {
        *channel = luma * (1.0 - saturation) + *channel * saturation;
        *channel = channel.clamp(0.0, 1.0) * alpha;
    }
    quantize([
        f16_round(rgb[0]),
        f16_round(rgb[1]),
        f16_round(rgb[2]),
        f16_round(alpha),
    ])
}

/// Straight byte to Q14. `(b * 16384 + 127) / 255`, capped at 1.
fn byte_q14(byte: u32) -> u32 {
    (byte * 16384 + 127) / 255
}

/// Premultiplied Q14 of two unorm bytes. Opaque 255 stays at 1.
fn premul_byte_q14(color: u32, alpha: u32) -> u32 {
    (color * alpha * 16384 + 32_512) / 65_025
}

/// Render version 3. Recovered bytes are premultiplied in Q14, stored as f16,
/// copied through the clear normal blend, then adjusted. The alpha f16 bits
/// survive the adjustment. The unorm resolve is round-ties-to-even.
fn color_adjust_texel_v3(
    rgba: [u8; 4],
    brightness: f32,
    contrast: f32,
    saturation: f32,
) -> [u8; 4] {
    let bytes = rgba.map(u32::from);
    let alpha = bytes[3];
    let stored = [
        q14_to_f16_bits(premul_byte_q14(bytes[0], alpha)),
        q14_to_f16_bits(premul_byte_q14(bytes[1], alpha)),
        q14_to_f16_bits(premul_byte_q14(bytes[2], alpha)),
        q14_to_f16_bits(byte_q14(alpha)),
    ];
    let q = stored.map(f16_bits_to_q14);
    let rgb = color_adjust_exact_q14(
        q,
        crate::renderer::color_adjust_q14(brightness),
        crate::renderer::color_adjust_q14(contrast),
        crate::renderer::color_adjust_q14(saturation),
    );
    quantize([
        f16_to_f32(q14_to_f16_bits(rgb[0])),
        f16_to_f32(q14_to_f16_bits(rgb[1])),
        f16_to_f32(q14_to_f16_bits(rgb[2])),
        f16_to_f32(stored[3]),
    ])
}

fn color_adjust_image(src: &[u8], brightness: f32, contrast: f32, saturation: f32) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    for (dst, px) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<4>().0)
    {
        *dst = color_adjust_texel(*px, brightness, contrast, saturation);
    }
    out
}

fn color_adjust_image_v3(src: &[u8], brightness: f32, contrast: f32, saturation: f32) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    for (dst, px) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<4>().0)
    {
        *dst = color_adjust_texel_v3(*px, brightness, contrast, saturation);
    }
    out
}

fn color_adjust_tile(rgba: [u8; 4], brightness: f32, contrast: f32, saturation: f32) -> Vec<u8> {
    let mut solid = vec![0u8; 256 * 256 * 4];
    for chunk in solid.as_chunks_mut::<4>().0 {
        *chunk = rgba;
    }
    color_adjust_image(&solid, brightness, contrast, saturation)
}

fn composite(ops: &[Op], width: u32, height: u32, frame: &Frame) -> (Vec<u8>, Vec<f32>) {
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

fn render_pair(case: &Case, version: u32) -> (Vec<u8>, Vec<f32>) {
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

fn render_at(case: &Case, version: u32) -> Vec<u8> {
    render_pair(case, version).0
}

fn render(case: &Case) -> Vec<u8> {
    render_at(case, RENDER_VERSION)
}

/// CPU bytes plus the f32 accumulator immediately before `quantize`.
fn render_prequant(case: &Case) -> (Vec<u8>, Vec<f32>) {
    render_pair(case, RENDER_VERSION)
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

struct MeasuredPixel {
    x: u32,
    y: u32,
    sources: Vec<[u8; 4]>,
    gpu: [u8; 4],
    cpu: [u8; 4],
    /// Accumulator f32 on each side, immediately before quantization.
    gpu_pre: [f32; 4],
    cpu_pre: [f32; 4],
}

struct MeasuredCase {
    name: String,
    count: u32,
    max_abs: [u32; 4],
    mismatches: Vec<MeasuredPixel>,
    length_note: String,
    /// Shader operations this case actually runs. Not a diagnosis.
    ops: String,
}

fn rgba(bytes: &[u8], offset: usize) -> [u8; 4] {
    bytes[offset..offset + 4].try_into().unwrap()
}

fn push_sources(node: &Node, doc_x: i32, doc_y: i32, out: &mut Vec<[u8; 4]>) {
    match node {
        Node::Raster { image, .. } => {
            if doc_x >= 0 && doc_y >= 0 && (doc_x as u32) < image.w && (doc_y as u32) < image.h {
                let i = ((doc_y as u32 * image.w + doc_x as u32) * 4) as usize;
                out.push(rgba(&image.pixels, i));
            }
        }
        Node::Group { children, .. } => {
            for child in children {
                push_sources(child, doc_x, doc_y, out);
            }
        }
    }
}

/// Straight image texels at the output pixel's document position. Identity
/// layers use this texel. A transformed layer samples somewhere else.
fn sources_at(case: &Case, x: u32, y: u32) -> Vec<[u8; 4]> {
    let scale = 1i32 << case.level.max(0);
    let doc_x = case.tx * 256 * scale + x as i32 * scale;
    let doc_y = case.ty * 256 * scale + y as i32 * scale;
    let mut sources = Vec::new();
    for node in &case.nodes {
        push_sources(node, doc_x, doc_y, &mut sources);
    }
    sources
}

fn fmt_rgba(px: [u8; 4]) -> String {
    format!("{},{},{},{}", px[0], px[1], px[2], px[3])
}

fn fmt_sources(sources: &[[u8; 4]]) -> String {
    if sources.is_empty() {
        return "none".into();
    }
    sources
        .iter()
        .map(|px| fmt_rgba(*px))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn pre_at(values: &[f32], pixel: usize) -> [f32; 4] {
    let o = pixel * 4;
    if o + 4 <= values.len() {
        [values[o], values[o + 1], values[o + 2], values[o + 3]]
    } else {
        [f32::NAN; 4]
    }
}

fn measure_case(
    case: &Case,
    cpu: &[u8],
    gpu: &[u8],
    cpu_pre: &[f32],
    gpu_pre: &[f32],
) -> MeasuredCase {
    let mut count = 0u32;
    let mut max_abs = [0u32; 4];
    let mut mismatches = Vec::new();
    let n = cpu.len().min(gpu.len()) / 4;
    for i in 0..n {
        let o = i * 4;
        if cpu[o..o + 4] == gpu[o..o + 4] {
            continue;
        }
        count += 1;
        for channel in 0..4 {
            max_abs[channel] =
                max_abs[channel].max(u32::from(cpu[o + channel].abs_diff(gpu[o + channel])));
        }
        let x = (i % 256) as u32;
        let y = (i / 256) as u32;
        mismatches.push(MeasuredPixel {
            x,
            y,
            sources: sources_at(case, x, y),
            gpu: rgba(gpu, o),
            cpu: rgba(cpu, o),
            gpu_pre: pre_at(gpu_pre, i),
            cpu_pre: pre_at(cpu_pre, i),
        });
    }
    let length_note = if cpu.len() == gpu.len() {
        String::new()
    } else {
        format!(" lengths cpu {} gpu {}", cpu.len(), gpu.len())
    };
    MeasuredCase {
        name: case.name.clone(),
        count,
        max_abs,
        mismatches,
        length_note,
        ops: shader_ops(case),
    }
}

fn shader_ops(case: &Case) -> String {
    let mut parts = Vec::new();
    fn walk(nodes: &[Node], parts: &mut Vec<String>) {
        for node in nodes {
            match node {
                Node::Raster { mode, .. } => {
                    let name = BLENDS.get(*mode as usize).copied().unwrap_or("unknown");
                    parts.push(format!(
                        "draw blend {name}: unpremultiply dst (rgb/max(a, 0.000001)) when dst.a > 0, mix_color, three-term sum"
                    ));
                }
                Node::Group {
                    isolated, children, ..
                } => {
                    let kind = if *isolated {
                        "isolated"
                    } else {
                        "pass-through"
                    };
                    parts.push(format!(
                        "{kind} group pop: unpremultiply child accum (rgb/max(a, 0.000001)) when a > 0, scale alpha, then blend"
                    ));
                    walk(children, parts);
                }
            }
        }
    }
    walk(&case.nodes, &mut parts);
    parts.join("; ")
}

/// Distance in byte space from `v * 255` to the nearest `n + 0.5` boundary.
fn dist_to_half(v: f32) -> f32 {
    if !v.is_finite() {
        return f32::NAN;
    }
    let scaled = v.clamp(0.0, 1.0) * 255.0;
    let shifted = scaled - 0.5;
    (shifted - shifted.round()).abs()
}

/// `clamp(v, 0, 1) * 255` when that product is finite.
fn scaled_byte(v: f32) -> Option<f32> {
    if !v.is_finite() {
        return None;
    }
    let scaled = v.clamp(0.0, 1.0) * 255.0;
    scaled.is_finite().then_some(scaled)
}

/// Exact f32 equality with `n + 0.5`. Not an epsilon test.
fn exactly_half_boundary(scaled: f32) -> bool {
    let n = scaled.floor();
    scaled == n + 0.5
}

/// `floor(x + 0.5)` on a non-negative byte-scaled value. At an exact tie this is
/// `n + 1`. Elsewhere it is the nearest integer.
fn round_half_up_byte(scaled: f32) -> u8 {
    (scaled + 0.5).floor().clamp(0.0, 255.0) as u8
}

fn round_ties_even_byte(scaled: f32) -> u8 {
    scaled.round_ties_even().clamp(0.0, 255.0) as u8
}

/// At an exact `n + 0.5`, the odd integer of `{n, n + 1}`.
fn round_ties_odd_byte(scaled: f32) -> u8 {
    let n = scaled.floor() as u32;
    let odd = if n.is_multiple_of(2) { n + 1 } else { n };
    odd.min(255) as u8
}

struct ChannelBoundary {
    /// Pre-store f32 bits match. The byte still differs; the caller checked that.
    bits_equal: bool,
    exact_half: bool,
    /// GPU byte is round-half-up and CPU byte is `round_ties_even`, and those
    /// two bytes are different.
    tie_even_vs_half_up: bool,
    /// Exact tie, and the pair is not tie-even versus round-half-up.
    exact_half_not_half_up: bool,
    /// Bit-identical pre-store value whose `v * 255` is not exactly `n + 0.5`.
    not_exact_half: bool,
    /// Exact tie and the GPU byte is round-ties-to-odd.
    gpu_ties_odd: bool,
    class: &'static str,
}

fn classify_boundary(gpu_v: f32, cpu_v: f32, gpu_byte: u8, cpu_byte: u8) -> ChannelBoundary {
    if gpu_v.to_bits() != cpu_v.to_bits() {
        return ChannelBoundary {
            bits_equal: false,
            exact_half: false,
            tie_even_vs_half_up: false,
            exact_half_not_half_up: false,
            not_exact_half: false,
            gpu_ties_odd: false,
            class: "floats-differ",
        };
    }
    let Some(scaled) = scaled_byte(gpu_v) else {
        return ChannelBoundary {
            bits_equal: true,
            exact_half: false,
            tie_even_vs_half_up: false,
            exact_half_not_half_up: false,
            not_exact_half: true,
            gpu_ties_odd: false,
            class: "not-exact-half",
        };
    };
    let ties_even = round_ties_even_byte(scaled);
    let half_up = round_half_up_byte(scaled);
    if !exactly_half_boundary(scaled) {
        return ChannelBoundary {
            bits_equal: true,
            exact_half: false,
            tie_even_vs_half_up: false,
            exact_half_not_half_up: false,
            not_exact_half: true,
            gpu_ties_odd: false,
            class: "not-exact-half",
        };
    }
    let tie_even_vs_half_up = half_up != ties_even && gpu_byte == half_up && cpu_byte == ties_even;
    ChannelBoundary {
        bits_equal: true,
        exact_half: true,
        tie_even_vs_half_up,
        exact_half_not_half_up: !tie_even_vs_half_up,
        not_exact_half: false,
        gpu_ties_odd: gpu_byte == round_ties_odd_byte(scaled),
        class: if tie_even_vs_half_up {
            "tie-even-vs-half-up"
        } else {
            "exact-half-not-half-up"
        },
    }
}

fn json_f32(v: f32) -> String {
    if v.is_finite() {
        format!("{v:.8e}")
    } else {
        "null".into()
    }
}

fn measure_line(row: &MeasuredCase) -> String {
    let pixels = row
        .mismatches
        .iter()
        .take(5)
        .map(|pixel| {
            format!(
                "pixel {},{} source {} gpu {} cpu {}",
                pixel.x,
                pixel.y,
                fmt_sources(&pixel.sources),
                fmt_rgba(pixel.gpu),
                fmt_rgba(pixel.cpu)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let pixels = if pixels.is_empty() {
        String::new()
    } else {
        format!("; {pixels}")
    };
    format!(
        "MEASURED-NOT-ENFORCED {}: {} pixels differ; max abs {},{},{},{}{}{}",
        row.name,
        row.count,
        row.max_abs[0],
        row.max_abs[1],
        row.max_abs[2],
        row.max_abs[3],
        pixels,
        row.length_note
    )
}

fn json_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The `unorm_byte` that color-adjust mode 18 uses (`effects.wgsl`). The atlas-read
/// measurement runs this exact text; `atlas_read_uses_effects_unorm_byte` checks it.
const UNORM_BYTE_WGSL: &str = "fn unorm_byte(v: f32) -> u32 {
  return u32(clamp(v, 0.0, 1.0) * 255.0 + 0.5);
}";

/// Texel `i` of the 256x1 atlas-read texture. Every channel sweeps all 256 bytes.
fn atlas_read_texel(i: usize) -> [u8; 4] {
    let i = i as u8;
    [i, 255 - i, i ^ 0x55, i.wrapping_add(128)]
}

fn atlas_read_shader() -> String {
    format!(
        "@group(0) @binding(0) var atlas: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> out: array<u32, 1024>;
{UNORM_BYTE_WGSL}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3u) {{
  if (id.x >= 256u) {{ return; }}
  let v = textureLoad(atlas, vec2u(id.x, 0u), 0);
  out[id.x * 4u] = unorm_byte(v.r);
  out[id.x * 4u + 1u] = unorm_byte(v.g);
  out[id.x * 4u + 2u] = unorm_byte(v.b);
  out[id.x * 4u + 3u] = unorm_byte(v.a);
}}
"
    )
}

/// Result of reading all 256 bytes from an `Rgba8Unorm` texture with
/// `textureLoad` + `unorm_byte`. Measured-not-enforced.
struct AtlasRead {
    mismatches: Vec<(usize, usize, u8, u32)>,
}

impl AtlasRead {
    fn line(&self) -> String {
        if self.mismatches.is_empty() {
            "atlas-read (measured-not-enforced): 256/256 byte values round-trip exactly on all 4 channels".into()
        } else {
            let shown: Vec<String> = self
                .mismatches
                .iter()
                .take(8)
                .map(|(t, c, want, got)| format!("texel {t} ch {c} byte {want} read {got}"))
                .collect();
            format!(
                "atlas-read (measured-not-enforced): {} channel mismatches: {}",
                self.mismatches.len(),
                shown.join("; ")
            )
        }
    }
}

fn write_atlas_read(adapter: &str, result: &Result<AtlasRead, String>) {
    let Some(dir) = std::env::var_os("ALTO_ALPHA_MEASURE_OUT") else {
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("create alpha measure directory");
    let (status, count, error) = match result {
        Ok(r) if r.mismatches.is_empty() => ("exact", 0, String::new()),
        Ok(r) => ("mismatch", r.mismatches.len(), String::new()),
        Err(e) => ("error", 0, e.clone()),
    };
    let mut list = String::new();
    if let Ok(r) = result {
        for (n, (t, c, want, got)) in r.mismatches.iter().enumerate() {
            if n > 0 {
                list.push_str(", ");
            }
            list.push_str(&format!(
                "{{\"texel\": {t}, \"channel\": {c}, \"byte\": {want}, \"read\": {got}}}"
            ));
        }
    }
    let json = format!(
        "{{\n  \"label\": \"measured-not-enforced\",\n  \"backend\": \"{}\",\n  \"format\": \"Rgba8Unorm\",\n  \"read\": \"textureLoad + unorm_byte\",\n  \"values\": 256,\n  \"status\": \"{status}\",\n  \"mismatchedChannels\": {count},\n  \"error\": \"{}\",\n  \"mismatches\": [{list}]\n}}\n",
        json_escape(adapter),
        json_escape(&error)
    );
    std::fs::write(dir.join("atlas-read.json"), json).expect("write atlas read json");
    let line = match result {
        Ok(r) => r.line(),
        Err(e) => format!("atlas-read (measured-not-enforced): GPU error: {e}"),
    };
    let markdown = format!(
        "### Atlas read (`Rgba8Unorm` + `textureLoad` + `unorm_byte`)\n\nBackend: `{}`\n\n{line}\n",
        adapter
    );
    std::fs::write(dir.join("atlas-read.md"), markdown).expect("write atlas read markdown");
}

fn write_alpha_measure(adapter: &str, rows: &[MeasuredCase]) {
    let Some(dir) = std::env::var_os("ALTO_ALPHA_MEASURE_OUT") else {
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("create alpha measure directory");
    let mut json = format!(
        "{{\n  \"label\": \"measured-not-enforced\",\n  \"backend\": \"{}\",\n  \"cases\": [\n",
        json_escape(adapter)
    );
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            json.push_str(",\n");
        }
        json.push_str(&format!(
            "    {{\"name\": \"{}\", \"enforced\": false, \"count\": {}, \"maxAbs\": [{},{},{},{}], \"mismatches\": [",
            json_escape(&row.name),
            row.count,
            row.max_abs[0],
            row.max_abs[1],
            row.max_abs[2],
            row.max_abs[3]
        ));
        for (pixel_index, pixel) in row.mismatches.iter().enumerate() {
            if pixel_index > 0 {
                json.push_str(", ");
            }
            let sources = pixel
                .sources
                .iter()
                .map(|px| format!("[{},{},{},{}]", px[0], px[1], px[2], px[3]))
                .collect::<Vec<_>>()
                .join(", ");
            json.push_str(&format!(
                "{{\"x\": {}, \"y\": {}, \"sources\": [{sources}], \"gpu\": [{},{},{},{}], \"cpu\": [{},{},{},{}]}}",
                pixel.x,
                pixel.y,
                pixel.gpu[0],
                pixel.gpu[1],
                pixel.gpu[2],
                pixel.gpu[3],
                pixel.cpu[0],
                pixel.cpu[1],
                pixel.cpu[2],
                pixel.cpu[3]
            ));
        }
        json.push_str("]}");
    }
    json.push_str("\n  ]\n}\n");
    let mut markdown = format!(
        "### Semi-transparent parity ({})\n\nThese cases are **measured-not-enforced**. A mismatch does not fail the job.\n\nBackend: `{adapter}`\n\n| case | mismatched pixels | max abs r,g,b,a | first mismatches |\n| --- | ---: | --- | --- |\n",
        json_escape(adapter)
    );
    for row in rows {
        let shown = row
            .mismatches
            .iter()
            .take(3)
            .map(|pixel| {
                format!(
                    "({},{}) source {} gpu {} cpu {}",
                    pixel.x,
                    pixel.y,
                    fmt_sources(&pixel.sources),
                    fmt_rgba(pixel.gpu),
                    fmt_rgba(pixel.cpu)
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let shown = if shown.is_empty() {
            "none".into()
        } else {
            shown
        };
        markdown.push_str(&format!(
            "| {} | {} | {},{},{},{} | {}{} |\n",
            row.name,
            row.count,
            row.max_abs[0],
            row.max_abs[1],
            row.max_abs[2],
            row.max_abs[3],
            shown,
            row.length_note
        ));
    }
    std::fs::write(dir.join("alpha-measure.json"), json).expect("write alpha measure json");
    std::fs::write(dir.join("alpha-measure.md"), markdown).expect("write alpha measure markdown");
    write_prequant(&dir, adapter, rows);
}

/// Every mismatching channel: pre-quantization f32 on both sides, and the
/// distance of each from the nearest `n+0.5` boundary. Epsilon is 1e-3 in
/// byte space (`channel * 255`).
fn write_prequant(dir: &std::path::Path, adapter: &str, rows: &[MeasuredCase]) {
    const EPS: f32 = 1.0e-3;
    const TIGHTER: f32 = 1.0e-4;
    let channels = ["r", "g", "b", "a"];
    let mut differing = 0u32;
    let mut on_boundary = 0u32;
    let mut on_boundary_tight = 0u32;
    let mut floats_within = 0u32;
    let mut floats_apart = 0u32;
    let mut bit_identical = 0u32;
    let mut exact_half = 0u32;
    let mut tie_even_vs_half_up = 0u32;
    let mut exact_half_not_half_up = 0u32;
    let mut not_exact_half = 0u32;
    let mut gpu_ties_odd = 0u32;
    let mut max_delta = 0f32;
    let mut lines = Vec::new();
    for row in rows {
        for pixel in &row.mismatches {
            for (index, name) in channels.iter().enumerate() {
                if pixel.gpu[index] == pixel.cpu[index] {
                    continue;
                }
                differing += 1;
                let gpu_v = pixel.gpu_pre[index];
                let cpu_v = pixel.cpu_pre[index];
                let boundary = classify_boundary(gpu_v, cpu_v, pixel.gpu[index], pixel.cpu[index]);
                let gpu_dist = dist_to_half(gpu_v);
                let cpu_dist = dist_to_half(cpu_v);
                let nearer = gpu_dist.min(cpu_dist);
                let delta = ((gpu_v.clamp(0.0, 1.0) - cpu_v.clamp(0.0, 1.0)) * 255.0).abs();
                if nearer <= EPS {
                    on_boundary += 1;
                }
                if nearer <= TIGHTER {
                    on_boundary_tight += 1;
                }
                if delta <= EPS {
                    floats_within += 1;
                } else {
                    floats_apart += 1;
                }
                if boundary.bits_equal {
                    bit_identical += 1;
                }
                if boundary.exact_half {
                    exact_half += 1;
                }
                if boundary.tie_even_vs_half_up {
                    tie_even_vs_half_up += 1;
                }
                if boundary.exact_half_not_half_up {
                    exact_half_not_half_up += 1;
                }
                if boundary.not_exact_half {
                    not_exact_half += 1;
                }
                if boundary.gpu_ties_odd {
                    gpu_ties_odd += 1;
                }
                let (ties_even_byte, half_up_byte) = scaled_byte(gpu_v)
                    .filter(|_| boundary.bits_equal)
                    .map(|scaled| (round_ties_even_byte(scaled), round_half_up_byte(scaled)))
                    .unwrap_or((0, 0));
                max_delta = max_delta.max(delta);
                lines.push(format!(
                    "    {{\"case\": \"{}\", \"x\": {}, \"y\": {}, \"channel\": \"{name}\", \"gpuByte\": {}, \"cpuByte\": {}, \"gpu\": {}, \"cpu\": {}, \"gpuBits\": {}, \"cpuBits\": {}, \"gpuDistToHalf\": {}, \"cpuDistToHalf\": {}, \"absScaledDelta\": {}, \"exactHalf\": {}, \"tiesEvenByte\": {ties_even_byte}, \"halfUpByte\": {half_up_byte}, \"boundaryClass\": \"{}\", \"ops\": \"{}\"}}",
                    json_escape(&row.name),
                    pixel.x,
                    pixel.y,
                    pixel.gpu[index],
                    pixel.cpu[index],
                    json_f32(gpu_v),
                    json_f32(cpu_v),
                    gpu_v.to_bits(),
                    cpu_v.to_bits(),
                    json_f32(gpu_dist),
                    json_f32(cpu_dist),
                    json_f32(delta),
                    if boundary.exact_half { "true" } else { "false" },
                    boundary.class,
                    json_escape(&row.ops),
                ));
            }
        }
    }
    let body = if lines.is_empty() {
        String::new()
    } else {
        lines.join(",\n")
    };
    let json = format!(
        "{{\n  \"backend\": \"{}\",\n  \"epsilonScaled\": {EPS:.0e},\n  \"tighterEpsilonScaled\": {TIGHTER:.0e},\n  \"differingChannels\": {differing},\n  \"withinEpsilonOfHalf\": {on_boundary},\n  \"withinTighterEpsilonOfHalf\": {on_boundary_tight},\n  \"prequantAbsDeltaWithinEpsilon\": {floats_within},\n  \"prequantAbsDeltaBeyondEpsilon\": {floats_apart},\n  \"bitIdenticalChannels\": {bit_identical},\n  \"bitIdenticalExactHalf\": {exact_half},\n  \"bitIdenticalTieEvenVsHalfUp\": {tie_even_vs_half_up},\n  \"bitIdenticalExactHalfNotHalfUp\": {exact_half_not_half_up},\n  \"bitIdenticalNotExactHalf\": {not_exact_half},\n  \"bitIdenticalExactHalfGpuTiesToOdd\": {gpu_ties_odd},\n  \"maxAbsScaledDelta\": {},\n  \"channels\": [\n{body}\n  ]\n}}\n",
        json_escape(adapter),
        json_f32(max_delta)
    );
    let mut markdown = format!(
        "### Pre-quantization dump ({})\n\n\
         Copied from the f32 accumulator immediately before quantization. \
         GPU values are `accum` stored to a debug buffer before the same `textureStore` into `rgba8unorm`. \
         CPU values are the same accumulator before `round_ties_even`. \
         The v2 formula is unchanged. Distance is `|channel * 255 - nearest n+0.5|` in byte space. \
         Epsilon is {EPS:.0e}. A tighter count uses {TIGHTER:.0e}.\n\n\
         - Differing channels: {differing}\n\
         - Within {EPS:.0e} of an n+0.5 boundary: {on_boundary}\n\
         - Within {TIGHTER:.0e} of an n+0.5 boundary: {on_boundary_tight}\n\
         - `|GPU - CPU| * 255` within {EPS:.0e}: {floats_within}\n\
         - `|GPU - CPU| * 255` beyond {EPS:.0e}: {floats_apart}\n\
         - Bit-identical pre-quant channels: {bit_identical}\n\
         - Max `|GPU - CPU| * 255`: {}\n\
         - Bit-identical and `v * 255` exactly `n + 0.5`: {exact_half}\n\
         - Of those, GPU byte is round-half-up and CPU byte is round-ties-even: {tie_even_vs_half_up}\n\
         - Of those, exact `n + 0.5` but not that pair: {exact_half_not_half_up}\n\
         - Bit-identical and not exactly `n + 0.5`: {not_exact_half}\n\
         - Of the exact halves, GPU byte equals round-ties-to-odd: {gpu_ties_odd}\n\n",
        json_escape(adapter),
        json_f32(max_delta)
    );
    markdown.push_str(
        "Exact means f32 equality of `clamp(v, 0, 1) * 255` with `floor(that) + 0.5`, not an epsilon. \
         round-half-up is `floor(clamp(v, 0, 1) * 255 + 0.5)`. \
         CPU bytes use `round_ties_even`. \
         A bit-identical channel that is not exactly `n + 0.5` means the unorm store differed from the CPU quantizer away from a tie.\n\n",
    );
    if not_exact_half > 0 {
        markdown.push_str(
            "At least one bit-identical channel is not exactly on `n + 0.5`. The unorm store conversion itself differs from CPU away from a tie.\n\n",
        );
    }
    if exact_half_not_half_up > 0 {
        markdown.push_str(
            "Some exact ties are not round-half-up versus round-ties-even: those two rules agree, and the GPU byte is the other integer.\n\n",
        );
    }
    if floats_apart == 0 && differing > 0 && bit_identical == differing {
        markdown.push_str(
            "Every differing channel has bit-identical pre-quantization floats. The ±1 is the `rgba8unorm` store versus CPU `round_ties_even`.\n\n",
        );
    } else if floats_apart == 0 && differing > 0 {
        markdown.push_str(&format!(
            "{bit_identical} of {differing} pre-quant channels are bit-identical. {on_boundary_tight} of {differing} are within {TIGHTER:.0e} of an n+0.5 boundary, and {on_boundary} of {differing} are within {EPS:.0e}. Max `|GPU - CPU| * 255` is {}. Where a channel sits on that boundary, the stored ±1 is the gap crossing it. The f32 ops in the mismatching cases are unpremultiply division and the blend FMAs. This dump does not separate those ops.\n\n",
            json_f32(max_delta)
        ));
    } else if floats_apart > 0 {
        markdown.push_str(
            "Some pre-quantization floats already differ by more than 1e-3 byte. Those pixels ran the f32 ops listed below (unpremultiply division and blend FMAs). This dump does not separate those two ops.\n\n",
        );
    }
    markdown.push_str("| case | channel pixels | on boundary | floats differ | ops |\n| --- | ---: | ---: | ---: | --- |\n");
    for row in rows {
        if row.count == 0 {
            continue;
        }
        let mut row_channels = 0u32;
        let mut row_boundary = 0u32;
        let mut row_apart = 0u32;
        for pixel in &row.mismatches {
            for index in 0..4 {
                if pixel.gpu[index] == pixel.cpu[index] {
                    continue;
                }
                row_channels += 1;
                let nearer =
                    dist_to_half(pixel.gpu_pre[index]).min(dist_to_half(pixel.cpu_pre[index]));
                let delta = ((pixel.gpu_pre[index].clamp(0.0, 1.0)
                    - pixel.cpu_pre[index].clamp(0.0, 1.0))
                    * 255.0)
                    .abs();
                if nearer <= EPS {
                    row_boundary += 1;
                }
                if delta > EPS {
                    row_apart += 1;
                }
            }
        }
        markdown.push_str(&format!(
            "| {} | {row_channels} | {row_boundary} | {row_apart} | {} |\n",
            row.name,
            json_escape(&row.ops)
        ));
    }
    std::fs::write(dir.join("prequant.json"), json).expect("write prequant json");
    std::fs::write(dir.join("prequant.md"), markdown).expect("write prequant markdown");
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

/// Parity-harness shader. The production `composite.wgsl` source is unchanged.
/// `pre_quant` copies `accum` before the same `textureStore` into `rgba8unorm`.
fn parity_shader() -> String {
    let src = shader_with_blend(include_str!("../shaders/composite.wgsl"));
    let decl = "@group(0) @binding(4) var<uniform> config: Config;";
    let with_decl = format!(
        "{decl}\n@group(0) @binding(5) var<storage, read_write> pre_quant: array<vec4<f32>>;"
    );
    let src = src.replacen(decl, &with_decl, 1);
    let store = "textureStore(output,vec2i(id.xy),i32(config.info.y),accum);";
    let patched = "pre_quant[id.y * 256u + id.x] = accum;\n  textureStore(output,vec2i(id.xy),i32(config.info.y),accum);";
    let src = src.replacen(store, patched, 1);
    assert!(
        src.contains("var<storage, read_write> pre_quant") && src.contains(store),
        "parity shader did not keep the production textureStore"
    );
    src
}

/// `effects.wgsl` uniform. Field order matches the WGSL `Config` in uniform address space.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct EffectConfig {
    out_region: [f32; 4],
    region_a: [f32; 4],
    region_b: [f32; 4],
    inv0: [f32; 4],
    inv1: [f32; 4],
    settings: [f32; 4],
    info: [u32; 4],
    extra: [f32; 4],
    curve: [f32; 64],
    sizes: [u32; 4],
    output_clip: [u32; 4],
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    adapter_name: String,
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
                source: wgpu::ShaderSource::Wgsl(parity_shader().into()),
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
                adapter_name: info.name,
            }))
        })
    }

    /// Upload all 256 byte values to an `Rgba8Unorm` texture and read them back
    /// with `textureLoad` + `unorm_byte`. Returns every channel that does not
    /// round-trip. Measured-not-enforced: the caller records, it does not fail.
    fn atlas_read(&self) -> Result<AtlasRead, String> {
        let device = &self.device;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("atlas read"),
            source: wgpu::ShaderSource::Wgsl(atlas_read_shader().into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("atlas read"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas read"),
            size: wgpu::Extent3d {
                width: 256,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let data: Vec<u8> = (0..256).flat_map(atlas_read_texel).collect();
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(1024),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 256,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atlas read out"),
            size: 4096,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atlas read readback"),
            size: 4096,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let view = texture.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas read"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: out.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(4, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&out, 0, &read, 0, 4096);
        self.queue.submit(Some(encoder.finish()));
        map_readback(device, &read)?;
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(format!("validation: {err}"));
        }
        let raw = read
            .slice(..)
            .get_mapped_range()
            .map_err(|e| e.to_string())?
            .to_vec();
        read.unmap();
        let got: &[u32] = bytemuck::cast_slice(&raw);
        let mut mismatches = Vec::new();
        for texel in 0..256 {
            let want = atlas_read_texel(texel);
            for ch in 0..4 {
                let value = got[texel * 4 + ch];
                if value != u32::from(want[ch]) {
                    mismatches.push((texel, ch, want[ch], value));
                }
            }
        }
        Ok(AtlasRead { mismatches })
    }

    fn render(&self, case: &Case) -> Result<(Vec<u8>, Vec<f32>), String> {
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
        let pre_bytes = 256 * 256 * 16;
        let pre_quant = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("parity prequant"),
            size: pre_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
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
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: pre_quant.as_entire_binding(),
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
        let pre_read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("parity prequant readback"),
            size: pre_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_buffer_to_buffer(&pre_quant, 0, &pre_read, 0, pre_bytes);
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
        map_readback(device, &pre_read)?;
        let pre_raw = pre_read
            .slice(..)
            .get_mapped_range()
            .map_err(|e| e.to_string())?
            .to_vec();
        pre_read.unmap();
        let prequant = bytemuck::cast_slice(&pre_raw).to_vec();
        Ok((bytes, prequant))
    }

    /// The production color-adjust chain for each `(contrast, saturation, brightness)`:
    /// mode 0, normal blend onto clear, mode 3, resolve. Sampling does not depend on
    /// the settings, so those passes run once.
    fn render_color_adjust(
        &self,
        pixels: &[u8],
        grid: &[(f32, f32, f32)],
    ) -> Result<Vec<Vec<u8>>, String> {
        if pixels.len() != 256 * 256 * 4 {
            return Err(format!("color-adjust image is {} bytes", pixels.len()));
        }
        let device = &self.device;
        let effects = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("color-adjust effects"),
            layout: None,
            module: &device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("color-adjust effects"),
                source: wgpu::ShaderSource::Wgsl(
                    shader_with_blend(include_str!("../shaders/effects.wgsl")).into(),
                ),
            }),
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let resolve = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("color-adjust resolve"),
            layout: None,
            module: &device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("color-adjust resolve"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("../shaders/effects-resolve.wgsl").into(),
                ),
            }),
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let float_tex = |label: &str| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: 256,
                    height: 256,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            })
        };
        let backdrop = float_tex("color-adjust clear");
        let sampled = float_tex("color-adjust sample");
        let blended = float_tex("color-adjust blend");
        let adjusted = float_tex("color-adjust filter");
        let dummy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("color-adjust dummy"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("color-adjust atlas"),
            size: wgpu::Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &atlas,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
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
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("color-adjust resolve"),
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
        let region = [0.0, 0.0, 1.0, 0.0];
        let config =
            |mode: u32, settings: [f32; 4], entries: u32, size: [u32; 4], clip: u32| EffectConfig {
                out_region: region,
                region_a: region,
                region_b: region,
                inv0: [1.0, 0.0, 0.0, 1.0],
                inv1: [0.0; 4],
                settings,
                info: [mode, 0, entries, 0],
                extra: [0.0; 4],
                curve: [0.0; 64],
                sizes: size,
                output_clip: [256, 256, clip, clip],
            };
        let tile = [GpuTile {
            xy: [0, 0],
            slot: 0,
            pad: 0,
        }];
        let full = [256, 256, 256, 256];
        let configs = [
            config(6, [0.0; 4], 0, [1, 1, 1, 1], 1),
            config(18, [0.0; 4], 1, full, 1),
            config(19, [1.0, 0.0, 0.0, 0.0], 0, full, 1),
            config(17, [0.0; 4], 0, full, 1),
        ];
        let uniforms: Vec<_> = configs
            .iter()
            .map(|item| upload_buffer(device, &self.queue, &[*item], wgpu::BufferUsages::UNIFORM))
            .collect();
        let tiles = upload_buffer(device, &self.queue, &tile, wgpu::BufferUsages::STORAGE);
        let resolve_config = upload_buffer(
            device,
            &self.queue,
            &[GpuConfig {
                origin_scale: [0.0, 0.0, 1.0, 256.0],
                info: [0, 256, 0, 0],
            }],
            wgpu::BufferUsages::UNIFORM,
        );
        let view = |texture: &wgpu::Texture| texture.create_view(&Default::default());
        let backdrop_v = view(&backdrop);
        let sampled_v = view(&sampled);
        let blended_v = view(&blended);
        let adjusted_v = view(&adjusted);
        let dummy_v = view(&dummy);
        let atlas_v = atlas.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let output_v = output.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let layout = effects.get_bind_group_layout(0);
        let bind = |a: &wgpu::TextureView,
                    b: &wgpu::TextureView,
                    clip: &wgpu::TextureView,
                    out: &wgpu::TextureView,
                    uniform: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("color-adjust stage"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(a),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(b),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(clip),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&atlas_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(out),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: tiles.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: uniform.as_entire_binding(),
                    },
                ],
            })
        };
        let stage = [
            bind(&dummy_v, &dummy_v, &dummy_v, &backdrop_v, &uniforms[0]),
            bind(&backdrop_v, &backdrop_v, &dummy_v, &sampled_v, &uniforms[1]),
            bind(&backdrop_v, &sampled_v, &dummy_v, &blended_v, &uniforms[2]),
            bind(&blended_v, &blended_v, &dummy_v, &adjusted_v, &uniforms[3]),
        ];
        let resolve_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("color-adjust resolve"),
            layout: &resolve.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&adjusted_v),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&output_v),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: resolve_config.as_entire_binding(),
                },
            ],
        });
        let mut prep = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("color-adjust sample"),
        });
        for group in &stage[..3] {
            let mut pass = prep.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("color-adjust sample"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&effects);
            pass.set_bind_group(0, group, &[]);
            pass.dispatch_workgroups(16, 16, 1);
        }
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        self.queue.submit(Some(prep.finish()));
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("poll: {e}"))?;
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(format!("validation: {err}"));
        }
        let mut tiles_out = Vec::with_capacity(grid.len());
        for &(contrast, saturation, brightness) in grid {
            let filter = config(
                17,
                [
                    f32::from_bits(crate::renderer::color_adjust_q14(brightness) as u32),
                    f32::from_bits(crate::renderer::color_adjust_q14(contrast) as u32),
                    f32::from_bits(crate::renderer::color_adjust_q14(saturation) as u32),
                    0.0,
                ],
                0,
                full,
                1,
            );
            self.queue
                .write_buffer(&uniforms[3], 0, bytemuck::bytes_of(&filter));
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("color-adjust"),
            });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("color-adjust"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&effects);
                pass.set_bind_group(0, &stage[3], &[]);
                pass.dispatch_workgroups(16, 16, 1);
            }
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("color-adjust resolve"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&resolve);
                pass.set_bind_group(0, &resolve_bind, &[]);
                pass.dispatch_workgroups(16, 16, 1);
            }
            let read = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("color-adjust readback"),
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
            tiles_out.push(bytes);
        }
        Ok(tiles_out)
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
    assert_eq!(f16_bits_to_q14(0x3c00), Q14_ONE as u32);
    assert_eq!(f16_bits_to_q14(0x3800), Q14_HALF as u32);
    assert_eq!(f16_bits_to_q14(0), 0);
    assert_eq!(q14_to_f16_bits(0), 0);
    assert_eq!(q14_to_f16_bits(Q14_ONE as u32), 0x3c00);
    assert_eq!(q14_to_f16_bits(Q14_HALF as u32), 0x3800);
    assert_eq!(q14_to_f16_bits(1), 0x0400);
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
    // Render version 3. Same bytes as renderColorAdjustV3 in tests/e2e/cpu-composite.js.
    let exact = [
        "85427e72aa6975f4",
        "d34d9def32069e30",
        "6fabe3fd4a1d755d",
        "03bdc46a2a94022a",
        "e2631093ebe7c14b",
        "6614a1c612c0eb35",
        "35828221f6e46782",
        "f2d63e24deb38bb5",
        "e21500eb040c8e1d",
        "54f48aae0e383c44",
        "8e735cc19d43f13b",
        "2edfc3149add3c41",
        // Contrast 0 makes every channel the same, so saturation does not move a pixel.
        "204b8da3774c1b25",
        "455fe98699531725",
        "204b8da3774c1b25",
        "455fe98699531725",
        "3696fac022da0187",
        "a719b6b1ac5d2588",
        "e3e29d265599c372",
        "db4228c4143ca9ff",
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

/// Version 2 changes pixels, so moving a document onto it is one undoable edit.
/// Opening and saving a version 1 snapshot leaves the version, and therefore the
/// locked hashes, where they were.
#[test]
fn upgrading_a_version_one_document_switches_the_locked_hashes_and_undo_restores_them() {
    use alto_engine::Engine;

    let v1: std::collections::BTreeMap<_, _> =
        parse_reference(&std::fs::read_to_string(render_v1_path()).unwrap())
            .into_iter()
            .map(|(name, hash, fnv)| (name, (hash, fnv)))
            .collect();
    let v2: std::collections::BTreeMap<_, _> =
        parse_reference(&std::fs::read_to_string(reference_path()).unwrap())
            .into_iter()
            .map(|(name, hash, fnv)| (name, (hash, fnv)))
            .collect();
    let catalog: std::collections::BTreeMap<_, _> = cases()
        .into_iter()
        .map(|case| (case.name.clone(), case))
        .collect();
    let hashes = |case: &Case, version: u32| {
        let tile = render_at(case, version);
        (
            ContentHash::of(&tile).hex(),
            format!("{:016x}", fnv64(&tile)),
        )
    };
    let bilinear = &catalog["scale-bilinear"];
    let rotation = &catalog["rotation"];

    let mut engine = Engine::new();
    let mut meta = alto_doc::DocumentMeta::srgb_u8(512, 512);
    meta.render_version = RENDER_VERSION_F32;
    meta.format_version = 1;
    let handle = engine.create_document(meta).unwrap();
    let saved = engine.snapshot(handle).unwrap();
    let text = String::from_utf8(saved.clone()).unwrap();
    assert!(
        !text.contains("render_version"),
        "saving a version 1 document must not upgrade it"
    );
    let opened = engine.open_snapshot(&saved).unwrap();
    assert_eq!(engine.snapshot(opened).unwrap(), saved);
    assert_eq!(engine.history_state(opened).unwrap(), (0, 0));
    assert_eq!(
        engine.document(opened).unwrap().meta.render_version,
        RENDER_VERSION_F32
    );

    let version = engine.document(handle).unwrap().meta.render_version;
    assert_eq!(hashes(bilinear, version), v1["scale-bilinear"]);
    let rotation_v1 = hashes(rotation, version);
    assert_ne!(rotation_v1, v2["rotation"]);
    assert_eq!(
        engine.render_scene(handle).unwrap().render_version,
        RENDER_VERSION_F32
    );

    engine.upgrade_render_version(handle).unwrap();
    assert_eq!(engine.history_state(handle).unwrap(), (1, 0));
    let version = engine.document(handle).unwrap().meta.render_version;
    assert_eq!(version, RENDER_VERSION);
    assert_eq!(hashes(bilinear, version), v2["scale-bilinear"]);
    assert_eq!(hashes(rotation, version), v2["rotation"]);
    assert_eq!(
        engine.render_scene(handle).unwrap().render_version,
        RENDER_VERSION
    );
    let upgraded = String::from_utf8(engine.snapshot(handle).unwrap()).unwrap();
    assert!(upgraded.contains("\"render_version\": 2"));
    assert!(upgraded.contains("\"format_version\": 3"));

    engine.undo(handle).unwrap();
    let version = engine.document(handle).unwrap().meta.render_version;
    assert_eq!(version, RENDER_VERSION_F32);
    assert_eq!(hashes(bilinear, version), v1["scale-bilinear"]);
    assert_eq!(hashes(rotation, version), rotation_v1);
    assert_eq!(engine.snapshot(handle).unwrap(), saved);
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

fn is_lavapipe_adapter(name: &str) -> bool {
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
    let effects = include_str!("../shaders/effects.wgsl");
    assert!(effects.contains(UNORM_BYTE_WGSL));
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
    let locked = std::fs::read_to_string(reference_path()).unwrap();
    assert!(!locked.contains("-alpha-measure"));
    assert_blend_layers_cover_alpha_pairs(&measured);
}

fn rasters_in_paint_order<'a>(nodes: &'a [Node], out: &mut Vec<&'a Image>) {
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
fn assert_blend_layers_cover_alpha_pairs(measured: &[&Case]) {
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
    let v2_before = include_str!("fixtures/cpu-reference-v2-before-alpha.txt");
    assert!(
        v2.starts_with(v2_before),
        "an existing v2 hash line changed by at least one character"
    );
    for line in v2[v2_before.len()..]
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        assert!(
            line.contains("-alpha-measure "),
            "new hash line is not an alpha-measure case: {line}"
        );
    }
    let v1 = std::fs::read_to_string(render_v1_path()).unwrap();
    let v1_before = include_str!("fixtures/render-v1-before-alpha.txt");
    assert_eq!(
        v1, v1_before,
        "an existing v1 hash changed by at least one character"
    );
}

#[test]
fn gpu_shader_matches_cpu_reference() {
    let gpu = match Gpu::new() {
        Ok(Some(gpu)) => gpu,
        Ok(None) => {
            // macOS CI always has Metal. ALTO_REQUIRE_GPU_PARITY makes a missing
            // adapter a failure on lavapipe CI too, instead of a skip.
            if std::env::var_os("ALTO_REQUIRE_GPU_PARITY").is_some() || cfg!(target_os = "macos") {
                panic!("GPU pixel parity requires a Metal, Vulkan, or DX12 adapter");
            }
            eprintln!("gpu pixel parity skipped: no Metal, Vulkan, or DX12 adapter");
            return;
        }
        Err(err) => panic!("GPU pixel parity could not start: {err}"),
    };
    if std::env::var_os("ALTO_REQUIRE_LAVAPIPE").is_some() {
        assert!(
            is_lavapipe_adapter(&gpu.adapter_name),
            "ALTO_REQUIRE_LAVAPIPE=1 but the adapter is {}",
            gpu.adapter_name
        );
    }
    let mut failed = Vec::new();
    let mut measured = Vec::new();
    for case in cases() {
        let (cpu, cpu_pre) = render_prequant(&case);
        let (gpu_tile, gpu_pre) = match gpu.render(&case) {
            Ok(pair) => pair,
            Err(err) => {
                println!("{}: GPU render failed: {err}", case.name);
                failed.push(format!("{}: {err}", case.name));
                continue;
            }
        };
        if case.measure {
            let row = measure_case(&case, &cpu, &gpu_tile, &cpu_pre, &gpu_pre);
            println!("{}", measure_line(&row));
            measured.push(row);
            continue;
        }
        let report = diff_report(&cpu, &gpu_tile);
        println!("{}: {report}", case.name);
        // Enforced cases have no allowlist. Measured cases are skipped above.
        if cpu != gpu_tile {
            failed.push(format!("{}: {report}", case.name));
        }
    }
    write_alpha_measure(&gpu.adapter_name, &measured);
    // Measured-not-enforced: printed and stored, never pushed to `failed`.
    let atlas = gpu.atlas_read();
    match &atlas {
        Ok(r) => println!("{}", r.line()),
        Err(e) => println!("atlas-read (measured-not-enforced): GPU error: {e}"),
    }
    write_atlas_read(&gpu.adapter_name, &atlas);
    let src = color_adjust_gradient();
    let grid = color_adjust_grid();
    match gpu.render_color_adjust(&src, &grid) {
        Ok(tiles) => {
            if tiles.len() != grid.len() {
                failed.push(format!(
                    "color-adjust: {} GPU tiles for {} settings",
                    tiles.len(),
                    grid.len()
                ));
            }
            for (tile, &(contrast, saturation, brightness)) in tiles.iter().zip(&grid) {
                let cpu = color_adjust_image_v3(&src, brightness, contrast, saturation);
                let name = color_adjust_case_name(contrast, saturation, brightness);
                let report = diff_report(&cpu, tile);
                println!("{name}: {report}");
                if cpu != *tile {
                    failed.push(format!("{name}: {report}"));
                }
            }
        }
        Err(err) => {
            println!("color-adjust: GPU render failed: {err}");
            failed.push(format!("color-adjust: {err}"));
        }
    }
    assert!(
        failed.is_empty(),
        "GPU and CPU pixels differ:\n{}",
        failed.join("\n")
    );
}
