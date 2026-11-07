//! The composite parity cases and the images they draw.
use super::*;

pub(super) fn cases() -> Vec<Case> {
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
pub(super) fn alpha_sweep(w: u32, h: u32, layer: u32) -> Image {
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

pub(super) fn paint_alpha_sweep(nodes: &mut [Node]) {
    let mut layer = 0u32;
    paint_alpha_sweep_at(nodes, &mut layer);
}

pub(super) fn paint_alpha_sweep_at(nodes: &mut [Node], layer: &mut u32) {
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

pub(super) fn split_tiles(image: &Image) -> Vec<Tile> {
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
