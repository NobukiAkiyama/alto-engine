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
//! Color adjust is the exception: for versions 1 and 2 the reference repeats
//! `effects.wgsl` mode 0, the normal blend onto clear, mode 3, and
//! `effects-resolve.wgsl`, with an f16 round trip after every store. Render version 3
//! runs the integer M5 stages of `effects-q14.wgsl` (Q14 in `rgba16uint`, resolved to
//! `rgba8uint`), and its reference is integer throughout. That includes the v3
//! pixel-mask stages (modes 21 to 23, `mask_programs`), whose Q8 mask lookup is
//! the compositor's sampler.
//! The parity image is a 256² gradient (every R, G, B and alpha, including alpha
//! 0, 1, 128 and 254) run through the contrast × saturation × brightness grid.
//! The other four stay explicit xfails in the browser suite.

#[path = "composite_q14.rs"]
mod q14;

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
        q: Option<IntegerInverse>,
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
                q: None,
                opacity,
                mode: *mode,
                tiles,
            });
        }
    }
}

fn walk_v3(node: &Node, parent: alto_core::FixedAffine, ops: &mut Vec<Op>) {
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
    let local =
        alto_core::FixedAffine::from_affine(Affine2(transform)).expect("v3 transform out of range");
    let world = parent
        .concat(local)
        .expect("v3 composed transform out of range");
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
                walk_v3(child, world, ops);
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
            let Some(q) =
                crate::sample::fixed_inverse(&world).expect("v3 transform has no Q16 inverse")
            else {
                return;
            };
            let mut tiles = split_tiles(image);
            tiles.extend(extra_tiles.iter().cloned());
            tiles.sort_by_key(|t| (t.tx, t.ty));
            ops.push(Op::Draw {
                inv: [0.0; 6],
                q: Some(q),
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

fn fetch_integer_bytes(
    tiles: &[Tile],
    inv: &IntegerInverse,
    origin_px: [i32; 2],
    scale: i32,
    x: i32,
    y: i32,
) -> [u8; 4] {
    let Some(px) = pixel_q8(origin_px[0], x, scale) else {
        return [0; 4];
    };
    let Some(py) = pixel_q8(origin_px[1], y, scale) else {
        return [0; 4];
    };
    let Some(q) = source_q8(inv, px, py) else {
        return [0; 4];
    };
    let (ix, fx) = floor_frac_q8(q[0]);
    let (iy, fy) = floor_frac_q8(q[1]);
    let a = sample_u8(tiles, ix, iy);
    let b = sample_u8(tiles, ix + 1, iy);
    let d = sample_u8(tiles, ix, iy + 1);
    let e = sample_u8(tiles, ix + 1, iy + 1);
    let mut out = [0; 4];
    for channel in 0..4 {
        let byte = bilinear_byte(a[channel], b[channel], d[channel], e[channel], fx, fy);
        out[channel] = byte;
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
    match crate::sample::integer_inverse_v2(inv) {
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
/// because the stage passes the input alpha through unchanged.
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

/// Render version 3, the integer M5 chain: mode 18 premultiplies the recovered
/// bytes into Q14, mode 19 copies them onto the empty backdrop, mode 17 adjusts
/// with alpha unchanged, and the resolve writes `div_q(q * 255, 16384)` bytes.
/// Every stage stores exact Q14 in `rgba16uint`, so nothing rounds in between.
fn color_adjust_texel_v3(
    rgba: [u8; 4],
    brightness: f32,
    contrast: f32,
    saturation: f32,
) -> [u8; 4] {
    let bytes = rgba.map(u32::from);
    let alpha = bytes[3];
    let q = [
        premul_byte_q14(bytes[0], alpha),
        premul_byte_q14(bytes[1], alpha),
        premul_byte_q14(bytes[2], alpha),
        byte_q14(alpha),
    ];
    let rgb = color_adjust_exact_q14(
        q,
        crate::renderer::color_adjust_q14(brightness),
        crate::renderer::color_adjust_q14(contrast),
        crate::renderer::color_adjust_q14(saturation),
    );
    q14::bytes([rgb[0], rgb[1], rgb[2], q[3]].map(i64::from))
}

/// The Q28 divisor of the mask factor `F = m * o` (`div28` in `effects-q14.wgsl`).
const Q28: i64 = 1 << 28;

/// Mode 21, the layer mask and opacity: `div28(v * F)` on all four channels.
/// `None` when a product or sum leaves i64.
fn mask_layer_q14(v: [i64; 4], f: i64) -> Option<[i64; 4]> {
    let mut out = [0; 4];
    for c in 0..4 {
        out[c] = div_q_i64(v[c].checked_mul(f)?, Q28)?;
    }
    Some(out)
}

/// Mode 22, the effect selection: `div_q(in * (16384 - s) + f * s, 16384)`.
/// The v2 mode 12 mix with coverage `k` is the same formula.
fn mask_select_q14(input: [i64; 4], filtered: [i64; 4], s: i64) -> Option<[i64; 4]> {
    let mut out = [0; 4];
    for c in 0..4 {
        let n = input[c]
            .checked_mul(q14::Q - s)?
            .checked_add(filtered[c].checked_mul(s)?)?;
        out[c] = div_q_i64(n, q14::Q)?;
    }
    Some(out)
}

/// Mode 23 `g`: the adjusted copy, or for a non-normal blend, the straight
/// product (screen for 2, multiply for every other mode, as v2) premultiplied by
/// the backdrop alpha.
fn mask_adjust_blend_q14(orig: [i64; 4], filt: [i64; 4], blend: u32) -> Option<[i64; 4]> {
    if blend == 0 {
        return Some(filt);
    }
    let d = q14::straight(orig);
    let t = q14::straight(filt);
    let mut g = [0, 0, 0, orig[3]];
    for c in 0..3 {
        let color = if blend == 2 {
            q14::Q - div_q_i64((q14::Q - d[c]).checked_mul(q14::Q - t[c])?, q14::Q)?
        } else {
            div_q_i64(d[c].checked_mul(t[c])?, q14::Q)?
        };
        g[c] = div_q_i64(color.checked_mul(orig[3])?, q14::Q)?;
    }
    Some(g)
}

/// Mode 23, the adjustment-layer mask, opacity and blend:
/// `div28(orig * (2^28 - F) + g * F)`.
fn mask_adjust_q14(orig: [i64; 4], filt: [i64; 4], f: i64, blend: u32) -> Option<[i64; 4]> {
    let g = mask_adjust_blend_q14(orig, filt, blend)?;
    let mut out = [0; 4];
    for c in 0..4 {
        let n = orig[c]
            .checked_mul(Q28 - f)?
            .checked_add(g[c].checked_mul(f)?)?;
        out[c] = div_q_i64(n, Q28)?;
    }
    Some(out)
}

/// The byte a v3 mask stage reads at pixel (x, y) of a target with the integer
/// ROI `region` (`[x, y, level scale]`), from one mask tile at key (0, 0) whose
/// R channel is the byte. See [`tiles_texel_cpu`].
fn mask_byte_cpu(
    image: &[u8],
    transform: [f64; 6],
    region: [i32; 3],
    x: i32,
    y: i32,
) -> Option<u8> {
    mask_tiles_byte_cpu(&[([0, 0], image)], transform, region, x, y)
}

/// The mask byte (R) of [`tiles_texel_cpu`].
fn mask_tiles_byte_cpu(
    tiles: &[([i32; 2], &[u8])],
    transform: [f64; 6],
    region: [i32; 3],
    x: i32,
    y: i32,
) -> Option<u8> {
    Some(tiles_texel_cpu(tiles, transform, region, x, y)?[0])
}

/// The RGBA bytes a v3 stage reads from the atlas (mode 18 the raster, modes 21
/// to 23 the mask) at pixel (x, y) of a target with the integer ROI `region`
/// (`[x, y, level scale]`), from the RGBA8 tiles `tiles` by tile key, with
/// document-to-pixel `transform`. Nearest: the texel is the floor of the Q8
/// position. A texel in no listed tile, or a position without an i32 value, is
/// transparent. As the host (`renderer::mask_sampler`), every lookup is Q16; it
/// is evaluated here from `integer_inverse` and `source_q8`, not from the packed
/// bits. `None` when the host refuses the lookup.
fn tiles_texel_cpu(
    tiles: &[([i32; 2], &[u8])],
    transform: [f64; 6],
    region: [i32; 3],
    x: i32,
    y: i32,
) -> Option<[u8; 4]> {
    let fixed = alto_core::FixedAffine::from_affine(Affine2(transform))?;
    tiles_texel_fixed_cpu(tiles, fixed, region, x, y)
}

fn tiles_texel_fixed_cpu(
    tiles: &[([i32; 2], &[u8])],
    fixed: alto_core::FixedAffine,
    region: [i32; 3],
    x: i32,
    y: i32,
) -> Option<[u8; 4]> {
    let [ox, oy, scale] = region;
    crate::renderer::fixed_mask_sampler(&fixed, [ox, oy, scale])?;
    let inv = crate::sample::fixed_inverse(&fixed)??;
    let (Some(px), Some(py)) = (pixel_q8(ox, x, scale), pixel_q8(oy, y, scale)) else {
        return Some([0; 4]);
    };
    let Some(q) = source_q8(&inv, px, py) else {
        return Some([0; 4]);
    };
    let (tx, ty) = (floor_frac_q8(q[0]).0, floor_frac_q8(q[1]).0);
    let key = [tx >> 8, ty >> 8];
    Some(
        tiles
            .iter()
            .find(|(k, _)| *k == key)
            .map_or([0; 4], |(_, image)| {
                let i = (((ty - key[1] * 256) * 256 + tx - key[0] * 256) * 4) as usize;
                [image[i], image[i + 1], image[i + 2], image[i + 3]]
            }),
    )
}

/// The document transform that puts a 256² tile's pixel `i` under pixel `i` of
/// a target with ROI `region`: mode 18 of a raster drawn as it is.
fn placement(region: [i32; 3]) -> [f64; 6] {
    let [x, y, scale] = region.map(f64::from);
    [scale, 0.0, 0.0, scale, x, y]
}

/// One `effects-q14.wgsl` dispatch over 256² targets with the identity ROI.
/// `a`/`b` are input targets, `slot` the atlas layer mode 18 reads, and
/// `settings` the Q14 integers the host bit-casts into the uniform.
#[derive(Clone, Copy)]
struct Q14Stage {
    mode: u32,
    settings: [i32; 4],
    a: usize,
    b: usize,
    out: usize,
    slot: u32,
    /// Modes 21 to 23: the mask's document transform (`RMask` after linking).
    transform: [f64; 6],
    /// Mode 18: the raster's document transform. `None` places the atlas layer
    /// as it is at the output ROI (`placement`).
    raster: Option<[f64; 6]>,
}

/// An M5 graph as `renderer/m5.rs` dispatches it for render version 3. Target 0
/// is never written, so it is the empty backdrop.
struct Q14Program {
    name: String,
    targets: usize,
    /// The integer ROI `[x, y, level scale]` of each 256² target; a target past
    /// the end is `[0, 0, 1]`.
    regions: Vec<[i32; 3]>,
    stages: Vec<Q14Stage>,
    result: usize,
}

impl Q14Program {
    fn region(&self, target: usize) -> [i32; 3] {
        self.regions.get(target).copied().unwrap_or([0, 0, 1])
    }
}

/// The texel of a 256² target with ROI `input` under pixel (x, y) of a target
/// with ROI `out`: `floor((p - origin) / scale)` at the pixel center
/// `p = out_origin + (i + 1/2) * out_scale`, exact in i64. `None` outside.
fn input_texel_cpu(out: [i32; 3], input: [i32; 3], x: i32, y: i32) -> Option<usize> {
    let axis = |o: i32, i: i32, v: i32| {
        let n = 2 * (i64::from(o) - i64::from(i)) + (2 * i64::from(v) + 1) * i64::from(out[2]);
        n.div_euclid(2 * i64::from(input[2]))
    };
    let (tx, ty) = (axis(out[0], input[0], x), axis(out[1], input[1], y));
    ((0..256).contains(&tx) && (0..256).contains(&ty)).then(|| (ty * 256 + tx) as usize)
}

impl Q14Stage {
    /// The atlas lookup transform at output ROI `region`: the raster of mode 18,
    /// the mask of modes 21 to 23, none for the rest.
    fn lookup_transform(&self, region: [i32; 3]) -> Option<[f64; 6]> {
        match self.mode {
            18 => Some(self.raster.unwrap_or_else(|| placement(region))),
            21..=23 => Some(self.transform),
            _ => None,
        }
    }
    /// Whether the stage reads input `a` and input `b`, as `renderer/m5.rs`
    /// passes them to `q14_params`.
    fn reads(&self) -> [bool; 2] {
        match self.mode {
            8 | 17 => [true, false],
            19..=23 => [true, true],
            _ => [false, false],
        }
    }
    /// Mode 18 of a raster with document transform `world`.
    fn raster(slot: u32, world: [f64; 6], out: usize) -> Self {
        Self {
            raster: Some(world),
            ..Self::ingress(slot, out)
        }
    }
    fn ingress(slot: u32, out: usize) -> Self {
        Self {
            mode: 18,
            settings: [0; 4],
            a: 0,
            b: 0,
            out,
            slot,
            transform: ident(),
            raster: None,
        }
    }
    fn layer(dst: usize, src: usize, opacity: f32, mode: u32, out: usize) -> Self {
        Self {
            mode: 19,
            settings: [crate::composite_opacity_q14(opacity), mode as i32, 0, 0],
            a: dst,
            b: src,
            out,
            slot: 0,
            transform: ident(),
            raster: None,
        }
    }
    fn color_adjust(
        input: usize,
        (contrast, saturation, brightness): (f32, f32, f32),
        out: usize,
    ) -> Self {
        Self {
            mode: 17,
            settings: [
                crate::renderer::color_adjust_q14(brightness),
                crate::renderer::color_adjust_q14(contrast),
                crate::renderer::color_adjust_q14(saturation),
                0,
            ],
            a: input,
            b: input,
            out,
            slot: 0,
            transform: ident(),
            raster: None,
        }
    }
    fn adjustment_mix(backdrop: usize, adjusted: usize, opacity: f32, out: usize) -> Self {
        Self {
            mode: 20,
            settings: [crate::composite_opacity_q14(opacity), 0, 0, 0],
            a: backdrop,
            b: adjusted,
            out,
            slot: 0,
            transform: ident(),
            raster: None,
        }
    }
    /// Mode 21: the layer mask in atlas layer `slot` and the layer opacity.
    fn mask(input: usize, slot: u32, opacity: f32, transform: [f64; 6], out: usize) -> Self {
        Self {
            mode: 21,
            settings: [crate::composite_opacity_q14(opacity), 1, 0, 0],
            a: input,
            b: input,
            out,
            slot,
            transform,
            raster: None,
        }
    }
    /// Mode 22: `before` where the selection in `slot` is 0, `after` where it is 255.
    fn select(before: usize, after: usize, slot: u32, out: usize) -> Self {
        Self {
            mode: 22,
            settings: [0; 4],
            a: before,
            b: after,
            out,
            slot,
            transform: ident(),
            raster: None,
        }
    }
    /// Mode 23: an adjustment layer's mask (or none), opacity and blend.
    fn adjust(
        backdrop: usize,
        adjusted: usize,
        mask: Option<u32>,
        opacity: f32,
        blend: u32,
        out: usize,
    ) -> Self {
        Self {
            mode: 23,
            settings: [
                crate::composite_opacity_q14(opacity),
                i32::from(mask.is_some()),
                blend as i32,
                0,
            ],
            a: backdrop,
            b: adjusted,
            out,
            slot: mask.unwrap_or(0),
            transform: ident(),
            raster: None,
        }
    }
}

/// CPU reference for a [`Q14Program`], one stage at a time, with the checked
/// Q14 functions of the integer tile compositor. A stage reads each input at
/// the texel under its output pixel (`input_texel_cpu`). Mode 18 reads the one
/// tile at key (0, 0) in its `slot` through the Q16 lookup of its raster
/// transform (by default the atlas layer as it is, `placement`), and a mask
/// stage its mask the same way.
fn q14_program_cpu(images: &[Vec<u8>], program: &Q14Program) -> Vec<u8> {
    let mut t = vec![vec![[0i64; 4]; 256 * 256]; program.targets];
    for stage in &program.stages {
        let region = program.region(stage.out);
        let tiles = [([0, 0], &images[stage.slot as usize][..])];
        let lookup = stage.lookup_transform(region);
        let read = |target: usize, x, y| {
            input_texel_cpu(region, program.region(target), x, y).map_or([0; 4], |j| t[target][j])
        };
        let next: Vec<[i64; 4]> = (0..256 * 256)
            .map(|i: i32| {
                let (x, y) = (i % 256, i / 256);
                let a = read(stage.a, x, y);
                let b = read(stage.b, x, y);
                let w = i64::from(stage.settings[0]);
                match stage.mode {
                    8 => a,
                    18 => {
                        let rgba = tiles_texel_cpu(&tiles, lookup.unwrap(), region, x, y)
                            .expect("v3 refuses a raster without a Q16 lookup");
                        let alpha = u32::from(rgba[3]);
                        let mut q = [0i64; 4];
                        for c in 0..3 {
                            q[c] = i64::from(premul_byte_q14(u32::from(rgba[c]), alpha));
                        }
                        q[3] = i64::from(byte_q14(alpha));
                        q
                    }
                    19 => {
                        let mode = stage.settings[1] as u32;
                        if w == q14::Q {
                            q14::blend(a, q14::straight(b), [b[0], b[1], b[2]], b[3], mode)
                        } else {
                            q14::pop(a, b, w, mode)
                        }
                    }
                    17 => {
                        let q = a.map(|v| v as u32);
                        let rgb = color_adjust_exact_q14(
                            q,
                            stage.settings[0],
                            stage.settings[1],
                            stage.settings[2],
                        );
                        [rgb[0], rgb[1], rgb[2], q[3]].map(i64::from)
                    }
                    20 => std::array::from_fn(|c| {
                        div_q_i64(a[c] * (q14::Q - w) + b[c] * w, q14::Q).unwrap()
                    }),
                    21..=23 => {
                        let m = || {
                            i64::from(byte_q14(u32::from(
                                mask_tiles_byte_cpu(&tiles, lookup.unwrap(), region, x, y)
                                    .expect("v3 refuses a mask without a Q16 lookup"),
                            )))
                        };
                        let f = |has: i32| if has == 0 { q14::Q } else { m() } * w;
                        match stage.mode {
                            21 => mask_layer_q14(a, f(stage.settings[1])),
                            22 => mask_select_q14(a, b, m()),
                            _ => mask_adjust_q14(
                                a,
                                b,
                                f(stage.settings[1]),
                                stage.settings[2] as u32,
                            ),
                        }
                        .expect("mask Q14 terms fit in i64")
                    }
                    mode => panic!("no CPU reference for Q14 mode {mode}"),
                }
                .map(|v| v.clamp(0, q14::Q))
            })
            .collect();
        t[stage.out] = next;
    }
    t[program.result]
        .iter()
        .flat_map(|q| q14::bytes(*q))
        .collect()
}

/// The browser `color-adjust` test as an M5 graph: an adjustment layer over one
/// raster. Raster ingress, the copy onto the empty backdrop, then the Q14 adjust.
fn color_adjust_program(settings: (f32, f32, f32)) -> Q14Program {
    Q14Program {
        name: color_adjust_case_name(settings.0, settings.1, settings.2),
        targets: 4,
        regions: vec![],
        stages: vec![
            Q14Stage::ingress(0, 1),
            Q14Stage::layer(0, 1, 1.0, 0, 2),
            Q14Stage::color_adjust(2, settings, 3),
        ],
        result: 3,
    }
}

/// Atlas layers for [`opacity_programs`]: the color-adjust gradient, opaque
/// blue, opaque red and half-alpha red.
fn opacity_images() -> Vec<Vec<u8>> {
    vec![
        color_adjust_gradient(),
        [0, 0, 255, 255].repeat(256 * 256),
        [255, 0, 0, 255].repeat(256 * 256),
        [255, 0, 0, 128].repeat(256 * 256),
    ]
}

/// Opacity 0.5 through the integer M5 stages, enforced on every backend: a layer
/// at half opacity (normal and four other modes), a color-adjusted layer at half
/// opacity, and a color-adjust adjustment layer at half opacity.
fn opacity_programs() -> Vec<Q14Program> {
    let vivid = (1.7, 1.6, 0.0);
    let mut out = vec![Q14Program {
        name: "m5-opacity-half".into(),
        targets: 5,
        regions: vec![],
        stages: vec![
            Q14Stage::ingress(1, 1),
            Q14Stage::layer(0, 1, 1.0, 0, 2),
            Q14Stage::ingress(2, 3),
            Q14Stage::layer(2, 3, 0.5, 0, 4),
        ],
        result: 4,
    }];
    for (mode, name) in [
        (1, "multiply"),
        (2, "screen"),
        (9, "soft-light"),
        (14, "color"),
    ] {
        out.push(Q14Program {
            name: format!("m5-opacity-half-{name}"),
            targets: 5,
            regions: vec![],
            stages: vec![
                Q14Stage::ingress(0, 1),
                Q14Stage::layer(0, 1, 1.0, 0, 2),
                Q14Stage::ingress(3, 3),
                Q14Stage::layer(2, 3, 0.5, mode, 4),
            ],
            result: 4,
        });
    }
    out.push(Q14Program {
        name: "m5-color-adjust-layer-opacity-half".into(),
        targets: 6,
        regions: vec![],
        stages: vec![
            Q14Stage::ingress(1, 1),
            Q14Stage::layer(0, 1, 1.0, 0, 2),
            Q14Stage::ingress(0, 3),
            Q14Stage::color_adjust(3, vivid, 4),
            Q14Stage::layer(2, 4, 0.5, 0, 5),
        ],
        result: 5,
    });
    out.push(Q14Program {
        name: "m5-adjustment-opacity-half".into(),
        targets: 5,
        regions: vec![],
        stages: vec![
            Q14Stage::ingress(0, 1),
            Q14Stage::layer(0, 1, 1.0, 0, 2),
            Q14Stage::color_adjust(2, vivid, 3),
            Q14Stage::adjustment_mix(2, 3, 0.5, 4),
        ],
        result: 4,
    });
    out
}

/// An R8 mask as the atlas holds it (`rgba_pixels`: `[v, v, v, 255]`).
fn mask_image(byte: impl Fn(u32, u32) -> u8) -> Vec<u8> {
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
fn feathered_rectangle(rect: [f64; 4], feather: f64) -> Vec<u8> {
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
const FEATHER_RECT: [f64; 4] = [40.25, 40.75, 216.5, 216.0];
const FEATHER: f64 = 64.0;

/// The mask rotated by 0.3 rad about the tile center, as an unlinked mask transform.
fn rotated_mask_transform() -> [f64; 6] {
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
fn tie_mask_transform() -> [f64; 6] {
    [1.0, 0.0, 0.0, 1.0, 2.501953125, -0.498046875]
}

/// Atlas layers for [`mask_programs`].
fn mask_images() -> Vec<Vec<u8>> {
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

/// The memo's enforced mask cases as `renderer/m5.rs` dispatches them for
/// render version 3. A masked raster: mode 18, mode 21 (mask and opacity), then
/// mode 19 at opacity 1 onto the backdrop.
fn mask_programs() -> Vec<Q14Program> {
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
fn mask_origin_cases() -> Vec<([i32; 3], [f64; 6])> {
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
fn mask_offset_regions() -> Vec<([i32; 3], [i32; 3])> {
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
fn mask_origin_programs() -> Vec<Q14Program> {
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
fn purge_regions() -> Vec<[i32; 3]> {
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
fn purge_programs() -> Vec<Q14Program> {
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

/// A masked raster drawn through the renderer for render version 3
/// (`Renderer::read_tile`), so its mask tiles are the ones
/// `Context::mask_entries` selects and uploads. The raster is the opaque
/// gradient (`mask_images` 4) in the display tile `key` at level 0, under a
/// root group; the mask spans several tiles.
struct RendererMaskCase {
    name: &'static str,
    key: [i32; 2],
    linked: bool,
    transform: [f64; 6],
    opacity: f32,
    /// R8 mask tiles by tile key.
    mask: Vec<([i32; 2], TileSpec)>,
}

impl RendererMaskCase {
    fn region(&self) -> [i32; 3] {
        [self.key[0] * 256, self.key[1] * 256, 1]
    }
    /// The mask's document transform as `apply_props` links it (world = identity).
    fn document_transform(&self) -> [f64; 6] {
        if self.linked {
            crate::renderer::multiply(ident(), self.transform)
        } else {
            self.transform
        }
    }
}

/// R8 mask bytes below 255 (so a 255 tile is told apart), different for each `seed`.
fn mask_tile_bytes(seed: i32) -> Vec<u8> {
    (0..256 * 256)
        .map(|i: i32| ((i % 256) * 7 + (i / 256) * 13 + seed * 31).rem_euclid(200) as u8)
        .collect()
}

/// A tile of the renderer cases, rebuilt from a seed in Rust and in
/// `tests/e2e/m5-v3-renderer-reference.mjs`.
#[derive(Clone, Copy, Debug)]
enum TileSpec {
    /// RGBA, [`raster_tile_bytes`].
    Raster(i32),
    /// RGBA, [`raster_tile_bytes`] with alpha 255.
    Opaque(i32),
    /// R8, [`mask_tile_bytes`].
    Mask(i32),
    /// R8, all 255.
    White,
}

impl TileSpec {
    fn bytes(self) -> Vec<u8> {
        match self {
            Self::Raster(seed) => raster_tile_bytes(seed),
            Self::Opaque(seed) => {
                let mut bytes = raster_tile_bytes(seed);
                for px in bytes.as_chunks_mut::<4>().0 {
                    px[3] = 255;
                }
                bytes
            }
            Self::Mask(seed) => mask_tile_bytes(seed),
            Self::White => vec![255; 256 * 256],
        }
    }
    fn json(self) -> String {
        match self {
            Self::Raster(seed) => format!("[\"raster\",{seed}]"),
            Self::Opaque(seed) => format!("[\"opaque\",{seed}]"),
            Self::Mask(seed) => format!("[\"mask\",{seed}]"),
            Self::White => "[\"white\",0]".into(),
        }
    }
    /// R8 mask bytes as the atlas holds them (`rgba_pixels`: `[v, v, v, 255]`).
    fn mask_rgba(self) -> Vec<u8> {
        self.bytes()
            .iter()
            .flat_map(|v| [*v, *v, *v, 255])
            .collect()
    }
}

fn renderer_mask_cases() -> Vec<RendererMaskCase> {
    // At origin 999936 the Q16 inverse (x coefficient 65537) reads texel
    // 1000195 at pixel (255, 0), in tile (3907, 0). The f64 inverse puts the
    // ROI's right edge at 1000191.0000000001, before that tile.
    let far = RendererMaskCase {
        name: "renderer-mask-origin-999936",
        key: [3906, 0],
        linked: false,
        transform: [0.9999900000999989, 0.0, 0.0, 1.0, 11.00180998190018, 0.0],
        opacity: 1.0,
        mask: vec![
            ([3905, 0], TileSpec::Mask(1)),
            ([3906, 0], TileSpec::Mask(2)),
            ([3907, 0], TileSpec::White),
        ],
    };
    // A linked mask shrunk by 0.8 and rotated by 0.3 rad about the center of
    // display tile (5, 3): the tile reads mask tiles (4..=6, 2..=4).
    let (s, c) = 0.3f64.sin_cos();
    let (k, cx, cy) = (0.8, 5.0 * 256.0 + 128.0, 3.0 * 256.0 + 128.0);
    let rotated = RendererMaskCase {
        name: "renderer-mask-rotated-3x3",
        key: [5, 3],
        linked: true,
        transform: [
            k * c,
            k * s,
            -k * s,
            k * c,
            cx - k * (c * cx - s * cy),
            cy - k * (s * cx + c * cy),
        ],
        opacity: 0.5,
        mask: (2..=4)
            .flat_map(|ty| (4..=6).map(move |tx| ([tx, ty], TileSpec::Mask(tx * 3 + ty))))
            .collect(),
    };
    vec![far, rotated]
}

/// A pixel mask (or an effect selection) of a [`RendererLayer`]: R8 tiles by
/// key, with its transform, linked to the node or not.
#[derive(Clone)]
struct RendererCaseMask {
    linked: bool,
    transform: [f64; 6],
    tiles: Vec<([i32; 2], TileSpec)>,
}

impl RendererCaseMask {
    /// The mask's document transform under node world `world`, as
    /// `Context::mask_entries` links it.
    fn document_fixed(&self, world: [f64; 6]) -> Option<alto_core::FixedAffine> {
        let local = alto_core::FixedAffine::from_affine(Affine2(self.transform))?;
        if self.linked {
            alto_core::FixedAffine::from_affine(Affine2(world))?.concat(local)
        } else {
            Some(local)
        }
    }
    /// The tiles as the atlas holds them.
    fn rgba(&self) -> Vec<([i32; 2], Vec<u8>)> {
        self.tiles
            .iter()
            .map(|(k, s)| (*k, s.mask_rgba()))
            .collect()
    }
}

/// One child of the root group of a [`RendererCase`], bottom first.
#[derive(Clone)]
enum RendererLayer {
    /// A raster node: RGBA tiles by key under node transform `transform`, with
    /// an optional color-adjust layer effect `(contrast, saturation,
    /// brightness)` and its selection, a pixel mask, opacity and blend.
    Raster {
        tiles: Vec<([i32; 2], TileSpec)>,
        transform: [f64; 6],
        adjust: Option<(f32, f32, f32)>,
        selection: Option<RendererCaseMask>,
        mask: Option<RendererCaseMask>,
        opacity: f32,
        blend: RenderBlend,
    },
    /// A color-adjust adjustment layer at the identity transform.
    Adjust {
        settings: (f32, f32, f32),
        mask: Option<RendererCaseMask>,
        opacity: f32,
        blend: RenderBlend,
    },
}

/// A render version 3 scene composed by the renderer (`Renderer::read_tile`),
/// so every stage uniform, tile selection and the resolve are the host's own
/// (`Context::q14_params`, `lookup_entries`, `resolve_span`). The display tile
/// is `key` at `level` (level 1 is the integer pyramid of four level-0 tiles),
/// and the canvas is `[0, canvas)`. `refused` is the `GpuUnsupported` detail
/// the render must fail with, if any.
struct RendererCase {
    name: String,
    level: i32,
    key: [i32; 2],
    canvas: [i32; 2],
    layers: Vec<RendererLayer>,
    refused: Option<&'static str>,
}

impl From<&RendererMaskCase> for RendererCase {
    /// The masked opaque gradient of a [`RendererMaskCase`] at its display tile.
    fn from(case: &RendererMaskCase) -> Self {
        Self {
            name: case.name.into(),
            level: 0,
            key: case.key,
            canvas: [(case.key[0] + 1) * 256, (case.key[1] + 1) * 256],
            layers: vec![RendererLayer::Raster {
                tiles: vec![(case.key, TileSpec::Opaque(0))],
                transform: ident(),
                adjust: None,
                selection: None,
                mask: Some(RendererCaseMask {
                    linked: case.linked,
                    transform: case.transform,
                    tiles: case.mask.clone(),
                }),
                opacity: case.opacity,
                blend: RenderBlend::Normal,
            }],
            refused: None,
        }
    }
}

/// RGBA raster tile bytes, different for each `seed`: the color-adjust
/// gradient (every channel value, alpha 0, 1, 128, 254) moved by `seed` rows
/// and columns, so a texel off by one is seen.
fn raster_tile_bytes(seed: i32) -> Vec<u8> {
    let gradient = color_adjust_gradient();
    let mut out = vec![0; 256 * 256 * 4];
    for y in 0..256 {
        for x in 0..256 {
            let from = (((y + seed * 37).rem_euclid(256) * 256 + (x + seed * 11).rem_euclid(256))
                * 4) as usize;
            let to = ((y * 256 + x) * 4) as usize;
            out[to..to + 4].copy_from_slice(&gradient[from..from + 4]);
        }
    }
    out
}

/// The real-renderer cases of the float purge: every v3 M5 mode the host
/// dispatches (18, 17, 19 below opacity 1, 20, 21, 22, 23) at the origin, at
/// negative tile keys, near 10^6 px and next to 2^23 px (the last tile whose
/// Q8 pixel centers fit i32), raster transforms whose source spans several
/// tiles, a canvas edge through the tile, level 1 (the pyramid), and the
/// refusals past the integer range.
fn renderer_cases() -> Vec<RendererCase> {
    let vivid = (1.7, 1.6, 0.0);
    let (s, c) = 0.3f64.sin_cos();
    // A rotation by 0.3 rad and a scale by `k` about the center of display tile `key`.
    let rotation = |key: [i32; 2], k: f64| {
        let (cx, cy) = (
            f64::from(key[0]) * 256.0 + 128.0,
            f64::from(key[1]) * 256.0 + 128.0,
        );
        [
            k * c,
            k * s,
            -k * s,
            k * c,
            cx - k * (c * cx - s * cy),
            cy - k * (s * cx + c * cy),
        ]
    };
    // Raster or mask tiles over keys `x` by `y`, seeded by key.
    let rasters = |x: std::ops::RangeInclusive<i32>, y: std::ops::RangeInclusive<i32>| {
        y.flat_map(|ty| {
            x.clone()
                .map(move |tx| ([tx, ty], TileSpec::Raster(tx * 5 + ty)))
        })
        .collect::<Vec<_>>()
    };
    let masks = |x: std::ops::RangeInclusive<i32>, y: std::ops::RangeInclusive<i32>| {
        y.flat_map(|ty| {
            x.clone()
                .map(move |tx| ([tx, ty], TileSpec::Mask(tx * 3 + ty)))
        })
        .collect::<Vec<_>>()
    };
    let raster = |tiles, transform, adjust, opacity, blend| RendererLayer::Raster {
        tiles,
        transform,
        adjust,
        selection: None,
        mask: None,
        opacity,
        blend,
    };
    let case = |name: String, key: [i32; 2], layers: Vec<RendererLayer>| RendererCase {
        name,
        level: 0,
        key,
        canvas: [(key[0] + 1) * 256, (key[1] + 1) * 256],
        layers,
        refused: None,
    };
    let mut out: Vec<RendererCase> = renderer_mask_cases().iter().map(Into::into).collect();
    // Display tiles on the canvas (it starts at 0, so a negative display key is
    // all transparent), with their raster and mask tiles at `src`, moved under
    // the display tile by a translation that is not a whole tile: at the
    // origin, at negative keys, near -10^6 px, and the last tile below 2^23 px
    // on both axes.
    for (key, src, off) in [
        ([0, 0], [0, 0], [0.0, 0.0]),
        ([1, 1], [-3, -2], [37.0, -21.0]),
        ([3906, 5], [-3907, 5], [-11.0, 5.0]),
        ([32767, 32767], [32767, 32767], [0.0, 0.0]),
    ] {
        let tag = format!("{key:?} from {src:?}");
        let shift = [
            f64::from(key[0] - src[0]) * 256.0 + off[0],
            f64::from(key[1] - src[1]) * 256.0 + off[1],
        ];
        let place =
            |t: [f64; 6]| crate::renderer::multiply([1.0, 0.0, 0.0, 1.0, shift[0], shift[1]], t);
        let at = |k: [i32; 2]| vec![(k, TileSpec::Raster(k[0] - k[1]))];
        let opaque = |k: [i32; 2]| {
            raster(
                vec![(k, TileSpec::Opaque(k[0] + k[1]))],
                place(ident()),
                None,
                1.0,
                RenderBlend::Normal,
            )
        };
        // Modes 18, 17 and 19 below opacity 1 (multiply onto the opaque layer).
        out.push(case(
            format!("renderer {tag} color-adjust layer multiply o=0.35"),
            key,
            vec![
                opaque(src),
                raster(
                    at(src),
                    place(ident()),
                    Some(vivid),
                    0.35,
                    RenderBlend::Multiply,
                ),
            ],
        ));
        // Mode 20: an adjustment layer at opacity 0.5 over a translucent layer.
        out.push(case(
            format!("renderer {tag} adjustment o=0.5"),
            key,
            vec![
                raster(at(src), place(ident()), None, 1.0, RenderBlend::Normal),
                RendererLayer::Adjust {
                    settings: vivid,
                    mask: None,
                    opacity: 0.5,
                    blend: RenderBlend::Normal,
                },
            ],
        ));
        // Modes 22, 23 and 21 with rotated masks over several tiles. Next to
        // 2^23 the rotations pivot two tiles in and halve the source, so every
        // source position stays below 2^23 px (otherwise refused, below).
        let k = |dx: i32, dy: i32| [src[0] + dx, src[1] + dy];
        let (pivot, k_mask, k_raster) = if key[0] > 30_000 {
            (k(-2, -2), 2.0, 2.0)
        } else {
            (src, 0.8, 1.25)
        };
        let rotated = RendererCaseMask {
            linked: false,
            transform: place(rotation(pivot, k_mask)),
            tiles: masks(src[0] - 1..=src[0] + 1, src[1] - 1..=src[1] + 1),
        };
        out.push(case(
            format!("renderer {tag} selection, masked screen adjustment o=0.5"),
            key,
            vec![
                RendererLayer::Raster {
                    tiles: at(src),
                    transform: place(ident()),
                    adjust: Some(vivid),
                    selection: Some(rotated.clone()),
                    mask: None,
                    opacity: 1.0,
                    blend: RenderBlend::Normal,
                },
                RendererLayer::Adjust {
                    settings: (0.6, 0.2, 0.1),
                    mask: Some(rotated.clone()),
                    opacity: 0.5,
                    blend: RenderBlend::Screen,
                },
            ],
        ));
        // Mode 18 through a rotated, scaled raster whose source spans several
        // tiles (the Q16 tile selection), then a linked mask on it (mode 21).
        out.push(case(
            format!("renderer {tag} rotated raster, linked mask o=0.5"),
            key,
            vec![
                opaque(src),
                RendererLayer::Raster {
                    tiles: rasters(src[0] - 1..=src[0] + 1, src[1] - 1..=src[1] + 1),
                    transform: place(rotation(pivot, k_raster)),
                    adjust: Some(vivid),
                    selection: None,
                    mask: Some(RendererCaseMask {
                        linked: true,
                        transform: [1.0, 0.0, 0.0, 1.0, 37.0, -21.0],
                        tiles: masks(src[0] - 2..=src[0] + 1, src[1] - 2..=src[1] + 1),
                    }),
                    opacity: 0.5,
                    blend: RenderBlend::Normal,
                },
            ],
        ));
    }
    // A canvas edge through a tile near 10^6 px: the resolve span.
    let key = [3906, 7];
    out.push(RendererCase {
        canvas: [3906 * 256 + 100, 7 * 256 + 211],
        ..case(
            "renderer [3906, 7] canvas edge".into(),
            key,
            vec![raster(
                vec![(key, TileSpec::Raster(9))],
                ident(),
                Some(vivid),
                0.5,
                RenderBlend::Normal,
            )],
        )
    });
    // Level 1, the integer pyramid over four level-0 M5 tiles, with the raster
    // at the origin and at negative keys.
    for (key, src) in [([0, 0], [0, 0]), ([1, 1], [-4, -3])] {
        let shift = [
            f64::from(key[0] * 2 - src[0]) * 256.0,
            f64::from(key[1] * 2 - src[1]) * 256.0,
        ];
        let layers = vec![
            raster(
                rasters(src[0] - 1..=src[0] + 2, src[1] - 1..=src[1] + 2),
                crate::renderer::multiply(
                    [1.0, 0.0, 0.0, 1.0, shift[0], shift[1]],
                    rotation(src, 0.9),
                ),
                Some(vivid),
                0.5,
                RenderBlend::Normal,
            ),
            RendererLayer::Adjust {
                settings: (1.2, 0.5, -0.1),
                mask: None,
                opacity: 0.35,
                blend: RenderBlend::Normal,
            },
        ];
        out.push(RendererCase {
            level: 1,
            canvas: [(key[0] + 1) * 512, (key[1] + 1) * 512],
            ..case(
                format!("renderer {key:?} from {src:?} level 1"),
                key,
                layers,
            )
        });
    }
    // The plain integer tile compositor (no effect or mask, so not M5): an
    // opaque base shifted off the tile grid, a 1.00004x scale (Q16 within 6 of
    // unit: the nearest draw that was an f32 fetch) multiplied at opacity 0.7,
    // and a rotated 4-tap layer screened at 0.5, at the same sources as above.
    for (key, src, off) in [
        ([0, 0], [0, 0], [0.25, 0.0]),
        ([1, 1], [-3, -2], [37.5, -21.25]),
        ([3906, 5], [-3907, 5], [-11.75, 5.5]),
        ([32767, 32767], [32767, 32767], [0.0, 0.0]),
    ] {
        let shift = [
            f64::from(key[0] - src[0]) * 256.0 + off[0],
            f64::from(key[1] - src[1]) * 256.0 + off[1],
        ];
        let place =
            |t: [f64; 6]| crate::renderer::multiply([1.0, 0.0, 0.0, 1.0, shift[0], shift[1]], t);
        let (pivot, k_raster) = if key[0] > 30_000 {
            ([src[0] - 2, src[1] - 2], 2.0)
        } else {
            (src, 1.25)
        };
        let around = |k: [i32; 2]| (k[0] - 1..=k[0] + 1, k[1] - 1..=k[1] + 1);
        let (xs, ys) = around(src);
        let opaques: Vec<_> = ys
            .clone()
            .flat_map(|ty| {
                xs.clone()
                    .map(move |tx| ([tx, ty], TileSpec::Opaque(tx - ty)))
            })
            .collect();
        // A 1.00004x scale about the pivot tile's center.
        let (cx, cy) = (
            f64::from(pivot[0]) * 256.0 + 128.0,
            f64::from(pivot[1]) * 256.0 + 128.0,
        );
        let near_unit = [
            1.00004,
            0.0,
            0.0,
            1.00004,
            cx - 1.00004 * cx,
            cy - 1.00004 * cy,
        ];
        out.push(case(
            format!("plain {key:?} from {src:?} nearest, near-unit multiply, rotated screen"),
            key,
            vec![
                raster(opaques, place(ident()), None, 1.0, RenderBlend::Normal),
                raster(
                    rasters(xs.clone(), ys.clone()),
                    place(near_unit),
                    None,
                    0.7,
                    RenderBlend::Multiply,
                ),
                raster(
                    rasters(xs, ys),
                    place(rotation(pivot, k_raster)),
                    None,
                    0.5,
                    RenderBlend::Screen,
                ),
            ],
        ));
    }
    // The opaque-occluder shortcut: an opaque layer over all the tiles its
    // integer draw reads hides the layer below; one with a missing tile in
    // that range (here, half a pixel past the only tile) does not.
    for (name, tiles) in [
        (
            "covering",
            vec![([4, 4], TileSpec::Opaque(1)), ([5, 4], TileSpec::Opaque(2))],
        ),
        ("half a pixel short", vec![([4, 4], TileSpec::Opaque(1))]),
    ] {
        out.push(case(
            format!("plain [4, 4] occluder {name}"),
            [4, 4],
            vec![
                raster(
                    vec![([4, 4], TileSpec::Raster(3))],
                    ident(),
                    None,
                    1.0,
                    RenderBlend::Normal,
                ),
                raster(
                    tiles,
                    [1.0, 0.0, 0.0, 1.0, -0.5, 0.0],
                    None,
                    1.0,
                    RenderBlend::Normal,
                ),
            ],
        ));
    }
    // The plain resolve at a canvas edge near 10^6 px, and the plain pyramid.
    out.push(RendererCase {
        canvas: [3906 * 256 + 100, 7 * 256 + 211],
        ..case(
            "plain [3906, 7] canvas edge".into(),
            [3906, 7],
            vec![raster(
                vec![([3906, 7], TileSpec::Raster(9))],
                [1.0, 0.0, 0.0, 1.0, 0.5, -0.5],
                None,
                0.6,
                RenderBlend::Normal,
            )],
        )
    });
    out.push(RendererCase {
        level: 1,
        canvas: [512 * 2, 512 * 2],
        ..case(
            "plain [1, 1] from [-4, -3] level 1".into(),
            [1, 1],
            vec![raster(
                rasters(-5..=-2, -4..=-1),
                crate::renderer::multiply(
                    [1.0, 0.0, 0.0, 1.0, 6.0 * 256.0 + 0.5, 5.0 * 256.0],
                    rotation([-4, -3], 0.9),
                ),
                None,
                0.8,
                RenderBlend::Normal,
            )],
        )
    });
    // The browser's plain tile 32767 (Linus 08:10): the color-adjust gradient
    // (the browser imports `colorAdjustGradient`) moved there on the wide
    // fixture's canvas; also compared with JS (`renderer_rust_and_js_references_agree`).
    out.push(RendererCase {
        canvas: [8_389_120, 256],
        ..case(
            "plain [32767, 0] wide fixture, moved gradient".into(),
            [32767, 0],
            vec![RendererLayer::Raster {
                tiles: vec![([0, 0], TileSpec::Raster(0))],
                transform: [1.0, 0.0, 0.0, 1.0, 8_388_352.0, 0.0],
                adjust: None,
                selection: None,
                mask: None,
                opacity: 1.0,
                blend: RenderBlend::Normal,
            }],
        )
    });
    // Plain refusals: the first tile at 2^23 px, and an inverse with no Q16
    // value (a 10^-6 scale inverts to 10^6, past an i32 coefficient).
    out.push(RendererCase {
        refused: Some("v3 draw ROI has no integer tile range"),
        ..case(
            "plain [32768, 0] refused".into(),
            [32768, 0],
            vec![raster(
                vec![([32768, 0], TileSpec::Raster(1))],
                ident(),
                None,
                1.0,
                RenderBlend::Normal,
            )],
        )
    });
    out.push(RendererCase {
        refused: Some("v3 draw transform has no Q16 inverse"),
        ..case(
            "plain [0, 0] tiny scale refused".into(),
            [0, 0],
            vec![raster(
                vec![([0, 0], TileSpec::Raster(1))],
                [1e-6, 0.0, 0.0, 1e-6, 0.0, 0.0],
                None,
                1.0,
                RenderBlend::Normal,
            )],
        )
    });
    // Refused: the first tile at 2^23 px has no Q8 pixel center in i32, for the
    // raster (mode 18) and, with the raster near the origin, for a mask whose
    // corner lands past 2^23 px.
    let key = [32768, 0];
    out.push(RendererCase {
        refused: Some("v3 raster ROI has no integer tile range"),
        ..case(
            "renderer [32768, 0] raster refused".into(),
            key,
            vec![raster(
                vec![(key, TileSpec::Raster(1))],
                ident(),
                Some(vivid),
                1.0,
                RenderBlend::Normal,
            )],
        )
    });
    // A mask whose source passes 2^23 texels on tile (0, 0) from pixel 208 on
    // (DQ3-4, Linus 04:40): the shader drops the pixels whose mask Q8 leaves i32. With the mask
    // tiles ending at texel 2^23 - 1 those pixels read nothing either way, so it
    // draws (byte 0 there, as the CPU). With a mask tile at 2^23 they could read
    // it, so that tile is refused.
    for (name, tiles, refused) in [
        ("mask up to 2^23 draws", masks(32767..=32767, 0..=0), None),
        (
            "mask tile at 2^23 refused",
            masks(32767..=32768, 0..=0),
            Some("v3 mask ROI has no integer tile range"),
        ),
    ] {
        out.push(RendererCase {
            refused,
            ..case(
                format!("renderer [0, 0] {name}"),
                [0, 0],
                vec![RendererLayer::Raster {
                    tiles: vec![([0, 0], TileSpec::Opaque(2))],
                    transform: ident(),
                    adjust: None,
                    selection: None,
                    mask: Some(RendererCaseMask {
                        linked: false,
                        transform: [1.0, 0.0, 0.0, 1.0, -8_388_400.0, 0.0],
                        tiles,
                    }),
                    opacity: 1.0,
                    blend: RenderBlend::Normal,
                }],
            )
        });
    }
    out
}

/// A mask's atlas tiles and its document transform, for the CPU reference.
type PreparedMask = (Vec<([i32; 2], Vec<u8>)>, alto_core::FixedAffine);

/// CPU reference for a level-0 tile `key` of a [`RendererCase`], evaluated per
/// pixel as `renderer/m5.rs` chains the stages, with the checked Q14 functions
/// of [`q14_program_cpu`]. Every target shares the tile's ROI. Pixels whose
/// center is off the canvas are transparent (the exact doubled test, not
/// `resolve_span`).
fn renderer_tile_cpu(case: &RendererCase, key: [i32; 2]) -> Vec<u8> {
    if case.plain() {
        return plain_tile_cpu(case, key);
    }
    let region = [key[0] * 256, key[1] * 256, 1];
    let q = q14::Q;
    let clamp = |v: [i64; 4]| v.map(|c| c.clamp(0, q));
    let opacity = |o: f32| i64::from(crate::composite_opacity_q14(o));
    struct Prepared {
        raster: Vec<([i32; 2], Vec<u8>)>,
        mask: Option<PreparedMask>,
        selection: Option<PreparedMask>,
    }
    let prepared: Vec<Prepared> = case
        .layers
        .iter()
        .map(|layer| match layer {
            RendererLayer::Raster {
                tiles,
                transform,
                selection,
                mask,
                ..
            } => Prepared {
                raster: tiles.iter().map(|(k, spec)| (*k, spec.bytes())).collect(),
                mask: mask
                    .as_ref()
                    .map(|m| (m.rgba(), m.document_fixed(*transform).unwrap())),
                selection: selection
                    .as_ref()
                    .map(|m| (m.rgba(), m.document_fixed(*transform).unwrap())),
            },
            RendererLayer::Adjust { mask, .. } => Prepared {
                raster: vec![],
                mask: mask
                    .as_ref()
                    .map(|m| (m.rgba(), m.document_fixed(ident()).unwrap())),
                selection: None,
            },
        })
        .collect();
    let coverage = |m: &PreparedMask, x: i32, y: i32| {
        let tiles: Vec<([i32; 2], &[u8])> = m.0.iter().map(|(k, b)| (*k, &b[..])).collect();
        i64::from(byte_q14(u32::from(
            tiles_texel_fixed_cpu(&tiles, m.1, region, x, y).expect("v3 refuses the mask")[0],
        )))
    };
    let adjust = |v: [i64; 4], (contrast, saturation, brightness): (f32, f32, f32)| {
        let rgb = color_adjust_exact_q14(
            v.map(|c| c as u32),
            crate::renderer::color_adjust_q14(brightness),
            crate::renderer::color_adjust_q14(contrast),
            crate::renderer::color_adjust_q14(saturation),
        );
        clamp([rgb[0], rgb[1], rgb[2], v[3] as u32].map(i64::from))
    };
    let mut out = vec![0u8; 256 * 256 * 4];
    for y in 0..256 {
        for x in 0..256 {
            let on = |o: i32, i: i32, end: i32| {
                (0..2 * i64::from(end)).contains(&(2 * (i64::from(o) + i64::from(i)) + 1))
            };
            if !on(region[0], x, case.canvas[0]) || !on(region[1], y, case.canvas[1]) {
                continue;
            }
            let mut accum = [0i64; 4];
            for (layer, p) in case.layers.iter().zip(&prepared) {
                match layer {
                    RendererLayer::Raster {
                        transform,
                        adjust: settings,
                        opacity: o,
                        blend,
                        ..
                    } => {
                        let tiles: Vec<([i32; 2], &[u8])> =
                            p.raster.iter().map(|(k, b)| (*k, &b[..])).collect();
                        let bytes = tiles_texel_cpu(&tiles, *transform, region, x, y)
                            .expect("v3 refuses the raster")
                            .map(u32::from);
                        let mut src = [
                            premul_byte_q14(bytes[0], bytes[3]),
                            premul_byte_q14(bytes[1], bytes[3]),
                            premul_byte_q14(bytes[2], bytes[3]),
                            byte_q14(bytes[3]),
                        ]
                        .map(i64::from);
                        if let Some(settings) = settings {
                            let adjusted = adjust(src, *settings);
                            src = match &p.selection {
                                Some(m) => clamp(
                                    mask_select_q14(src, adjusted, coverage(m, x, y)).unwrap(),
                                ),
                                None => adjusted,
                            };
                        }
                        let mut op = opacity(*o);
                        if let Some(m) = &p.mask {
                            src = clamp(mask_layer_q14(src, coverage(m, x, y) * op).unwrap());
                            op = q;
                        }
                        let mode = *blend as u32;
                        accum = clamp(if op == q {
                            q14::blend(
                                accum,
                                q14::straight(src),
                                [src[0], src[1], src[2]],
                                src[3],
                                mode,
                            )
                        } else {
                            q14::pop(accum, src, op, mode)
                        });
                    }
                    RendererLayer::Adjust {
                        settings,
                        opacity: o,
                        blend,
                        ..
                    } => {
                        let adjusted = adjust(accum, *settings);
                        let op = opacity(*o);
                        accum = match &p.mask {
                            Some(m) => clamp(
                                mask_adjust_q14(
                                    accum,
                                    adjusted,
                                    coverage(m, x, y) * op,
                                    *blend as u32,
                                )
                                .unwrap(),
                            ),
                            None if op != q => std::array::from_fn(|c| {
                                div_q_i64(accum[c] * (q - op) + adjusted[c] * op, q).unwrap()
                            }),
                            None => adjusted,
                        };
                    }
                }
            }
            let i = ((y * 256 + x) * 4) as usize;
            out[i..i + 4].copy_from_slice(&q14::bytes(accum));
        }
    }
    out
}

/// CPU reference for a [`RendererCase`]: the level-0 tile, or at level 1 the
/// pyramid's `(a + b + c + d + 2) / 4` over the four level-0 children
/// (`downsample-q14.wgsl`), all of which the cases keep on the canvas.
fn renderer_case_cpu(case: &RendererCase) -> Vec<u8> {
    if case.level == 0 {
        return renderer_tile_cpu(case, case.key);
    }
    assert_eq!(case.level, 1, "{}", case.name);
    let children: Vec<Vec<u8>> = (0..4)
        .map(|i| renderer_tile_cpu(case, [case.key[0] * 2 + i % 2, case.key[1] * 2 + i / 2]))
        .collect();
    let mut out = vec![0u8; 256 * 256 * 4];
    for y in 0..256 {
        for x in 0..256 {
            let child = &children[(y / 128) * 2 + x / 128];
            for c in 0..4 {
                let at = |dx: usize, dy: usize| {
                    u32::from(child[(((y % 128) * 2 + dy) * 256 + (x % 128) * 2 + dx) * 4 + c])
                };
                out[(y * 256 + x) * 4 + c] =
                    ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8;
            }
        }
    }
    out
}

impl RendererCase {
    /// A copy with the same canvas and layers.
    fn clone_layers(&self) -> Self {
        Self {
            name: self.name.clone(),
            level: self.level,
            key: self.key,
            canvas: self.canvas,
            layers: self.layers.clone(),
            refused: self.refused,
        }
    }
    /// Whether every layer is a raster with no effect or mask: the renderer
    /// draws such a scene with the integer tile compositor, not M5.
    fn plain(&self) -> bool {
        self.layers.iter().all(|l| {
            matches!(
                l,
                RendererLayer::Raster {
                    adjust: None,
                    selection: None,
                    mask: None,
                    ..
                }
            )
        })
    }
}

/// CPU reference for a level-0 tile `key` of a plain [`RendererCase`]: the
/// draws of [`composite_integer`], every tile of every layer listed (the
/// renderer's tile selection and opaque-occluder shortcut must not change it).
fn plain_tile_cpu(case: &RendererCase, key: [i32; 2]) -> Vec<u8> {
    let ops: Vec<Op> = case
        .layers
        .iter()
        .filter_map(|layer| {
            let RendererLayer::Raster {
                tiles,
                transform,
                opacity,
                blend,
                ..
            } = layer
            else {
                unreachable!("a plain case has rasters only");
            };
            let mut tiles: Vec<Tile> = tiles
                .iter()
                .map(|(k, spec)| Tile {
                    tx: k[0],
                    ty: k[1],
                    pixels: spec.bytes(),
                })
                .collect();
            tiles.sort_by_key(|t| (t.tx, t.ty));
            let fixed = alto_core::FixedAffine::from_affine(Affine2(*transform))?;
            let q = crate::sample::fixed_inverse(&fixed)??;
            Some(Op::Draw {
                inv: [0.0; 6],
                q: Some(q),
                opacity: *opacity,
                mode: *blend as u32,
                tiles,
            })
        })
        .collect();
    let frame = Frame {
        origin: [0.0; 2],
        scale: 1.0,
        origin_px: [key[0] * 256, key[1] * 256],
        scale_i: 1,
        version: 3,
    };
    composite_integer(&ops, case.canvas[0] as u32, case.canvas[1] as u32, &frame).0
}

/// A display tile `(level, tx, ty)` and the refusal it must give, if any.
type ColdWarmKey = ((i32, i32, i32), Option<&'static str>);

/// A scene change for [`renderer_cold_warm`]: `before` and `after` share the
/// canvas and node identities; `keys` are the display tiles read after it,
/// each with the refusal it must give (`None`: drawn, equal to the CPU).
struct ColdWarmCase {
    name: &'static str,
    before: RendererCase,
    after: RendererCase,
    keys: Vec<ColdWarmKey>,
}

/// The scene changes of [`renderer_cold_warm`] (Linus 04:40 item 5): the
/// large-translation-out-of-range layer, the 2^23 boundary (drawn, refused, and
/// a far layer at the refused origin), a moved layer read at levels 0 and 1,
/// a source tile added where the f64 and integer selections part (the
/// 999936 case), and a masked M5 scene.
fn cold_warm_cases() -> Vec<ColdWarmCase> {
    let blank = |canvas: [i32; 2]| RendererCase {
        name: String::new(),
        level: 0,
        key: [0, 0],
        canvas,
        layers: vec![],
        refused: None,
    };
    let with = |canvas: [i32; 2], layers: Vec<RendererLayer>| RendererCase {
        layers,
        ..blank(canvas)
    };
    let raster = |tiles: Vec<([i32; 2], TileSpec)>, transform: [f64; 6]| RendererLayer::Raster {
        tiles,
        transform,
        adjust: None,
        selection: None,
        mask: None,
        opacity: 0.8,
        blend: RenderBlend::Normal,
    };
    let (s, c) = 0.3f64.sin_cos();
    let rotated = [c, s, -s, c, 300.25, 17.5];
    let far = [0.9999900000999989, 0.0, 0.0, 1.0, 11.00180998190018, 0.0];
    let edge = [3908 * 256, 256];
    let boundary = [32769 * 256, 256];
    let masked = renderer_mask_cases().swap_remove(1);
    // The browser's 2^23 document (Linus 08:10): the empty v3 fixture 8388864 +
    // 256 px wide, opened through `open_package` (the `engine.open` path), with
    // one 256x256 raster moved by a translation, as the browser test imports it.
    let wide = {
        let bytes = include_bytes!("../../../tests/fixtures/render-v3-empty-8389120x256.alto");
        let mut engine = alto_engine::Engine::new();
        let (doc, warnings) = engine.open_package(bytes).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let scene = engine.render_scene(doc).unwrap();
        assert_eq!(scene.render_version, alto_core::RENDER_VERSION_EXACT);
        [scene.canvas.x1, scene.canvas.y1]
    };
    assert_eq!(wide, [8_389_120, 256]);
    let moved = |x: f64| {
        with(
            wide,
            vec![RendererLayer::Raster {
                tiles: vec![([0, 0], TileSpec::Raster(8))],
                transform: [1.0, 0.0, 0.0, 1.0, x, 0.0],
                adjust: None,
                selection: None,
                mask: None,
                opacity: 1.0,
                blend: RenderBlend::Normal,
            }],
        )
    };
    vec![
        ColdWarmCase {
            name: "wide fixture: plain tile 32767 draws",
            before: blank(wide),
            after: moved(8_388_352.0),
            keys: vec![((0, 32_767, 0), None)],
        },
        ColdWarmCase {
            name: "wide fixture: plain tile 32768 with a contribution is refused",
            before: blank(wide),
            after: moved(8_388_608.0),
            keys: vec![(
                (0, 32_768, 0),
                Some("v3 draw ROI has no integer tile range"),
            )],
        },
        ColdWarmCase {
            name: "wide fixture: plain tile 32768 without a contribution is transparent",
            before: blank(wide),
            after: moved(8_388_352.0),
            keys: vec![((0, 32_768, 0), None)],
        },
        ColdWarmCase {
            name: "large-translation-out-of-range",
            before: blank([256, 256]),
            after: with(
                [256, 256],
                vec![raster(
                    vec![([0, 0], TileSpec::Raster(1))],
                    [0.01, 0.0, 0.0, 0.01, 84_000.0, 0.0],
                )],
            ),
            keys: vec![((0, 0, 0), None)],
        },
        ColdWarmCase {
            name: "2^23 boundary",
            before: blank(boundary),
            after: with(
                boundary,
                vec![
                    raster(vec![([32_767, 0], TileSpec::Raster(2))], ident()),
                    raster(vec![([32_768, 0], TileSpec::Raster(3))], ident()),
                ],
            ),
            keys: vec![
                ((0, 32_767, 0), None),
                (
                    (0, 32_768, 0),
                    Some("v3 draw ROI has no integer tile range"),
                ),
            ],
        },
        ColdWarmCase {
            name: "far layer at the 2^23 origin",
            before: blank(boundary),
            after: with(
                boundary,
                vec![raster(vec![([0, 0], TileSpec::Raster(4))], ident())],
            ),
            keys: vec![((0, 32_768, 0), None), ((0, 0, 0), None)],
        },
        ColdWarmCase {
            name: "moved layer",
            before: with(
                [1024, 512],
                vec![raster(
                    (0..=2).map(|x| ([x, 0], TileSpec::Raster(x))).collect(),
                    [1.0, 0.0, 0.0, 1.0, 0.5, 0.25],
                )],
            ),
            after: with(
                [1024, 512],
                vec![raster(
                    (0..=2).map(|x| ([x, 0], TileSpec::Raster(x))).collect(),
                    rotated,
                )],
            ),
            keys: vec![
                ((0, 0, 0), None),
                ((0, 1, 0), None),
                ((0, 2, 0), None),
                ((0, 1, 1), None),
                ((1, 0, 0), None),
            ],
        },
        ColdWarmCase {
            name: "source tile added at the 999936 edge",
            before: with(
                edge,
                vec![raster(
                    vec![
                        ([3905, 0], TileSpec::Raster(5)),
                        ([3906, 0], TileSpec::Raster(6)),
                    ],
                    far,
                )],
            ),
            after: with(
                edge,
                vec![raster(
                    vec![
                        ([3905, 0], TileSpec::Raster(5)),
                        ([3906, 0], TileSpec::Raster(6)),
                        ([3907, 0], TileSpec::Opaque(7)),
                    ],
                    far,
                )],
            ),
            keys: vec![((0, 3906, 0), None)],
        },
        ColdWarmCase {
            name: "masked M5",
            before: blank(RendererCase::from(&masked).canvas),
            after: RendererCase::from(&masked),
            keys: vec![((0, masked.key[0], masked.key[1]), None)],
        },
    ]
}

/// Linus 04:40 item 5: each [`cold_warm_cases`] scene read warm (the tiles of
/// `before` cached, then `scene_damage` applied) and cold (caches reset). Every
/// pair, per key: the warm and cold results.
type ColdWarm = Vec<(
    Result<Vec<u8>, crate::RenderError>,
    Result<Vec<u8>, crate::RenderError>,
)>;

fn renderer_cold_warm(cases: &[ColdWarmCase]) -> Result<Vec<ColdWarm>, String> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::METAL | wgpu::Backends::VULKAN | wgpu::Backends::DX12;
    let instance = wgpu::Instance::new(descriptor);
    let mut renderer = pollster::block_on(crate::renderer::Renderer::headless(&instance, 256))
        .map_err(|e| format!("headless renderer: {e}"))?;
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
    let out = cases
        .iter()
        .map(|case| {
            let ids: Vec<NodeId> = (0..=case.before.layers.len().max(case.after.layers.len()))
                .map(|_| NodeId::new())
                .collect();
            let mut store = alto_raster::ObjectStore::default();
            let before = renderer_scene(&case.before, &mut store, &ids);
            let after = renderer_scene(&case.after, &mut store, &ids);
            let read = |r: &mut crate::renderer::Renderer, scene: &RenderScene, key| {
                pollster::block_on(r.read_tile(scene, &store, key))
            };
            renderer.reset_render_cache();
            for (key, _) in &case.keys {
                let _ = read(&mut renderer, &before, *key);
            }
            renderer.invalidate_damage(&crate::damage::scene_damage(&before, &after));
            let warm: Vec<_> = case
                .keys
                .iter()
                .map(|(k, _)| read(&mut renderer, &after, *k))
                .collect();
            let cold: Vec<_> = case
                .keys
                .iter()
                .map(|(k, _)| {
                    renderer.reset_render_cache();
                    read(&mut renderer, &after, *k)
                })
                .collect();
            warm.into_iter().zip(cold).collect()
        })
        .collect();
    done.store(true, std::sync::atomic::Ordering::Release);
    poller
        .join()
        .map_err(|_| "device poller panicked".to_string())?;
    Ok(out)
}

/// One render result per [`RendererCase`].
type RendererResults = Vec<Result<Vec<u8>, crate::RenderError>>;

/// [`RendererCase`]s composed by one headless `Renderer` (caches reset between
/// cases) and read back, each with its render result.
/// The render scene of a [`RendererCase`], its tiles inserted into `store`.
/// `ids` (root first, then one per layer) keep node identities across two
/// scenes, so their `scene_damage` sees moves and edits rather than new nodes.
fn renderer_scene(
    case: &RendererCase,
    store: &mut alto_raster::ObjectStore,
    ids: &[NodeId],
) -> RenderScene {
    use alto_core::{PixelFormat, TileKey};
    use alto_raster::{ObjectStore, TileData};
    let tiles = |store: &mut ObjectStore, format, list: &[([i32; 2], TileSpec)]| {
        list.iter()
            .map(|(key, spec)| crate::RenderTile {
                key: TileKey {
                    tx: key[0],
                    ty: key[1],
                },
                hash: store.insert_tile(&TileData::new(format, spec.bytes()).expect("tile size")),
            })
            .collect::<Vec<_>>()
    };
    let mask = |store: &mut ObjectStore, m: &Option<RendererCaseMask>| {
        m.as_ref().map(|m| RMask {
            tiles: tiles(store, PixelFormat::R8, &m.tiles),
            linked: m.linked,
            transform: m.transform,
        })
    };
    let children: Vec<usize> = (1..=case.layers.len()).collect();
    let mut scene = scene(
        RContent::Group {
            children,
            isolated: false,
        },
        RFeatures::default(),
    );
    scene.render_version = alto_core::RENDER_VERSION_EXACT;
    if let Some(root) = ids.first() {
        scene.nodes[0].key = *root;
    }
    scene.canvas = IRect {
        x0: 0,
        y0: 0,
        x1: case.canvas[0],
        y1: case.canvas[1],
    };
    let color_adjust =
        |(contrast, saturation, brightness): (f32, f32, f32)| REffectKind::ColorAdjust {
            brightness,
            contrast,
            saturation,
        };
    for (i, layer) in case.layers.iter().enumerate() {
        let mut node = scene.nodes[0].clone();
        node.key = ids.get(i + 1).copied().unwrap_or_else(NodeId::new);
        node.features = RFeatures::default();
        match layer {
            RendererLayer::Raster {
                tiles: raster,
                transform,
                adjust,
                selection,
                mask: m,
                opacity,
                blend,
            } => {
                node.transform = *transform;
                node.opacity = *opacity;
                node.blend = *blend;
                node.content = RContent::Tiles(tiles(store, PixelFormat::Rgba8, raster));
                node.features.mask = mask(store, m);
                if let Some(settings) = adjust {
                    node.features.effects = vec![REffect {
                        kind: color_adjust(*settings),
                        selection: mask(store, selection),
                    }];
                }
            }
            RendererLayer::Adjust {
                settings,
                mask: m,
                opacity,
                blend,
            } => {
                node.opacity = *opacity;
                node.blend = *blend;
                node.content = RContent::Adjustment(REffect {
                    kind: color_adjust(*settings),
                    selection: None,
                });
                node.features.mask = mask(store, m);
            }
        }
        node.effects = node.features.effects.len();
        scene.nodes.push(node);
    }
    scene
}

fn renderer_gpu(cases: &[RendererCase]) -> Result<RendererResults, String> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::METAL | wgpu::Backends::VULKAN | wgpu::Backends::DX12;
    let instance = wgpu::Instance::new(descriptor);
    let mut renderer = pollster::block_on(crate::renderer::Renderer::headless(&instance, 256))
        .map_err(|e| format!("headless renderer: {e}"))?;
    // Native `map_async` completes only while the device is polled.
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
    let result = cases
        .iter()
        .map(|case| {
            let mut store = alto_raster::ObjectStore::default();
            let scene = renderer_scene(case, &mut store, &[]);
            renderer.reset_render_cache();
            pollster::block_on(renderer.read_tile(
                &scene,
                &store,
                (case.level, case.key[0], case.key[1]),
            ))
        })
        .collect();
    done.store(true, std::sync::atomic::Ordering::Release);
    poller
        .join()
        .map_err(|_| "device poller panicked".to_string())?;
    Ok(result)
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

/// Render version 3 nearest (`SAMPLE_NEAREST`): the texel under the Q8 position.
fn fetch_nearest_bytes(
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
fn composite_integer(ops: &[Op], width: u32, height: u32, frame: &Frame) -> (Vec<u8>, Vec<f32>) {
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

fn composite(ops: &[Op], width: u32, height: u32, frame: &Frame) -> (Vec<u8>, Vec<f32>) {
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

fn render_pair(case: &Case, version: u32) -> (Vec<u8>, Vec<f32>) {
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
fn downsample_q14(children: &[Vec<u8>; 4]) -> Vec<u8> {
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
fn pyramid_children(case: &Case) -> [Case; 4] {
    std::array::from_fn(|i| {
        let mut child = case.clone();
        child.level -= 1;
        child.tx = case.tx * 2 + (i % 2) as i32;
        child.ty = case.ty * 2 + (i / 2) as i32;
        child
    })
}
fn render_pyramid(case: &Case) -> Vec<u8> {
    if case.level <= 0 {
        return render_at(case, 3);
    }
    downsample_q14(&pyramid_children(case).map(|c| render_pyramid(&c)))
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
    render_path: Option<&'static str>,
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
        render_path: None, // Case fixtures contain only plain raster/group nodes.
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

/// The `unorm_byte` that every v3 atlas read uses (`q14.wgsl`). The atlas-read
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

/// Result of reading all 256 bytes from an `Rgba8Unorm` (all 4 channels) or an
/// `R8Unorm` (the v3 glyph atlas and scratch, `.r`) texture with `textureLoad` +
/// `unorm_byte`. Enforced.
struct AtlasRead {
    format: &'static str,
    mismatches: Vec<(usize, usize, u8, u32)>,
}

impl AtlasRead {
    fn line(&self) -> String {
        let channels = if self.format == "R8Unorm" {
            "the r channel"
        } else {
            "all 4 channels"
        };
        if self.mismatches.is_empty() {
            format!(
                "atlas-read {} (enforced): 256/256 byte values round-trip exactly on {channels}",
                self.format
            )
        } else {
            let shown: Vec<String> = self
                .mismatches
                .iter()
                .take(8)
                .map(|(t, c, want, got)| format!("texel {t} ch {c} byte {want} read {got}"))
                .collect();
            format!(
                "atlas-read {} (enforced): {} channel mismatches: {}",
                self.format,
                self.mismatches.len(),
                shown.join("; ")
            )
        }
    }
}

/// The JSON fields of one atlas read.
fn atlas_read_fields(result: &Result<AtlasRead, String>) -> String {
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
    format!(
        "\"read\": \"textureLoad + unorm_byte\", \"values\": 256, \"status\": \"{status}\", \"mismatchedChannels\": {count}, \"error\": \"{}\", \"mismatches\": [{list}]",
        json_escape(&error)
    )
}

/// `atlas-read.json`: the `Rgba8Unorm` read at the top level (as before) and the
/// `R8Unorm` read under `r8Unorm`.
fn write_atlas_read(
    adapter: &str,
    rgba: &Result<AtlasRead, String>,
    r8: &Result<AtlasRead, String>,
) {
    let Some(dir) = std::env::var_os("ALTO_ALPHA_MEASURE_OUT") else {
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("create alpha measure directory");
    let json = format!(
        "{{\"label\": \"enforced\", \"backend\": \"{}\", \"format\": \"Rgba8Unorm\", {}, \"r8Unorm\": {{\"format\": \"R8Unorm\", {}}}}}\n",
        json_escape(adapter),
        atlas_read_fields(rgba),
        atlas_read_fields(r8)
    );
    std::fs::write(dir.join("atlas-read.json"), json).expect("write atlas read json");
    let line = |result: &Result<AtlasRead, String>, format: &str| match result {
        Ok(r) => r.line(),
        Err(e) => format!("atlas-read {format} (enforced): GPU error: {e}"),
    };
    let markdown = format!(
        "### Atlas read (`textureLoad` + `unorm_byte`)\n\nBackend: `{}`\n\n- {}\n- {}\n",
        adapter,
        line(rgba, "Rgba8Unorm"),
        line(r8, "R8Unorm")
    );
    std::fs::write(dir.join("atlas-read.md"), markdown).expect("write atlas read markdown");
}

fn write_alpha_measure(adapter: &str, rows: &[MeasuredCase]) {
    write_measure(adapter, rows, false, &[]);
}
fn write_v3_measure(adapter: &str, rows: &[MeasuredCase], errors: &[String]) {
    write_measure(adapter, rows, true, errors);
}
fn write_measure(adapter: &str, rows: &[MeasuredCase], integer: bool, errors: &[String]) {
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
        let enforced = integer && row.render_path != Some("v3-unsupported");
        let render_path = row
            .render_path
            .map_or("null".to_string(), |p| format!("\"{p}\""));
        if index > 0 {
            json.push_str(",\n");
        }
        json.push_str(&format!(
            "    {{\"name\": \"{}\", \"renderPath\": {render_path}, \"enforced\": {enforced}, \"status\": \"{}\", \"count\": {}, \"maxAbs\": [{},{},{},{}], \"mismatches\": [",
            json_escape(&row.name),
            if row.count==0 && row.length_note.is_empty() {"exact"} else {"mismatch"},
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
    json.push_str("\n  ]");
    if integer {
        let exact_count = rows
            .iter()
            .filter(|r| {
                r.render_path != Some("v3-unsupported") && r.count == 0 && r.length_note.is_empty()
            })
            .count();
        let unsupported_count = rows
            .iter()
            .filter(|r| r.render_path == Some("v3-unsupported"))
            .count();
        json.push_str(&format!(
            ",\n  \"integerExactCount\": {exact_count},\n  \"v3UnsupportedCount\": {unsupported_count}"
        ));
        let status = if !errors.is_empty() {
            "error"
        } else if rows.iter().any(|r| {
            r.render_path != Some("v3-unsupported") && (r.count != 0 || !r.length_note.is_empty())
        }) {
            "mismatch"
        } else {
            "exact"
        };
        let list = errors
            .iter()
            .map(|e| format!("\"{}\"", json_escape(e)))
            .collect::<Vec<_>>()
            .join(",");
        json.push_str(&format!(
            ",\n  \"status\": \"{status}\",\n  \"errors\": [{list}]"
        ));
    }
    json.push_str("\n}\n");
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
            "| {}{} | {} | {},{},{},{} | {}{} |\n",
            row.name,
            if row.render_path == Some("v3-unsupported") {
                " (v3-unsupported; refused by the host)"
            } else {
                ""
            },
            row.count,
            row.max_abs[0],
            row.max_abs[1],
            row.max_abs[2],
            row.max_abs[3],
            shown,
            row.length_note
        ));
    }
    if integer && !errors.is_empty() {
        markdown.push_str("\nGPU errors:\n\n");
        for error in errors {
            markdown.push_str(&format!("- {error}\n"));
        }
    }
    if integer {
        json = json.replacen("measured-not-enforced", "enforced", 1);
        markdown = markdown
            .replacen("measured-not-enforced", "enforced", 1)
            .replace("A mismatch does not fail", "A mismatch fails");
    }
    std::fs::write(
        dir.join(if integer {
            "composite-v3.json"
        } else {
            "alpha-measure.json"
        }),
        json,
    )
    .expect("write alpha measure json");
    std::fs::write(
        dir.join(if integer {
            "composite-v3.md"
        } else {
            "alpha-measure.md"
        }),
        markdown,
    )
    .expect("write alpha measure markdown");
    if !integer {
        write_prequant(&dir, adapter, rows);
    }
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

/// The numeric parity host refuses v3 modes 17 and up before any command is
/// built, as the renderer does in `prepare_scene` (DQ3-3: no new variant).
fn has_unspecified_v3_mode(ops: &[Op]) -> bool {
    ops.iter().any(|op| match op {
        Op::Pop { mode, .. } | Op::Draw { mode, .. } => *mode >= 17,
        _ => false,
    })
}

/// Version 3 has exactly three classes (`renderer::v3_gap`): `None` (integer tile
/// compositor), `v3-m5-integer` (integer M5 stages) and `v3-unsupported` (refused
/// by the host). Versions 1 and 2 keep their first-gap names.
fn gap(scene: &RenderScene) -> Option<&'static str> {
    if scene.render_version == 3 {
        if crate::renderer::v3_gap(scene).is_some() {
            return Some("v3-unsupported");
        }
        return (!plain_scene(scene)).then_some("v3-m5-integer");
    }
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

fn integer_parity_shader() -> String {
    let source = crate::integer_composite_shader();
    let marker = "  output[id.y*256u+id.x]=";
    assert_eq!(source.matches(marker).count(), 1);
    source.replace("@compute @workgroup_size(16,16)",
        "@group(0) @binding(5) var<storage,read_write> pre_quant:array<vec4f>;\n@compute @workgroup_size(16,16)")
        .replace(marker, &format!("  pre_quant[id.y*256u+id.x]=vec4f(accum)/16384.0;\n{marker}"))
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

    /// Upload all 256 byte values to an `Rgba8Unorm` or `R8Unorm` texture and read
    /// them back with `textureLoad` + `unorm_byte`. Returns every channel that does
    /// not round-trip (`R8Unorm`: the r channel only). Enforced: the caller
    /// records it and fails on any mismatch.
    fn atlas_read(&self, r8: bool) -> Result<AtlasRead, String> {
        let (format, label, texel_bytes) = if r8 {
            (wgpu::TextureFormat::R8Unorm, "R8Unorm", 1)
        } else {
            (wgpu::TextureFormat::Rgba8Unorm, "Rgba8Unorm", 4)
        };
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
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let data: Vec<u8> = (0..256)
            .flat_map(|i| atlas_read_texel(i)[..texel_bytes].to_vec())
            .collect();
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
                bytes_per_row: Some(256 * texel_bytes as u32),
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
            for ch in 0..texel_bytes {
                let value = got[texel * 4 + ch];
                if value != u32::from(want[ch]) {
                    mismatches.push((texel, ch, want[ch], value));
                }
            }
        }
        Ok(AtlasRead {
            format: label,
            mismatches,
        })
    }

    fn render(&self, case: &Case) -> Result<(Vec<u8>, Vec<f32>), String> {
        self.render_version(case, 2)
    }

    fn downsample_q14(&self, children: &[Vec<u8>; 4]) -> Result<Vec<u8>, String> {
        let device = &self.device;
        let input = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("integer pyramid parity input"),
            size: wgpu::Extent3d {
                width: 512,
                height: 512,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (i, child) in children.iter().enumerate() {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &input,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: (i as u32 % 2) * 256,
                        y: (i as u32 / 2) * 256,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                child,
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
            label: Some("integer pyramid parity output"),
            size: wgpu::Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Uint,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("integer pyramid parity"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/downsample-q14.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let config = upload_buffer(
            device,
            &self.queue,
            &[[0u32; 4]],
            wgpu::BufferUsages::UNIFORM,
        );
        let input_view = input.create_view(&Default::default());
        let output_view = output.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: config.as_entire_binding(),
                },
            ],
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * 1024,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(16, 16, 1);
        }
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
        self.queue.submit(Some(encoder.finish()));
        map_readback(device, &read)?;
        let bytes = read
            .slice(..)
            .get_mapped_range()
            .map_err(|e| e.to_string())?
            .to_vec();
        read.unmap();
        Ok(bytes)
    }
    fn render_pyramid(&self, case: &Case) -> Result<Vec<u8>, String> {
        if case.level <= 0 {
            return self.render_version(case, 3).map(|p| p.0);
        }
        let children = pyramid_children(case).map(|c| self.render_pyramid(&c));
        let [a, b, c, d] = children;
        self.downsample_q14(&[a?, b?, c?, d?])
    }

    fn render_version(&self, case: &Case, version: u32) -> Result<(Vec<u8>, Vec<f32>), String> {
        let integer = version == 3;
        let mut ops = Vec::new();
        for node in &case.nodes {
            if integer {
                walk_v3(node, alto_core::FixedAffine::IDENTITY, &mut ops);
            } else {
                walk(node, ident(), &mut ops);
            }
        }
        if integer && has_unspecified_v3_mode(&ops) {
            return Err("v3 unspecified blend mode".into());
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
                    q,
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
                    // Version 3 takes its draw from the host (`plain_draw_v3`).
                    if integer {
                        let d = crate::renderer::plain_draw_v3(
                            q.expect("v3 draw inverse"),
                            (case.level, case.tx, case.ty),
                            [case.w as i32, case.h as i32],
                            tiles
                                .iter()
                                .map(|t| alto_core::TileKey { tx: t.tx, ty: t.ty }),
                        )
                        .map_err(|e| format!("{}: {e}", case.name))?;
                        // A draw that reaches none of its tiles is skipped, as the planners do.
                        if d.keys.is_none() {
                            continue;
                        }
                        commands.push(GpuCommand {
                            inv0: d.inverse.m.map(|v| f32::from_bits(v.cast_unsigned())),
                            inv1: [0.0, 0.0, *opacity, *mode as f32],
                            info: [0, start, entries.len() as u32 - start, d.sample],
                            trans: crate::sample::pack_wide(d.inverse.tx, d.inverse.ty),
                        });
                        continue;
                    }
                    let packed = crate::sample::pack_inverse(*inv, version);
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
        if integer {
            for c in &mut commands {
                c.inv1[2] = f32::from_bits(crate::composite_opacity_q14(c.inv1[2]) as u32);
                c.inv1[3] = f32::from_bits(c.inv1[3] as u32);
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
        let integer_output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("integer parity bytes"),
            size: 256 * 256 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let integer_pipeline = if integer {
            let src = integer_parity_shader();
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("integer parity"),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            });
            Some(
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("integer parity"),
                    layout: None,
                    module: &module,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    cache: None,
                }),
            )
        } else {
            None
        };
        let pipeline = integer_pipeline.as_ref().unwrap_or(&self.pipeline);
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
        let config_buf = if integer {
            // The host's integer uniform: origin, command count, on-canvas span.
            let origin = [case.tx, case.ty].map(|k| i64::from(k) * 256);
            upload_buffer(
                device,
                &self.queue,
                &[crate::renderer::IntegerConfig {
                    origin_scale: [origin[0] as i32, origin[1] as i32, 1, 0],
                    info: [commands.len() as u32, 0, 0, 0],
                    span: crate::renderer::resolve_span(
                        origin,
                        1,
                        [i64::from(case.w), i64::from(case.h)],
                    ),
                }],
                wgpu::BufferUsages::UNIFORM,
            )
        } else {
            upload_buffer(device, &self.queue, &config, wgpu::BufferUsages::UNIFORM)
        };
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
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: if integer {
                        integer_output.as_entire_binding()
                    } else {
                        wgpu::BindingResource::TextureView(&output_view)
                    },
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
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(16, 16, 1);
        }
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("parity readback"),
            size: 256 * 1024,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        if integer {
            encoder.copy_buffer_to_buffer(&integer_output, 0, &read, 0, 256 * 1024);
        } else {
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
        }
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

    /// Runs Q14 stage programs through the production `effects-q14.wgsl` and
    /// `effects-resolve-q14.wgsl`. Targets are `rgba16uint` and start at zero, so
    /// an unwritten target is the empty backdrop. Each program's result target is
    /// resolved to `rgba8uint` and read back.
    fn render_q14(
        &self,
        images: &[Vec<u8>],
        programs: &[Q14Program],
    ) -> Result<Vec<Vec<u8>>, String> {
        let device = &self.device;
        let compile = |label: &str, source: String| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: None,
                module: &device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(label),
                    source: wgpu::ShaderSource::Wgsl(source.into()),
                }),
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let effects = compile("q14 stages", crate::integer_effects_shader());
        let resolve = compile(
            "q14 resolve",
            include_str!("../shaders/effects-resolve-q14.wgsl").to_string(),
        );
        let size = |layers: usize| wgpu::Extent3d {
            width: 256,
            height: 256,
            depth_or_array_layers: layers as u32,
        };
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("q14 atlas"),
            size: size(images.len()),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (layer, pixels) in images.iter().enumerate() {
            if pixels.len() != 256 * 256 * 4 {
                return Err(format!("q14 image {layer} is {} bytes", pixels.len()));
            }
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(1024),
                    rows_per_image: Some(256),
                },
                size(1),
            );
        }
        let atlas_v = atlas.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("q14 resolve"),
            size: size(1),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Uint,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_v = output.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        // The programs have no canvas: the resolve span is the whole tile, and
        // the slot is 0 (`effects-resolve-q14.wgsl` `span`, `info`).
        let resolve_config = upload_buffer(
            device,
            &self.queue,
            &[[0u32, 0, 256, 256], [0; 4]],
            wgpu::BufferUsages::UNIFORM,
        );
        let mut tiles_out = Vec::with_capacity(programs.len());
        for program in programs {
            let targets: Vec<_> = (0..program.targets)
                .map(|_| {
                    device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("q14 target"),
                        size: size(1),
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba16Uint,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::STORAGE_BINDING,
                        view_formats: &[],
                    })
                })
                .collect();
            let views: Vec<_> = targets
                .iter()
                .map(|t| t.create_view(&Default::default()))
                .collect();
            let mut encoder = device.create_command_encoder(&Default::default());
            let mut keep = Vec::new();
            for stage in &program.stages {
                if stage.out == stage.a || stage.out == stage.b {
                    return Err(format!("{}: stage reads its own output", program.name));
                }
                let tile = [GpuTile {
                    xy: [0, 0],
                    slot: stage.slot,
                    pad: 0,
                }];
                let tiles = upload_buffer(device, &self.queue, &tile, wgpu::BufferUsages::STORAGE);
                // Every stage takes its whole uniform from the host's
                // `mask_sampler`, `input_map` and `mask_grid`, as
                // `Context::q14_params` builds it; the float slots stay zero.
                let f32_region = |t: usize| program.region(t).map(|v| v as f32);
                let out_region = f32_region(stage.out);
                let sampler = stage
                    .lookup_transform(program.region(stage.out))
                    .map(|t| {
                        crate::renderer::fixed_mask_sampler(
                            &alto_core::FixedAffine::from_affine(Affine2(t)).ok_or_else(|| {
                                format!("{}: v3 transform out of range", program.name)
                            })?,
                            program.region(stage.out),
                        )
                        .ok_or_else(|| format!("{}: v3 refuses the lookup", program.name))
                    })
                    .transpose()?;
                let map = |t: usize, read: bool| {
                    if !read {
                        return Ok([0; 4]);
                    }
                    crate::renderer::input_map(out_region, [256, 256], f32_region(t))
                        .ok_or_else(|| format!("{}: v3 refuses the region", program.name))
                };
                let [read_a, read_b] = stage.reads();
                let curve = crate::renderer::mask_grid(
                    stage.settings,
                    map(stage.a, read_a)?,
                    map(stage.b, read_b)?,
                    sampler.as_ref(),
                );
                let uniform = upload_buffer(
                    device,
                    &self.queue,
                    &[EffectConfig {
                        info: [
                            stage.mode,
                            0,
                            u32::from(sampler.is_some()),
                            crate::sample::SAMPLE_INTEGER,
                        ],
                        curve,
                        sizes: [256; 4],
                        output_clip: [256; 4],
                        ..EffectConfig::zeroed()
                    }],
                    wgpu::BufferUsages::UNIFORM,
                );
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("q14 stage"),
                    layout: &effects.get_bind_group_layout(0),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&views[stage.a]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&views[stage.b]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(&atlas_v),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(&views[stage.out]),
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
                });
                {
                    let mut pass = encoder.begin_compute_pass(&Default::default());
                    pass.set_pipeline(&effects);
                    pass.set_bind_group(0, &bind, &[]);
                    pass.dispatch_workgroups(16, 16, 1);
                }
                keep.push((tiles, uniform));
            }
            let resolve_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("q14 resolve"),
                layout: &resolve.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&views[program.result]),
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
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&resolve);
                pass.set_bind_group(0, &resolve_bind, &[]);
                pass.dispatch_workgroups(16, 16, 1);
            }
            let read = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("q14 readback"),
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
                size(1),
            );
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            self.queue.submit(Some(encoder.finish()));
            map_readback(device, &read)?;
            if let Some(err) = pollster::block_on(scope.pop()) {
                return Err(format!("{}: validation: {err}", program.name));
            }
            let bytes = read
                .slice(..)
                .get_mapped_range()
                .map_err(|e| e.to_string())?
                .to_vec();
            read.unmap();
            drop(keep);
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
fn q14_round_f64(v: f32) -> i32 {
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
fn q16_tie_values() -> Vec<f64> {
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
    let core = include_str!("../../alto-core/src/fixed_affine.rs");
    let sample = include_str!("sample.rs");
    let renderer = include_str!("renderer.rs");
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
fn renderer_case_json(case: &RendererCase) -> String {
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
    // Invert has no integer stage; Gaussian blur does (checked below).
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
        // A pixel mask, a color-adjust adjustment layer, a filled vector and its
        // vector mask or clip path are the ones of these with integer stages.
        let integer = matches!(
            reason,
            "adjustment" | "mask" | "vector" | "vector mask" | "clip path"
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

    // Color-adjust and Gaussian blur layer effects are integer M5, selection-limited
    // ones too (mode 22). Any other effect has no integer stage yet, with or
    // without a selection.
    // Blends of masked adjustment layers are mode 23. Without a mask a non-normal
    // blend is refused until the stages upstream of mode 23 are integer.
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
    // A stroked path keeps the v2 tessellator, so it is refused.
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
    assert_eq!(crate::renderer::v3_gap(&stroked), Some("vector stroke"));
    // A stroke that cannot change a pixel does not refuse the node: width
    // +/-0, or an alpha below 2^-15 (its Q14 is 0). 2^-15 itself is visible.
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
        let want = visible.then_some("vector stroke");
        assert_eq!(crate::renderer::v3_gap(&s), want, "{width} {alpha:e}");
    }
    let mut multiply = adjustment.clone();
    multiply.render_version = 3;
    multiply.nodes[0].blend = RenderBlend::Multiply;
    assert_eq!(crate::renderer::v3_gap(&multiply), Some("adjustment"));
    assert_eq!(gap(&multiply), Some("v3-unsupported"));
    multiply.nodes[0].features.mask = mask.nodes[0].features.mask.clone();
    assert_eq!(crate::renderer::v3_gap(&multiply), None);
    assert_eq!(gap(&multiply), Some("v3-m5-integer"));
    let mut invert = multiply.clone();
    invert.nodes[0].content = RContent::Adjustment(REffect {
        kind: REffectKind::Invert,
        selection: None,
    });
    assert_eq!(crate::renderer::v3_gap(&invert), Some("adjustment"));
    // Integer vector coverage composes with pixel masks; unsupported stages
    // remain refused when a pixel mask is present.
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
        let expected = if matches!(reason, "vector mask" | "clip path") {
            None
        } else {
            Some(reason)
        };
        assert_eq!(crate::renderer::v3_gap(&v3), expected, "mask + {reason}");
    }
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
    let q14 = include_str!("../shaders/q14.wgsl");
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

static GPU_RENDERER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn gpu_shader_matches_cpu_reference() {
    let _gpu_lock = GPU_RENDERER_TEST_LOCK.lock().unwrap();
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
    let mut measured_v3 = Vec::new();
    let mut v3_errors = Vec::new();
    // Version 3 composes only level 0 directly (higher levels are the integer
    // pyramid, checked below). A draw that reaches none of its tiles is skipped,
    // so large-translation-out-of-range is transparent and equals the CPU.
    for mut case in cases().into_iter().filter(|c| c.measure) {
        case.name = format!("v3-{}", case.name);
        if case.level > 0 {
            let cpu = render_pyramid(&case);
            match gpu.render_pyramid(&case) {
                Ok(bytes) => {
                    let row = measure_case(&case, &cpu, &bytes, &[], &[]);
                    println!("v3 composite (enforced): {}", measure_line(&row));
                    if cpu != bytes {
                        failed.push(format!("{}: {}", case.name, diff_report(&cpu, &bytes)));
                    }
                    measured_v3.push(row);
                }
                Err(err) => {
                    v3_errors.push(format!("{}: {err}", case.name));
                    failed.push(format!("{}: {err}", case.name));
                }
            }
            continue;
        }
        let (cpu, cpu_pre) = render_pair(&case, 3);
        match gpu.render_version(&case, 3) {
            Ok((bytes, pre)) => {
                let row = measure_case(&case, &cpu, &bytes, &cpu_pre, &pre);
                println!("v3 composite (enforced): {}", measure_line(&row));
                if cpu != bytes {
                    failed.push(format!("{}: {}", case.name, diff_report(&cpu, &bytes)));
                }
                measured_v3.push(row);
            }
            Err(err) => {
                v3_errors.push(format!("{}: {err}", case.name));
                failed.push(format!("{}: {err}", case.name));
            }
        }
    }
    for mut case in cases().into_iter().filter(|c| c.measure) {
        case.level = 1;
        case.tx = 0;
        case.ty = 0;
        case.name = format!("v3-{}-level1", case.name);
        let cpu = render_pyramid(&case);
        match gpu.render_pyramid(&case) {
            Ok(bytes) => {
                let row = measure_case(&case, &cpu, &bytes, &[], &[]);
                if cpu != bytes {
                    failed.push(format!("{}: {}", case.name, diff_report(&cpu, &bytes)));
                }
                measured_v3.push(row);
            }
            Err(err) => {
                v3_errors.push(format!("{}: {err}", case.name));
                failed.push(format!("{}: {err}", case.name));
            }
        }
    }
    write_v3_measure(&gpu.adapter_name, &measured_v3, &v3_errors);

    // Enforced: every byte on every channel must round-trip, in the Rgba8Unorm
    // raster read and the R8Unorm glyph atlas read. Stored first, then failed.
    let atlas = gpu.atlas_read(false);
    let atlas_r8 = gpu.atlas_read(true);
    for (format, result) in [("Rgba8Unorm", &atlas), ("R8Unorm", &atlas_r8)] {
        match result {
            Ok(r) => {
                println!("{}", r.line());
                if !r.mismatches.is_empty() {
                    failed.push(r.line());
                }
            }
            Err(e) => {
                println!("atlas-read {format} (enforced): GPU error: {e}");
                failed.push(format!("atlas-read {format}: {e}"));
            }
        }
    }
    write_atlas_read(&gpu.adapter_name, &atlas, &atlas_r8);
    // Enforced: the v3 M5 color-adjust chain and opacity 0.5, integer-exact.
    let src = color_adjust_gradient();
    let adjust: Vec<_> = color_adjust_grid()
        .into_iter()
        .map(color_adjust_program)
        .collect();
    let images = opacity_images();
    let opacity = opacity_programs();
    // Enforced: the v3 pixel-mask stages (modes 21, 22 and 23).
    let mask_atlas = mask_images();
    let masks = mask_programs();
    // Enforced: the same stages at non-zero and negative ROI origins, and with
    // inputs at other ROIs (the integer texel map).
    let origins = mask_origin_programs();
    // Enforced: the float purge, modes 18, 17, 19, 8 and 20 placed by integers
    // at far ROIs, with transformed rasters.
    let purge = purge_programs();
    for (images, programs) in [
        (std::slice::from_ref(&src), &adjust),
        (&images[..], &opacity),
        (&mask_atlas[..], &masks),
        (&mask_atlas[..], &origins),
        (&mask_atlas[..], &purge),
    ] {
        match gpu.render_q14(images, programs) {
            Ok(tiles) => {
                if tiles.len() != programs.len() {
                    failed.push(format!(
                        "q14 stages: {} GPU tiles for {} programs",
                        tiles.len(),
                        programs.len()
                    ));
                }
                for (tile, program) in tiles.iter().zip(programs.iter()) {
                    let cpu = q14_program_cpu(images, program);
                    let report = diff_report(&cpu, tile);
                    println!("{} (v3 M5 integer, enforced): {report}", program.name);
                    if cpu != *tile {
                        failed.push(format!("{}: {report}", program.name));
                    }
                }
            }
            Err(err) => {
                println!("q14 stages: GPU render failed: {err}");
                failed.push(format!("q14 stages: {err}"));
            }
        }
    }
    // Enforced: v3 scenes through the renderer, so every uniform, the raster
    // and mask tiles `Context::lookup_entries` selects and uploads, and the
    // resolve are the host's own. Refused cases must fail with GpuUnsupported.
    let cases = renderer_cases();
    match renderer_gpu(&cases) {
        Ok(tiles) => {
            for (tile, case) in tiles.iter().zip(&cases) {
                match (tile, case.refused) {
                    (Ok(tile), None) => {
                        let cpu = renderer_case_cpu(case);
                        let report = diff_report(&cpu, tile);
                        println!("{} (v3 renderer, enforced): {report}", case.name);
                        if cpu != *tile {
                            failed.push(format!("{}: {report}", case.name));
                        }
                    }
                    (Err(crate::RenderError::GpuUnsupported(m)), Some(want)) if m == want => {
                        println!("{} (v3 renderer, enforced): refused: {m}", case.name);
                    }
                    (result, want) => {
                        let got = result.as_ref().map(|_| "a tile").map_err(|e| e.to_string());
                        println!("{}: expected {want:?}, got {got:?}", case.name);
                        failed.push(format!("{}: expected {want:?}, got {got:?}", case.name));
                    }
                }
            }
        }
        Err(err) => {
            println!("renderer cases: GPU render failed: {err}");
            failed.push(format!("renderer cases: {err}"));
        }
    }
    // Enforced (Linus 04:40): the same scene read warm after `scene_damage` and
    // cold gives the same bytes or the same refusal, and drawn tiles equal the CPU.
    let cold_warm = cold_warm_cases();
    match renderer_cold_warm(&cold_warm) {
        Ok(results) => {
            for (case, per_key) in cold_warm.iter().zip(results) {
                for (((level, x, y), want), (warm, cold)) in case.keys.iter().zip(per_key) {
                    let name = format!("cold/warm {} ({level}, {x}, {y})", case.name);
                    let refusal = |r: &Result<Vec<u8>, crate::RenderError>| match r {
                        Err(crate::RenderError::GpuUnsupported(m)) => Some(m.clone()),
                        _ => None,
                    };
                    match (&warm, &cold) {
                        (Ok(w), Ok(c)) if want.is_none() => {
                            let cpu = renderer_case_cpu(&RendererCase {
                                level: *level,
                                key: [*x, *y],
                                ..RendererCase {
                                    name: name.clone(),
                                    ..case.after.clone_layers()
                                }
                            });
                            if w != c || *c != cpu {
                                failed.push(format!(
                                    "{name}: warm vs cold {}; cold vs CPU {}",
                                    diff_report(c, w),
                                    diff_report(&cpu, c)
                                ));
                            } else {
                                println!("{name} (enforced): warm = cold = CPU");
                            }
                        }
                        _ if want.is_some()
                            && refusal(&warm).as_deref() == *want
                            && refusal(&cold).as_deref() == *want =>
                        {
                            println!(
                                "{name} (enforced): refused warm and cold: {}",
                                want.unwrap()
                            );
                        }
                        _ => failed.push(format!(
                            "{name}: expected {want:?}, warm {:?}, cold {:?}",
                            warm.as_ref().map(|_| "tile").map_err(|e| e.to_string()),
                            cold.as_ref().map(|_| "tile").map_err(|e| e.to_string())
                        )),
                    }
                }
            }
        }
        Err(err) => failed.push(format!("cold/warm: {err}")),
    }
    assert!(
        failed.is_empty(),
        "GPU and CPU pixels differ:\n{}",
        failed.join("\n")
    );
}

fn v3_vector_test_render_at(document: &RenderScene, key: (i32, i32, i32)) -> Option<Vec<u8>> {
    v3_test_render_with(document, &alto_raster::ObjectStore::default(), key)
}
fn v3_test_render_with(
    document: &RenderScene,
    store: &alto_raster::ObjectStore,
    key: (i32, i32, i32),
) -> Option<Vec<u8>> {
    v3_test_try_render(document, store, key).map(|r| r.expect("v3 vector tile"))
}
fn v3_test_try_render(
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
fn v3_vector_test_render(document: &RenderScene) -> Option<Vec<u8>> {
    v3_vector_test_render_at(document, (0, 0, 0))
}
fn blur_pass_reference(
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

fn test_rectangle(x0: f64, y0: f64, x1: f64, y1: f64, fill: [f32; 4]) -> alto_vector::StyledPath {
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
fn v3_square_under(top: RNode) -> (RenderScene, alto_raster::ObjectStore) {
    use alto_core::{PixelFormat, TileKey};
    let mut bytes = vec![0u8; 256 * 256 * 4];
    for y in 80..180 {
        for x in 80..180 {
            bytes[(y * 256 + x) * 4..][..4].copy_from_slice(&[255, 0, 0, 255]);
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

fn v3_adjustment(kind: REffectKind, features: RFeatures) -> RNode {
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
fn v3_adjustment_vector_mask_and_clip_path_are_refused() {
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
        // No integer mix of backdrop, adjusted copy and coverage is defined.
        assert_eq!(crate::renderer::v3_gap(&document), Some("adjustment"));
        assert_eq!(gap(&document), Some("v3-unsupported"));
        let Some(result) = v3_test_try_render(&document, &store, (0, 0, 0)) else {
            return;
        };
        assert!(
            matches!(result, Err(crate::RenderError::GpuUnsupported(_))),
            "{result:?}"
        );
    }
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

#[test]
fn v3_hosts_refuse_blend_modes_17_and_up() {
    // RenderBlend has variants 0..=16 only, so a v3 RenderScene cannot carry 17.
    assert_eq!(RenderBlend::Add as u32, 16);
    for mode in [0u32, 16] {
        let mut ops = Vec::new();
        walk(
            &raster(solid(1, 1, [0; 4]), mode, 1.0, true, ident()),
            ident(),
            &mut ops,
        );
        assert!(!has_unspecified_v3_mode(&ops), "mode {mode}");
    }
    for mode in [17u32, 18, 255] {
        let mut ops = Vec::new();
        walk(
            &raster(solid(1, 1, [0; 4]), mode, 1.0, true, ident()),
            ident(),
            &mut ops,
        );
        assert!(has_unspecified_v3_mode(&ops), "draw mode {mode}");
        ops.clear();
        let mut g = group(true, 0.5, Vec::new());
        if let Node::Group { mode: m, .. } = &mut g {
            *m = mode;
        }
        walk(&g, ident(), &mut ops);
        assert!(has_unspecified_v3_mode(&ops), "pop mode {mode}");
    }
}

#[test]
fn m5_opacity_half_over_blue_is_the_known_pixel() {
    let images = opacity_images();
    let programs = opacity_programs();
    let half = programs
        .iter()
        .find(|p| p.name == "m5-opacity-half")
        .unwrap();
    let tile = q14_program_cpu(&images, half);
    // sa = 8192, pre = 8192 red; blue keeps 16384 * (1 - 0.5). 8192 * 255 / 16384
    // is the tie 127.5, which goes up.
    assert_eq!(pixel(&tile, 7, 9), [128, 0, 128, 255]);
    assert!(tile.chunks(4).all(|p| p == &tile[..4]));
    // The Q14 program reproduces the color-adjust reference used by the browser.
    let src = color_adjust_gradient();
    for settings in color_adjust_grid() {
        let program = color_adjust_program(settings);
        assert_eq!(
            q14_program_cpu(std::slice::from_ref(&src), &program),
            color_adjust_image_v3(&src, settings.2, settings.0, settings.1),
            "{}",
            program.name
        );
    }
    // Every half-opacity program differs from the same graph at opacity 1.
    for program in &programs {
        let mut opaque = Q14Program {
            name: program.name.clone(),
            targets: program.targets,
            regions: program.regions.clone(),
            stages: program.stages.clone(),
            result: program.result,
        };
        for stage in &mut opaque.stages {
            if matches!(stage.mode, 19 | 20) {
                stage.settings[0] = q14::Q as i32;
            }
        }
        assert_ne!(
            q14_program_cpu(&images, program),
            q14_program_cpu(&images, &opaque),
            "{}",
            program.name
        );
    }
}

#[test]
fn integer_effects_shader_has_no_float_targets_and_validates() {
    let shader = crate::integer_effects_shader();
    assert!(shader.contains("var output: texture_storage_2d<rgba16uint,write>"));
    assert!(shader.contains("var a: texture_2d<u32>"));
    let resolve = include_str!("../shaders/effects-resolve-q14.wgsl");
    assert!(resolve.contains("texture_storage_2d_array<rgba8uint,write>"));
    for text in [shader.as_str(), resolve] {
        for forbidden in [
            "rgba16float",
            "rgba8unorm",
            "textureSample",
            "f16",
            "round(",
            "=sqrt(",
            "= sqrt(",
        ] {
            assert!(!text.contains(forbidden), "v3 M5 shader uses {forbidden}");
        }
    }
    let v2 = include_str!("../shaders/effects.wgsl");
    for gone in ["f16", "Q14", "info.x==17u", "info.x==18u", "info.x==19u"] {
        assert!(!v2.contains(gone), "v1/v2 effects.wgsl still has {gone}");
    }
    // The host binds 0, 1 and 3..=6 only; an unused declared binding would leave
    // the derived layout without it and fail the bind group.
    assert!(!shader.contains("@binding(2)"));
    for text in [shader, resolve.to_string()] {
        let module = naga::front::wgsl::parse_str(&text)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&text)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
    }
}

#[test]
fn composite_v3_rust_and_js_references_agree_byte_for_byte() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let js = std::process::Command::new("node")
        .arg(root.join("tests/e2e/composite-v3-reference.mjs"))
        .output()
        .expect("node is required to compare the Rust and JavaScript v3 references");
    assert!(
        js.status.success(),
        "JS reference failed: {}",
        String::from_utf8_lossy(&js.stderr)
    );
    let mut data = js.stdout.as_slice();
    let mut count = 0;
    for case in cases().into_iter().filter(|c| c.measure) {
        let end = data.iter().position(|b| *b == b'\n').expect("JS case name");
        assert_eq!(std::str::from_utf8(&data[..end]).unwrap(), case.name);
        data = &data[end + 1..];
        let (bytes, rest) = data.split_at(256 * 256 * 4);
        let rust = render_at(&case, 3);
        assert!(
            rust == bytes,
            "{}: {}",
            case.name,
            diff_report(&rust, bytes)
        );
        data = rest;
        count += 1;
    }
    for mut case in cases().into_iter().filter(|c| c.measure) {
        case.level = 1;
        case.tx = 0;
        case.ty = 0;
        let end = data
            .iter()
            .position(|b| *b == b'\n')
            .expect("JS pyramid case name");
        assert_eq!(
            std::str::from_utf8(&data[..end]).unwrap(),
            format!("{}-level1", case.name)
        );
        data = &data[end + 1..];
        let (bytes, rest) = data.split_at(256 * 256 * 4);
        let rust = render_pyramid(&case);
        assert!(
            rust == bytes,
            "{} level1: {}",
            case.name,
            diff_report(&rust, bytes)
        );
        data = rest;
    }
    assert!(data.is_empty(), "JS produced extra cases");
    assert!(count >= 37);
}

#[test]
fn integer_composite_shader_has_only_integer_color_output() {
    let shader = crate::integer_composite_shader();
    assert!(shader.contains("var<storage, read_write> output: array<u32>"));
    assert!(shader.contains("opacity_q: u32"));
    assert!(shader.contains("unorm_byte(s.r)"));
    for forbidden in [
        "textureStore",
        "textureSample",
        "rgba8unorm",
        "rgba16float",
        "round(",
        "=sqrt(",
        "= sqrt(",
    ] {
        assert!(
            !shader.contains(forbidden),
            "unexpected v3 shader operation: {forbidden}"
        );
    }
}

#[test]
fn v3_isolation_uses_document_opacity_before_q14_quantization() {
    let almost_one = f32::from_bits(1.0f32.to_bits() - 1);
    assert_eq!(crate::composite_opacity_q14(almost_one), 16384);
    let mut ops = Vec::new();
    walk(&group(false, almost_one, Vec::new()), ident(), &mut ops);
    assert!(matches!(ops.as_slice(), [Op::Push, Op::Pop { .. }]));
    ops.clear();
    walk(&group(false, 1.0, Vec::new()), ident(), &mut ops);
    assert!(ops.is_empty());
}

#[test]
fn integer_parity_shader_records_the_q14_accumulator_and_validates() {
    let shader = integer_parity_shader();
    assert!(shader.contains("pre_quant[id.y*256u+id.x]=vec4f(accum)/16384.0;"));
    let module = naga::front::wgsl::parse_str(&shader).unwrap();
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap();
}
