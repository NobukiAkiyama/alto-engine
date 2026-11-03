//! 1:1 CPU reads of rasters that need no GPU effects.
use super::*;
use alto_core::{MAX_TREE_DEPTH, NodeId, PixelFormat, TileKey};
use alto_doc::{Document, NodeKind};
use alto_raster::TileMap;

fn bad(message: &str) -> EngineError {
    DocError::Invalid(message.into()).into()
}

fn translation(m: [f64; 6]) -> Result<(i32, i32), EngineError> {
    if (m[0] - 1.0).abs() > 1e-6
        || m[1].abs() > 1e-6
        || m[2].abs() > 1e-6
        || (m[3] - 1.0).abs() > 1e-6
        || (m[4] - m[4].round()).abs() > 1e-6
        || (m[5] - m[5].round()).abs() > 1e-6
    {
        return Err(bad("Geometry requires whole-pixel translation"));
    }
    Ok((m[4].round() as i32, m[5].round() as i32))
}

fn chain(doc: &Document, id: NodeId, include_self: bool) -> Result<(i32, i32), EngineError> {
    let mut x = 0;
    let mut y = 0;
    let mut next = if include_self {
        Some(id)
    } else {
        doc.node(id)?.parent
    };
    for _ in 0..MAX_TREE_DEPTH {
        let Some(id) = next else {
            return Ok((x, y));
        };
        let node = doc.node(id)?;
        let (dx, dy) = translation(node.props.transform.0)?;
        x += dx;
        y += dy;
        next = node.parent;
    }
    Err(bad(&format!("Group nesting exceeds {MAX_TREE_DEPTH}")))
}

fn read_pixel(engine: &Engine, map: &TileMap, x: i32, y: i32) -> Result<[u8; 4], EngineError> {
    let key = TileKey {
        tx: x.div_euclid(256),
        ty: y.div_euclid(256),
    };
    let Some(hash) = map.tiles.get(&key) else {
        return Ok([0, 0, 0, 0]);
    };
    let tile = engine.require_tile(hash)?;
    let bpp = map.format.bytes_per_pixel();
    let i = (y.rem_euclid(256) as usize * 256 + x.rem_euclid(256) as usize) * bpp;
    let mut out = [0, 0, 0, 0];
    match map.format {
        PixelFormat::Rgba8 => out.copy_from_slice(&tile.pixels[i..i + 4]),
        PixelFormat::R8 => out = [0, 0, 0, tile.pixels[i]],
    }
    Ok(out)
}

fn mix_mode(mode: u32, s: [f32; 3], d: [f32; 3]) -> [f32; 3] {
    let each = |f: fn(f32, f32) -> f32| [f(s[0], d[0]), f(s[1], d[1]), f(s[2], d[2])];
    let dodge = |s: f32, d: f32| {
        if s >= 1.0 {
            1.0
        } else {
            (d / (1.0 - s).max(1e-6)).min(1.0)
        }
    };
    let burn = |s: f32, d: f32| {
        if s <= 0.0 {
            0.0
        } else {
            1.0 - ((1.0 - d) / s.max(1e-6)).min(1.0)
        }
    };
    let soft = |s: f32, d: f32| {
        let d2 = if d <= 0.25 {
            ((16.0 * d - 12.0) * d + 4.0) * d
        } else {
            d.sqrt()
        };
        if s <= 0.5 {
            d - (1.0 - 2.0 * s) * d * (1.0 - d)
        } else {
            d + (2.0 * s - 1.0) * (d2 - d)
        }
    };
    let lum = |c: [f32; 3]| c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722;
    let clip = |c: [f32; 3]| {
        let l = lum(c);
        let n = c[0].min(c[1]).min(c[2]);
        let x = c[0].max(c[1]).max(c[2]);
        let mut o = c;
        if n < 0.0 {
            o = [0, 1, 2].map(|i| l + (c[i] - l) * l / (l - n).max(1e-6));
        }
        if x > 1.0 {
            o = [0, 1, 2].map(|i| l + (o[i] - l) * (1.0 - l) / (x - l).max(1e-6));
        }
        o.map(|v| v.clamp(0.0, 1.0))
    };
    let set_lum =
        |c: [f32; 3], l: f32| clip([c[0] + l - lum(c), c[1] + l - lum(c), c[2] + l - lum(c)]);
    let set_sat = |c: [f32; 3], sat: f32| {
        let n = c[0].min(c[1]).min(c[2]);
        let x = c[0].max(c[1]).max(c[2]);
        if x <= n {
            [0.0; 3]
        } else {
            c.map(|v| (v - n) * sat / (x - n).max(1e-6))
        }
    };
    match mode {
        1 => [s[0] * d[0], s[1] * d[1], s[2] * d[2]],
        2 => each(|s, d| 1.0 - (1.0 - d) * (1.0 - s)),
        3 => each(|s, d| {
            if d < 0.5 {
                2.0 * s * d
            } else {
                1.0 - 2.0 * (1.0 - s) * (1.0 - d)
            }
        }),
        4 => each(|s, d| s.min(d)),
        5 => each(|s, d| s.max(d)),
        6 => each(dodge),
        7 => each(burn),
        8 => each(|s, d| {
            if s < 0.5 {
                2.0 * d * s
            } else {
                1.0 - 2.0 * (1.0 - d) * (1.0 - s)
            }
        }),
        9 => each(soft),
        10 => each(|s, d| (d - s).abs()),
        11 => each(|s, d| d + s - 2.0 * d * s),
        12 => set_lum(
            set_sat(s, d[0].max(d[1]).max(d[2]) - d[0].min(d[1]).min(d[2])),
            lum(d),
        ),
        13 => set_lum(
            set_sat(d, s[0].max(s[1]).max(s[2]) - s[0].min(s[1]).min(s[2])),
            lum(d),
        ),
        14 => set_lum(s, lum(d)),
        15 => set_lum(d, lum(s)),
        16 => each(|s, d| (s + d).min(1.0)),
        _ => s,
    }
}

fn composite_pixel(dst: [f32; 4], src: [u8; 4], opacity: f32, mode: u32) -> [f32; 4] {
    let sa = src[3] as f32 / 255.0 * opacity;
    let s = [
        src[0] as f32 / 255.0,
        src[1] as f32 / 255.0,
        src[2] as f32 / 255.0,
    ];
    let da = dst[3];
    let d = if da > 0.0 {
        [dst[0] / da, dst[1] / da, dst[2] / da]
    } else {
        [0.0; 3]
    };
    let b = mix_mode(mode, s, d);
    let a = sa + da * (1.0 - sa);
    [
        (1.0 - sa) * dst[0] + (1.0 - da) * s[0] * sa + da * sa * b[0],
        (1.0 - sa) * dst[1] + (1.0 - da) * s[1] * sa + da * sa * b[1],
        (1.0 - sa) * dst[2] + (1.0 - da) * s[2] * sa + da * sa * b[2],
        a,
    ]
}

fn straight(px: [f32; 4]) -> [u8; 4] {
    if px[3] <= 0.0 {
        return [0, 0, 0, 0];
    }
    let c = |i: usize| ((px[i] / px[3]) * 255.0).round().clamp(0.0, 255.0) as u8;
    [
        c(0),
        c(1),
        c(2),
        (px[3] * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

struct Draw {
    map: TileMap,
    offset: (i32, i32),
    opacity: f32,
    mode: u32,
}

fn draws(doc: &Document, id: NodeId) -> Result<Vec<Draw>, EngineError> {
    let mut out = Vec::new();
    fn walk(doc: &Document, id: NodeId, out: &mut Vec<Draw>) -> Result<(), EngineError> {
        let node = doc.node(id)?;
        if !node.props.visible {
            return Ok(());
        }
        if !node.props.effects.is_empty() || node.props.mask.is_some() && node.props.mask_enabled {
            return Err(bad("Export bakes rasters without effects or masks"));
        }
        match &node.kind {
            NodeKind::Group {
                children,
                isolation,
            } => {
                if *isolation != alto_doc::Isolation::PassThrough {
                    return Err(bad("Export supports pass-through groups"));
                }
                for child in children {
                    walk(doc, *child, out)?;
                }
            }
            NodeKind::Raster { tiles, .. } => out.push(Draw {
                map: tiles.clone(),
                offset: chain(doc, id, true)?,
                opacity: node.props.opacity,
                mode: node.props.blend as u32,
            }),
            NodeKind::Vector { .. } | NodeKind::Text { .. } | NodeKind::Adjustment { .. } => {
                return Err(bad("Export supports raster layers"));
            }
            NodeKind::Unsupported { .. } => return Err(bad("Export supports raster layers")),
        }
        Ok(())
    }
    walk(doc, id, &mut out)?;
    Ok(out)
}

fn pixel_at(engine: &Engine, draws: &[Draw], x: i32, y: i32) -> Result<[u8; 4], EngineError> {
    let mut acc = [0.0; 4];
    for draw in draws {
        let src = read_pixel(engine, &draw.map, x - draw.offset.0, y - draw.offset.1)?;
        acc = composite_pixel(acc, src, draw.opacity, draw.mode);
    }
    Ok(straight(acc))
}

impl Engine {
    pub fn sample_pixel(&self, handle: DocHandle, x: i32, y: i32) -> Result<[u8; 4], EngineError> {
        let doc = self.document(handle)?;
        if x < 0 || y < 0 || x >= doc.meta.width as i32 || y >= doc.meta.height as i32 {
            return Ok([0, 0, 0, 0]);
        }
        pixel_at(self, &draws(doc, doc.root)?, x, y)
    }

    pub fn export_rgba(&self, handle: DocHandle) -> Result<Vec<u8>, EngineError> {
        let doc = self.document(handle)?;
        let width = doc.meta.width;
        let height = doc.meta.height;
        if width as u64 * height as u64 > 64_000_000 {
            return Err(bad("Export exceeds 64M pixels"));
        }
        let draws = draws(doc, doc.root)?;
        let mut out = vec![0u8; width as usize * height as usize * 4];
        for y in 0..height as i32 {
            for x in 0..width as i32 {
                let px = pixel_at(self, &draws, x, y)?;
                let i = (y as usize * width as usize + x as usize) * 4;
                out[i..i + 4].copy_from_slice(&px);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alto_doc::{BlendMode, Node, Op, PropPatch};
    use alto_scene::RenderBlend;

    fn layer(engine: &mut Engine, handle: DocHandle) -> NodeId {
        let node = Node::raster("paint");
        let id = node.id;
        let root = engine.document(handle).unwrap().root;
        engine
            .transact(
                handle,
                "layer",
                vec![Op::InsertNode {
                    node,
                    parent: root,
                    index: 0,
                }],
                engine.document(handle).unwrap().revision,
            )
            .unwrap();
        id
    }

    #[test]
    fn overlay_reaches_the_renderer_and_levels_bump_the_format() {
        let mut engine = Engine::new();
        let handle = engine
            .create_document(DocumentMeta::srgb_u8(32, 32))
            .unwrap();
        let id = layer(&mut engine, handle);
        let mut node = engine.document(handle).unwrap().node(id).unwrap().clone();
        node.props.blend = BlendMode::Overlay;
        node.props.transform = alto_core::Affine2([0.1, 0.0, 0.0, 1.0, 0.3, 4.5]);
        let revision = engine.document(handle).unwrap().revision;
        engine
            .transact(
                handle,
                "blend",
                vec![Op::ReplaceNode { id, node }],
                revision,
            )
            .unwrap();
        let scene = engine.render_scene(handle).unwrap();
        assert_eq!(scene.nodes[1].blend, RenderBlend::Overlay);
        assert_eq!(scene.nodes[1].blend as u32, 3);
        assert_eq!(scene.nodes[1].transform[0], 0.1);
        assert_eq!(scene.nodes[1].transform[4], 0.3);
        engine.undo(handle).unwrap();
        assert_eq!(
            engine.render_scene(handle).unwrap().nodes[1].blend,
            RenderBlend::Normal
        );
        let revision = engine.document(handle).unwrap().revision;
        engine
            .transact(
                handle,
                "levels",
                vec![Op::SetProps {
                    id,
                    patch: PropPatch {
                        effects: Some(vec![alto_doc::Effect::Levels {
                            in_black: 0.0,
                            in_white: 1.0,
                            gamma: 1.0,
                            out_black: 0.0,
                            out_white: 1.0,
                            selection: None,
                            extra: Default::default(),
                        }]),
                        ..PropPatch::default()
                    },
                }],
                revision,
            )
            .unwrap();
        let snapshot = engine.snapshot(handle).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
        assert_eq!(value["meta"]["format_version"], 5);
        assert_eq!(value["meta"]["render_version"], 3);
        let reopened = engine.open_snapshot(&snapshot).unwrap();
        assert_eq!(engine.document(reopened).unwrap().meta.format_version, 5);
        engine.undo(handle).unwrap();
        let snapshot = engine.snapshot(handle).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
        assert_eq!(value["meta"]["format_version"], 5);
        assert_eq!(value["meta"]["render_version"], 3);
        engine.open_snapshot(&snapshot).unwrap();
    }
}
