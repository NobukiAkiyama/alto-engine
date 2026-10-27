//! Canvas geometry, fills, and 1:1 reads. New hashes are stored before the transaction.
use super::*;
use alto_core::{
    Affine2, MAX_TREE_DEPTH, NodeId, PixelFormat, RENDER_VERSION, RENDER_VERSION_EXACT,
    RENDER_VERSION_F32, TileKey,
};
use alto_doc::{Document, Node, NodeKind, Op, PropPatch, TileChange};
use alto_raster::{TileData, TileMap};
use alto_text::TextFrame;
use alto_vector::{PathCommand, PathData, StyledPath};
use std::collections::HashMap;

fn bad(message: &str) -> EngineError {
    DocError::Invalid(message.into()).into()
}

fn identity(m: [f64; 6]) -> bool {
    (m[0] - 1.0).abs() < 1e-6
        && m[1].abs() < 1e-6
        && m[2].abs() < 1e-6
        && (m[3] - 1.0).abs() < 1e-6
        && m[4].abs() < 1e-6
        && m[5].abs() < 1e-6
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

fn plain(node: &Node) -> Result<(), EngineError> {
    if !identity(node.props.mask_transform.0) {
        return Err(bad("Geometry requires an identity mask transform"));
    }
    if node
        .props
        .vector_mask
        .as_ref()
        .is_some_and(|mask| !identity(mask.transform.0))
        || node
            .props
            .clip_path
            .as_ref()
            .is_some_and(|path| !identity(path.transform.0))
    {
        return Err(bad("Geometry requires an identity vector mask"));
    }
    Ok(())
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

/// Destination pixels by document position. Writes walk rows, so consecutive pixels
/// almost always land in the tile the previous write used.
struct Scratch {
    bpp: usize,
    tiles: Vec<(TileKey, Vec<u8>)>,
    index: HashMap<TileKey, usize>,
    last: std::cell::Cell<Option<(TileKey, usize)>>,
}
impl Scratch {
    fn new(format: PixelFormat) -> Self {
        Self {
            bpp: format.bytes_per_pixel(),
            tiles: Vec::new(),
            index: HashMap::new(),
            last: std::cell::Cell::new(None),
        }
    }
    fn slot(&self, key: TileKey) -> Option<usize> {
        if let Some((k, i)) = self.last.get()
            && k == key
        {
            return Some(i);
        }
        let i = *self.index.get(&key)?;
        self.last.set(Some((key, i)));
        Some(i)
    }
    fn get(&self, x: i32, y: i32) -> [u8; 4] {
        let Some(slot) = self.slot(TileKey {
            tx: x.div_euclid(256),
            ty: y.div_euclid(256),
        }) else {
            return [0, 0, 0, 0];
        };
        let tile = &self.tiles[slot].1;
        let i = (y.rem_euclid(256) as usize * 256 + x.rem_euclid(256) as usize) * self.bpp;
        let mut out = [0, 0, 0, 255];
        out[..self.bpp].copy_from_slice(&tile[i..i + self.bpp]);
        if self.bpp == 1 {
            out = [0, 0, 0, tile[i]];
        }
        out
    }
    fn put(&mut self, x: i32, y: i32, pixel: &[u8]) {
        let key = TileKey {
            tx: x.div_euclid(256),
            ty: y.div_euclid(256),
        };
        let slot = match self.slot(key) {
            Some(slot) => slot,
            None => self.insert(key, vec![0; 256 * 256 * self.bpp]),
        };
        let i = (y.rem_euclid(256) as usize * 256 + x.rem_euclid(256) as usize) * self.bpp;
        self.tiles[slot].1[i..i + self.bpp].copy_from_slice(&pixel[..self.bpp]);
    }
    /// A whole tile written at once, replacing any earlier writes to it.
    fn insert(&mut self, key: TileKey, pixels: Vec<u8>) -> usize {
        if let Some(&slot) = self.index.get(&key) {
            self.tiles[slot].1 = pixels;
            return slot;
        }
        self.tiles.push((key, pixels));
        let slot = self.tiles.len() - 1;
        self.index.insert(key, slot);
        self.last.set(Some((key, slot)));
        slot
    }
    fn finish(self, engine: &mut Engine, format: PixelFormat) -> Result<TileMap, EngineError> {
        let mut map = TileMap::new(format);
        for (key, pixels) in self.tiles {
            let data = TileData::new(format, pixels).map_err(|_| bad("Tile size"))?;
            if let Some(hash) = engine.put_tile(data) {
                map.tiles.insert(key, hash);
            }
        }
        Ok(map)
    }
}

fn differ(layer: NodeId, old: &TileMap, new: &TileMap) -> Option<Op> {
    let mut keys: Vec<_> = old.tiles.keys().chain(new.tiles.keys()).copied().collect();
    keys.sort();
    keys.dedup();
    let changes: Vec<_> = keys
        .into_iter()
        .filter_map(|key| {
            let before = old.tiles.get(&key).copied();
            let after = new.tiles.get(&key).copied();
            (before != after).then_some(TileChange { key, before, after })
        })
        .collect();
    (!changes.is_empty()).then_some(Op::SetTiles { layer, changes })
}

fn map_commands(data: &PathData, f: &impl Fn(f64, f64) -> (f64, f64)) -> PathData {
    PathData {
        commands: data
            .commands
            .iter()
            .map(|command| match *command {
                PathCommand::Move { x, y } => {
                    let (x, y) = f(x, y);
                    PathCommand::Move { x, y }
                }
                PathCommand::Line { x, y } => {
                    let (x, y) = f(x, y);
                    PathCommand::Line { x, y }
                }
                PathCommand::Quad { cx, cy, x, y } => {
                    let (cx, cy) = f(cx, cy);
                    let (x, y) = f(x, y);
                    PathCommand::Quad { cx, cy, x, y }
                }
                PathCommand::Cubic {
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                } => {
                    let (c1x, c1y) = f(c1x, c1y);
                    let (c2x, c2y) = f(c2x, c2y);
                    let (x, y) = f(x, y);
                    PathCommand::Cubic {
                        c1x,
                        c1y,
                        c2x,
                        c2y,
                        x,
                        y,
                    }
                }
                PathCommand::Close => PathCommand::Close,
            })
            .collect(),
    }
}

fn remap_map<F>(engine: &mut Engine, map: &TileMap, mut place: F) -> Result<TileMap, EngineError>
where
    F: FnMut(i32, i32) -> Option<(i32, i32)>,
{
    if map.tiles.len() as u64 * 65_536 > 16_777_216 {
        return Err(bad("Geometry exceeds the pixel budget"));
    }
    let mut scratch = Scratch::new(map.format);
    for (key, hash) in &map.tiles {
        let tile = engine.require_tile(hash)?;
        let bpp = map.format.bytes_per_pixel();
        for i in 0..(256 * 256) {
            let pixel = &tile.pixels[i * bpp..(i + 1) * bpp];
            if pixel.iter().all(|v| *v == 0) {
                continue;
            }
            let x = key.tx * 256 + (i % 256) as i32;
            let y = key.ty * 256 + (i / 256) as i32;
            if let Some((nx, ny)) = place(x, y) {
                scratch.put(nx, ny, pixel);
            }
        }
    }
    scratch.finish(engine, map.format)
}

fn sample_channel(engine: &Engine, map: &TileMap, x: i32, y: i32, channel: usize) -> f32 {
    read_pixel(engine, map, x, y).unwrap_or([0, 0, 0, 0])[channel] as f32 / 255.0
}

fn catmull(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
    let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
    let c = -0.5 * p0 + 0.5 * p2;
    ((a * t + b) * t + c) * t + p1
}

fn resample(
    engine: &mut Engine,
    map: &TileMap,
    offset: (i32, i32),
    ancestor: (i32, i32),
    old: (u32, u32),
    new: (u32, u32),
) -> Result<TileMap, EngineError> {
    if new.0 as u64 * new.1 as u64 > 16_777_216 {
        return Err(bad("Resize exceeds the pixel budget"));
    }
    let sx = new.0 as f32 / old.0 as f32;
    let sy = new.1 as f32 / old.1 as f32;
    let mut scratch = Scratch::new(map.format);
    let bpp = map.format.bytes_per_pixel();
    for y in 0..new.1 as i32 {
        for x in 0..new.0 as i32 {
            let src_x = (x as f32 + 0.5) / sx - 0.5;
            let src_y = (y as f32 + 0.5) / sy - 0.5;
            let mut pixel = [0u8; 4];
            for (channel, sample) in pixel.iter_mut().take(bpp).enumerate() {
                let ix = src_x.floor() as i32;
                let iy = src_y.floor() as i32;
                let tx = src_x - ix as f32;
                let mut col = [0.0; 4];
                for row in 0..4 {
                    let yy = iy + row - 1;
                    let p = |dx: i32| {
                        sample_channel(engine, map, ix + dx - offset.0, yy - offset.1, channel)
                    };
                    col[row as usize] = catmull(p(-1), p(0), p(1), p(2), tx);
                }
                let ty = src_y - iy as f32;
                let value = catmull(col[0], col[1], col[2], col[3], ty).clamp(0.0, 1.0);
                *sample = (value * 255.0).round() as u8;
            }
            if pixel[..bpp].iter().all(|v| *v == 0) {
                continue;
            }
            scratch.put(x - ancestor.0, y - ancestor.1, &pixel);
        }
    }
    scratch.finish(engine, map.format)
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
            NodeKind::Raster { tiles } => out.push(Draw {
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

struct Geom<'a, F, P> {
    intent: &'a str,
    width: u32,
    height: u32,
    map_px: F,
    map_pt: P,
    text_scale: Option<(f64, f64)>,
    swap_frame: bool,
}

pub struct Gradient {
    pub from: (f32, f32),
    pub to: (f32, f32),
    pub start: [u8; 4],
    pub end: [u8; 4],
    pub radial: bool,
}

impl Engine {
    fn geometry<F, P>(&mut self, handle: DocHandle, geom: Geom<'_, F, P>) -> Result<(), EngineError>
    where
        F: Fn(i32, i32) -> Option<(i32, i32)>,
        P: Fn(f64, f64) -> (f64, f64),
    {
        let Geom {
            intent,
            width: new_w,
            height: new_h,
            map_px,
            map_pt,
            text_scale,
            swap_frame,
        } = geom;
        let doc = self.document(handle)?.clone();
        if new_w == 0 || new_h == 0 {
            return Err(bad("Canvas size must be positive"));
        }
        let mut ops = Vec::new();
        for node in doc.nodes.values() {
            plain(node)?;
            let offset = chain(&doc, node.id, true)?;
            let ancestor = chain(&doc, node.id, false)?;
            let local = |x: f64, y: f64| {
                let (nx, ny) = map_pt(x + offset.0 as f64, y + offset.1 as f64);
                (nx - ancestor.0 as f64, ny - ancestor.1 as f64)
            };
            match &node.kind {
                NodeKind::Raster { tiles } => {
                    let moved = remap_map(self, tiles, |x, y| {
                        map_px(x + offset.0, y + offset.1)
                            .map(|(nx, ny)| (nx - ancestor.0, ny - ancestor.1))
                    })?;
                    if let Some(op) = differ(node.id, tiles, &moved) {
                        ops.push(op);
                    }
                }
                NodeKind::Vector { paths } => {
                    let paths: Vec<StyledPath> = paths
                        .iter()
                        .map(|path| StyledPath {
                            data: map_commands(&path.data, &local),
                            ..path.clone()
                        })
                        .collect();
                    if paths != *paths_ref(node) {
                        ops.push(Op::SetVector { id: node.id, paths });
                    }
                }
                NodeKind::Text { content } => {
                    let mut content = content.clone();
                    if let Some((sx, sy)) = text_scale {
                        let scale = ((sx + sy) * 0.5) as f32;
                        content.style.size = (content.style.size * scale).clamp(0.01, 4096.0);
                        for span in &mut content.spans {
                            if let Some(size) = span.style.size {
                                span.style.size = Some((size * scale).clamp(0.01, 4096.0));
                            }
                        }
                    }
                    if swap_frame && let TextFrame::Area { width, height } = content.frame {
                        content.frame = TextFrame::Area {
                            width: height,
                            height: width,
                        };
                    }
                    if let (Some((sx, sy)), TextFrame::Area { width, height }) =
                        (text_scale, content.frame)
                    {
                        content.frame = TextFrame::Area {
                            width: width * sx,
                            height: height * sy,
                        };
                    }
                    if content.as_ref() != node_text(node) {
                        ops.push(Op::SetText {
                            id: node.id,
                            content,
                        });
                    }
                }
                _ => {}
            }
            if let Some(mask) = &node.props.mask {
                let moved = remap_map(self, mask, |x, y| {
                    map_px(x + offset.0, y + offset.1)
                        .map(|(nx, ny)| (nx - ancestor.0, ny - ancestor.1))
                })?;
                if moved != *mask {
                    ops.push(Op::SetMask {
                        layer: node.id,
                        mask: Some(moved),
                    });
                }
            }
            if let Some(mask) = &node.props.vector_mask {
                let mut next = mask.clone();
                next.path = map_commands(&mask.path, &local);
                if *next != **mask {
                    ops.push(Op::SetVectorMask {
                        id: node.id,
                        mask: Some(*next),
                    });
                }
            }
            if let Some(path) = &node.props.clip_path {
                let mut next = path.clone();
                next.path = map_commands(&path.path, &local);
                if *next != **path {
                    ops.push(Op::SetClipPath {
                        id: node.id,
                        path: Some(*next),
                    });
                }
            }
            let mut effects = node.props.effects.clone();
            let mut effects_changed = false;
            for effect in &mut effects {
                if let Some(map) = effect.selection().cloned() {
                    let moved = remap_map(self, &map, |x, y| {
                        map_px(x + offset.0, y + offset.1)
                            .map(|(nx, ny)| (nx - ancestor.0, ny - ancestor.1))
                    })?;
                    *effect.selection_mut() = Some(moved);
                    effects_changed = true;
                }
            }
            if node.props.transform != Affine2::default() || effects_changed {
                ops.push(Op::SetProps {
                    id: node.id,
                    patch: PropPatch {
                        transform: Some(Affine2::default()),
                        effects: effects_changed.then_some(effects),
                        ..PropPatch::default()
                    },
                });
            }
        }
        if let Some(selection) = &doc.selection {
            let moved = remap_map(self, selection, &map_px)?;
            if moved != *selection {
                ops.push(Op::SetSelection {
                    selection: (!moved.tiles.is_empty()).then_some(moved),
                });
            }
        }
        if doc.meta.width != new_w || doc.meta.height != new_h {
            let mut meta = doc.meta.clone();
            meta.width = new_w;
            meta.height = new_h;
            ops.push(Op::SetMeta { meta });
        }
        if ops.is_empty() {
            return Ok(());
        }
        self.transact(handle, intent, ops, doc.revision)?;
        Ok(())
    }

    pub fn crop(
        &mut self,
        handle: DocHandle,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) -> Result<(), EngineError> {
        {
            let doc = self.document(handle)?;
            if width == 0 || height == 0 || x < 0 || y < 0 {
                return Err(bad("Crop rectangle is outside the canvas"));
            }
            let right = x
                .checked_add(i32::try_from(width).map_err(|_| bad("Crop rectangle is too large"))?)
                .ok_or_else(|| bad("Crop rectangle is too large"))?;
            let bottom = y
                .checked_add(i32::try_from(height).map_err(|_| bad("Crop rectangle is too large"))?)
                .ok_or_else(|| bad("Crop rectangle is too large"))?;
            if right > doc.meta.width as i32 || bottom > doc.meta.height as i32 {
                return Err(bad("Crop rectangle is outside the canvas"));
            }
        }
        self.geometry(
            handle,
            Geom {
                intent: "image.crop",
                width,
                height,
                map_px: |px, py| {
                    let nx = px - x;
                    let ny = py - y;
                    (nx >= 0 && ny >= 0 && nx < width as i32 && ny < height as i32)
                        .then_some((nx, ny))
                },
                map_pt: |px, py| (px - x as f64, py - y as f64),
                text_scale: None,
                swap_frame: false,
            },
        )
    }

    pub fn rotate90(&mut self, handle: DocHandle, clockwise: bool) -> Result<(), EngineError> {
        let (w, h) = {
            let doc = self.document(handle)?;
            (doc.meta.width, doc.meta.height)
        };
        if clockwise {
            self.geometry(
                handle,
                Geom {
                    intent: "image.rotate",
                    width: h,
                    height: w,
                    map_px: move |x, y| Some((h as i32 - 1 - y, x)),
                    map_pt: move |x, y| (h as f64 - y, x),
                    text_scale: None,
                    swap_frame: true,
                },
            )
        } else {
            self.geometry(
                handle,
                Geom {
                    intent: "image.rotate",
                    width: h,
                    height: w,
                    map_px: move |x, y| Some((y, w as i32 - 1 - x)),
                    map_pt: move |x, y| (y, w as f64 - x),
                    text_scale: None,
                    swap_frame: true,
                },
            )
        }
    }

    pub fn flip(&mut self, handle: DocHandle, horizontal: bool) -> Result<(), EngineError> {
        let (w, h) = {
            let doc = self.document(handle)?;
            (doc.meta.width, doc.meta.height)
        };
        if horizontal {
            self.geometry(
                handle,
                Geom {
                    intent: "image.flip",
                    width: w,
                    height: h,
                    map_px: move |x, y| Some((w as i32 - 1 - x, y)),
                    map_pt: move |x, y| (w as f64 - x, y),
                    text_scale: None,
                    swap_frame: false,
                },
            )
        } else {
            self.geometry(
                handle,
                Geom {
                    intent: "image.flip",
                    width: w,
                    height: h,
                    map_px: move |x, y| Some((x, h as i32 - 1 - y)),
                    map_pt: move |x, y| (x, h as f64 - y),
                    text_scale: None,
                    swap_frame: false,
                },
            )
        }
    }

    pub fn resize(
        &mut self,
        handle: DocHandle,
        width: u32,
        height: u32,
    ) -> Result<(), EngineError> {
        let doc = self.document(handle)?.clone();
        if width == 0 || height == 0 {
            return Err(bad("Canvas size must be positive"));
        }
        let (old_w, old_h) = (doc.meta.width, doc.meta.height);
        let sx = width as f64 / old_w as f64;
        let sy = height as f64 / old_h as f64;
        let mut ops = Vec::new();
        for node in doc.nodes.values() {
            plain(node)?;
            let offset = chain(&doc, node.id, true)?;
            let ancestor = chain(&doc, node.id, false)?;
            let local = |x: f64, y: f64| {
                (
                    (x + offset.0 as f64) * sx - ancestor.0 as f64 * sx,
                    (y + offset.1 as f64) * sy - ancestor.1 as f64 * sy,
                )
            };
            match &node.kind {
                NodeKind::Raster { tiles } => {
                    let moved = resample(
                        self,
                        tiles,
                        offset,
                        (
                            ancestor.0 * width as i32 / old_w as i32,
                            ancestor.1 * height as i32 / old_h as i32,
                        ),
                        (old_w, old_h),
                        (width, height),
                    )?;
                    if let Some(op) = differ(node.id, tiles, &moved) {
                        ops.push(op);
                    }
                }
                NodeKind::Vector { paths } => {
                    let paths: Vec<_> = paths
                        .iter()
                        .map(|path| StyledPath {
                            data: map_commands(&path.data, &local),
                            ..path.clone()
                        })
                        .collect();
                    ops.push(Op::SetVector { id: node.id, paths });
                }
                NodeKind::Text { content } => {
                    let mut content = content.clone();
                    let scale = ((sx + sy) * 0.5) as f32;
                    content.style.size = (content.style.size * scale).clamp(0.01, 4096.0);
                    for span in &mut content.spans {
                        if let Some(size) = span.style.size {
                            span.style.size = Some((size * scale).clamp(0.01, 4096.0));
                        }
                    }
                    if let TextFrame::Area { width, height } = content.frame {
                        content.frame = TextFrame::Area {
                            width: width * sx,
                            height: height * sy,
                        };
                    }
                    ops.push(Op::SetText {
                        id: node.id,
                        content,
                    });
                }
                _ => {}
            }
            let ancestor_scaled = (
                (ancestor.0 as f64 * sx).round() as i32,
                (ancestor.1 as f64 * sy).round() as i32,
            );
            if node.props.transform
                != Affine2([
                    1.0,
                    0.0,
                    0.0,
                    1.0,
                    (offset.0 as f64 * sx - ancestor_scaled.0 as f64),
                    (offset.1 as f64 * sy - ancestor_scaled.1 as f64),
                ])
            {
                ops.push(Op::SetProps {
                    id: node.id,
                    patch: PropPatch {
                        transform: Some(Affine2([
                            1.0,
                            0.0,
                            0.0,
                            1.0,
                            offset.0 as f64 * sx - ancestor_scaled.0 as f64,
                            offset.1 as f64 * sy - ancestor_scaled.1 as f64,
                        ])),
                        ..PropPatch::default()
                    },
                });
            }
        }
        if doc.meta.width != width || doc.meta.height != height {
            let mut meta = doc.meta.clone();
            meta.width = width;
            meta.height = height;
            ops.push(Op::SetMeta { meta });
        }
        if !ops.is_empty() {
            self.transact(handle, "image.resize", ops, doc.revision)?;
        }
        Ok(())
    }

    fn paint_layer(
        &mut self,
        handle: DocHandle,
        layer: NodeId,
        intent: &str,
        mut paint: impl FnMut(i32, i32, [u8; 4]) -> Option<[u8; 4]>,
    ) -> Result<(), EngineError> {
        let (tiles, offset, revision) = {
            let doc = self.document(handle)?;
            let NodeKind::Raster { tiles } = &doc.node(layer)?.kind else {
                return Err(bad("Fill target must be a raster"));
            };
            (tiles.clone(), chain(doc, layer, true)?, doc.revision)
        };
        if tiles.tiles.len() > 4096 {
            return Err(bad("Fill exceeds 4096 tiles"));
        }
        // Each output tile starts as a copy of its source; only changed pixels are rewritten.
        let mut scratch = Scratch::new(PixelFormat::Rgba8);
        for (key, hash) in &tiles.tiles {
            let mut pixels = self.require_tile(hash)?.pixels.to_vec();
            for (i, px) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let x = key.tx * 256 + (i % 256) as i32 + offset.0;
                let y = key.ty * 256 + (i / 256) as i32 + offset.1;
                if let Some(next) = paint(x, y, *px) {
                    *px = next;
                }
            }
            scratch.insert(*key, pixels);
        }
        let moved = scratch.finish(self, PixelFormat::Rgba8)?;
        let Some(op) = differ(layer, &tiles, &moved) else {
            return Ok(());
        };
        self.transact(handle, intent, vec![op], revision)?;
        Ok(())
    }

    pub fn fill(
        &mut self,
        handle: DocHandle,
        layer: NodeId,
        color: [u8; 4],
    ) -> Result<(), EngineError> {
        let selection = self.document(handle)?.selection.clone();
        let decoded = selection
            .as_ref()
            .map(|map| m5::decode(map, self))
            .transpose()?;
        self.paint_layer(handle, layer, "layer.fill", move |x, y, _| {
            if decoded
                .as_ref()
                .is_some_and(|tiles| m5::sample(tiles, x, y) < 128)
            {
                None
            } else {
                Some(color)
            }
        })
    }

    pub fn gradient(
        &mut self,
        handle: DocHandle,
        layer: NodeId,
        gradient: Gradient,
    ) -> Result<(), EngineError> {
        let Gradient {
            from,
            to,
            start,
            end,
            radial,
        } = gradient;
        self.paint_layer(handle, layer, "layer.gradient", move |x, y, _| {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let dx = to.0 - from.0;
            let dy = to.1 - from.1;
            let len = dx.hypot(dy).max(0.0001);
            let t = if radial {
                (px - from.0).hypot(py - from.1) / len
            } else {
                ((px - from.0) * dx + (py - from.1) * dy) / (len * len)
            }
            .clamp(0.0, 1.0);
            Some(std::array::from_fn(|i| {
                (start[i] as f32 + (end[i] as f32 - start[i] as f32) * t).round() as u8
            }))
        })
    }

    pub fn clone_region(
        &mut self,
        handle: DocHandle,
        layer: NodeId,
        from: (i32, i32),
        to: (i32, i32),
        size: (u32, u32),
    ) -> Result<(), EngineError> {
        if size.0 == 0 || size.1 == 0 || size.0 as u64 * size.1 as u64 > 1_048_576 {
            return Err(bad("Clone region exceeds the pixel budget"));
        }
        let copied = {
            let doc = self.document(handle)?;
            let node = doc.node(layer)?;
            let NodeKind::Raster { tiles } = &node.kind else {
                return Err(bad("Clone target must be a raster"));
            };
            let offset = chain(doc, layer, true)?;
            let mut copied = Vec::with_capacity((size.0 * size.1) as usize);
            for y in 0..size.1 as i32 {
                for x in 0..size.0 as i32 {
                    copied.push(read_pixel(
                        self,
                        tiles,
                        from.0 + x - offset.0,
                        from.1 + y - offset.1,
                    )?);
                }
            }
            copied
        };
        self.paint_layer(handle, layer, "layer.stamp", move |x, y, _pixel| {
            let dx = x - to.0;
            let dy = y - to.1;
            if dx < 0 || dy < 0 || dx >= size.0 as i32 || dy >= size.1 as i32 {
                return None;
            }
            Some(copied[(dy as u32 * size.0 + dx as u32) as usize])
        })
    }

    pub fn smudge(
        &mut self,
        handle: DocHandle,
        layer: NodeId,
        at: (i32, i32),
        radius: f32,
        delta: (i32, i32),
    ) -> Result<(), EngineError> {
        if !(1.0..=128.0).contains(&radius) {
            return Err(bad("Smudge radius must be 1..128"));
        }
        let (sources, radius_i, span) = {
            let doc = self.document(handle)?;
            let node = doc.node(layer)?;
            let NodeKind::Raster { tiles } = &node.kind else {
                return Err(bad("Smudge target must be a raster"));
            };
            let offset = chain(doc, layer, true)?;
            let radius_i = radius.ceil() as i32;
            let x0 = at.0 - radius_i - delta.0;
            let y0 = at.1 - radius_i - delta.1;
            let span = radius_i * 2 + 1;
            let mut sources = Vec::with_capacity((span * span) as usize);
            for y in 0..span {
                for x in 0..span {
                    sources.push(read_pixel(
                        self,
                        tiles,
                        x0 + x - offset.0,
                        y0 + y - offset.1,
                    )?);
                }
            }
            (sources, radius_i, span)
        };
        self.paint_layer(handle, layer, "layer.smudge", move |x, y, pixel| {
            let dx = x - at.0;
            let dy = y - at.1;
            if (dx * dx + dy * dy) as f32 > radius * radius {
                return None;
            }
            let ix = x - at.0 + radius_i;
            let iy = y - at.1 + radius_i;
            let source = sources[(iy * span + ix) as usize];
            Some(std::array::from_fn(|i| {
                ((pixel[i] as u16 + source[i] as u16) / 2) as u8
            }))
        })
    }

    pub fn sample_pixel(&self, handle: DocHandle, x: i32, y: i32) -> Result<[u8; 4], EngineError> {
        let doc = self.document(handle)?;
        if x < 0 || y < 0 || x >= doc.meta.width as i32 || y >= doc.meta.height as i32 {
            return Ok([0, 0, 0, 0]);
        }
        pixel_at(self, &draws(doc, doc.root)?, x, y)
    }

    pub fn histogram(
        &self,
        handle: DocHandle,
        layer: NodeId,
    ) -> Result<[[u32; 256]; 4], EngineError> {
        let doc = self.document(handle)?;
        let NodeKind::Raster { tiles } = &doc.node(layer)?.kind else {
            return Err(bad("Histogram requires a raster layer"));
        };
        let mut bins = [[0u32; 256]; 4];
        for hash in tiles.tiles.values() {
            let tile = self.require_tile(hash)?;
            for pixel in tile.pixels.as_chunks::<4>().0 {
                for (channel, value) in pixel.iter().enumerate() {
                    bins[channel][*value as usize] += 1;
                }
            }
        }
        Ok(bins)
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

    pub fn rasterize(&mut self, handle: DocHandle, id: NodeId) -> Result<(), EngineError> {
        let doc = self.document(handle)?.clone();
        let node = doc.node(id)?.clone();
        if !node.props.effects.is_empty() {
            return Err(bad("Rasterize keeps GPU effects on the layer"));
        }
        let ancestor = chain(&doc, id, false)?;
        match &node.kind {
            NodeKind::Raster { tiles } => {
                let offset = chain(&doc, id, true)?;
                let moved = remap_map(self, tiles, |x, y| {
                    Some((x + offset.0 - ancestor.0, y + offset.1 - ancestor.1))
                })?;
                let mut ops = Vec::new();
                if let Some(op) = differ(id, tiles, &moved) {
                    ops.push(op);
                }
                if node.props.mask.is_some() {
                    ops.push(Op::SetMask {
                        layer: id,
                        mask: None,
                    });
                }
                if node.props.transform != Affine2::default() {
                    ops.push(Op::SetProps {
                        id,
                        patch: PropPatch {
                            transform: Some(Affine2::default()),
                            mask_enabled: Some(false),
                            ..PropPatch::default()
                        },
                    });
                }
                if !ops.is_empty() {
                    self.transact(handle, "layer.rasterize", ops, doc.revision)?;
                }
                Ok(())
            }
            NodeKind::Group { children, .. } => {
                let mut draws = Vec::new();
                for child in children {
                    let child_node = doc.node(*child)?;
                    let NodeKind::Raster { tiles } = &child_node.kind else {
                        return Err(bad("Group rasterize supports raster children"));
                    };
                    if !child_node.props.effects.is_empty() {
                        return Err(bad("Rasterize keeps GPU effects on the layer"));
                    }
                    draws.push(Draw {
                        map: tiles.clone(),
                        offset: chain(&doc, *child, true)?,
                        opacity: child_node.props.opacity,
                        mode: child_node.props.blend as u32,
                    });
                }
                let mut scratch = Scratch::new(PixelFormat::Rgba8);
                for y in 0..doc.meta.height as i32 {
                    for x in 0..doc.meta.width as i32 {
                        let px = pixel_at(self, &draws, x, y)?;
                        if px[3] != 0 {
                            scratch.put(x - ancestor.0, y - ancestor.1, &px);
                        }
                    }
                }
                let tiles = scratch.finish(self, PixelFormat::Rgba8)?;
                let mut replacement = Node::raster(node.name.clone());
                replacement.id = id;
                replacement.parent = node.parent;
                replacement.props = node.props.clone();
                replacement.props.transform = Affine2::default();
                replacement.props.mask = None;
                replacement.props.effects.clear();
                replacement.props.vector_mask = None;
                replacement.props.clip_path = None;
                replacement.kind = NodeKind::Raster { tiles };
                let mut ops: Vec<_> = children
                    .iter()
                    .copied()
                    .map(|id| Op::RemoveNode { id })
                    .collect();
                ops.push(Op::ReplaceNode {
                    id,
                    node: replacement,
                });
                self.transact(handle, "layer.rasterize", ops, doc.revision)?;
                Ok(())
            }
            _ => Err(bad("Rasterize supports a raster or a group of rasters")),
        }
    }

    pub fn merge_down(&mut self, handle: DocHandle, id: NodeId) -> Result<(), EngineError> {
        let doc = self.document(handle)?.clone();
        let node = doc.node(id)?;
        let parent = node
            .parent
            .ok_or_else(|| bad("The root cannot merge down"))?;
        let siblings = doc
            .node(parent)?
            .children()
            .ok_or_else(|| bad("Merge target has no parent group"))?;
        let index = siblings
            .iter()
            .position(|child| *child == id)
            .ok_or_else(|| bad("Parent link broken"))?;
        if index == 0 {
            return Err(bad("There is no layer below"));
        }
        let lower = siblings[index - 1];
        let NodeKind::Raster { tiles: upper_tiles } = &node.kind else {
            return Err(bad("Merge supports raster layers"));
        };
        let lower_node = doc.node(lower)?;
        let NodeKind::Raster { tiles: lower_tiles } = &lower_node.kind else {
            return Err(bad("Merge supports raster layers"));
        };
        let upper_tiles = upper_tiles.clone();
        let lower_tiles = lower_tiles.clone();
        let upper_offset = chain(&doc, id, true)?;
        let lower_offset = chain(&doc, lower, true)?;
        let upper = Draw {
            map: upper_tiles,
            offset: upper_offset,
            opacity: node.props.opacity,
            mode: node.props.blend as u32,
        };
        let lower_draw = Draw {
            map: lower_tiles.clone(),
            offset: lower_offset,
            opacity: lower_node.props.opacity,
            mode: 0,
        };
        let mut scratch = Scratch::new(PixelFormat::Rgba8);
        for (key, hash) in &lower_tiles.tiles {
            let tile = self.require_tile(hash)?;
            for i in 0..(256 * 256) {
                scratch.put(
                    key.tx * 256 + (i % 256) as i32,
                    key.ty * 256 + (i / 256) as i32,
                    &tile.pixels[i * 4..i * 4 + 4],
                );
            }
        }
        for (key, hash) in &upper.map.tiles {
            let tile = self.require_tile(hash)?;
            for i in 0..(256 * 256) {
                let local_x = key.tx * 256 + (i % 256) as i32;
                let local_y = key.ty * 256 + (i / 256) as i32;
                let src = [
                    tile.pixels[i * 4],
                    tile.pixels[i * 4 + 1],
                    tile.pixels[i * 4 + 2],
                    tile.pixels[i * 4 + 3],
                ];
                if src[3] == 0 {
                    continue;
                }
                let dest_x = local_x + upper_offset.0 - lower_offset.0;
                let dest_y = local_y + upper_offset.1 - lower_offset.1;
                let base = scratch.get(dest_x, dest_y);
                let blended = straight(composite_pixel(
                    [
                        base[0] as f32 / 255.0 * base[3] as f32 / 255.0,
                        base[1] as f32 / 255.0 * base[3] as f32 / 255.0,
                        base[2] as f32 / 255.0 * base[3] as f32 / 255.0,
                        base[3] as f32 / 255.0,
                    ],
                    src,
                    upper.opacity,
                    upper.mode,
                ));
                scratch.put(dest_x, dest_y, &blended);
            }
        }
        let _ = lower_draw;
        let moved = scratch.finish(self, PixelFormat::Rgba8)?;
        let mut ops = Vec::new();
        if let Some(op) = differ(lower, &lower_tiles, &moved) {
            ops.push(op);
        }
        ops.push(Op::RemoveNode { id });
        self.transact(handle, "layer.merge", ops, doc.revision)?;
        Ok(())
    }

    /// One undoable edit from render version 1 to version 2. Bilinear and rotated
    /// pixels change, so this is never applied by load or save. The live
    /// `format_version` becomes 3, which is what a snapshot of version 2 writes,
    /// so the journal digest matches a document opened from that snapshot.
    /// A document already on version 2 or 3 is rejected and the history is unchanged.
    /// This method does not move a document onto render version 3.
    pub fn upgrade_render_version(&mut self, handle: DocHandle) -> Result<ChangeSet, EngineError> {
        let ops = self.render_upgrade_ops(handle)?;
        let revision = self.document(handle)?.revision;
        self.transact(handle, "render.upgrade", ops, revision)
    }

    /// The single `SetMeta` of [`Self::upgrade_render_version`]. The WASM command
    /// applies this through the same journal path as other edits. It never
    /// selects render version 3.
    pub fn render_upgrade_ops(&self, handle: DocHandle) -> Result<Vec<Op>, EngineError> {
        let doc = self.document(handle)?;
        if doc.meta.render_version != RENDER_VERSION_F32 {
            return Err(bad("Render version is already current"));
        }
        let mut meta = doc.meta.clone();
        meta.render_version = RENDER_VERSION;
        meta.format_version = 3;
        Ok(vec![Op::SetMeta { meta }])
    }

    /// One undoable edit onto render version 3. Refused unless
    /// [`crate::unstable_render::set_unstable_render_v3`] accepted the request.
    /// Release builds never accept it. A document already on version 3 is rejected
    /// and the history is unchanged.
    pub fn upgrade_render_version_unstable(
        &mut self,
        handle: DocHandle,
    ) -> Result<ChangeSet, EngineError> {
        let ops = self.render_upgrade_ops_unstable(handle)?;
        let revision = self.document(handle)?.revision;
        self.transact(handle, "render.upgrade", ops, revision)
    }

    /// The single `SetMeta` of [`Self::upgrade_render_version_unstable`].
    pub fn render_upgrade_ops_unstable(&self, handle: DocHandle) -> Result<Vec<Op>, EngineError> {
        if !crate::unstable_render::unstable_render_v3_enabled() {
            return Err(bad(crate::unstable_render::UNSTABLE_RENDER_V3_OFF));
        }
        let doc = self.document(handle)?;
        if doc.meta.render_version >= RENDER_VERSION_EXACT {
            return Err(bad("Render version is already current"));
        }
        let mut meta = doc.meta.clone();
        meta.render_version = RENDER_VERSION_EXACT;
        meta.format_version = 4;
        Ok(vec![Op::SetMeta { meta }])
    }
}

fn paths_ref(node: &Node) -> &Vec<StyledPath> {
    match &node.kind {
        NodeKind::Vector { paths } => paths,
        _ => unreachable!(),
    }
}
fn node_text(node: &Node) -> &alto_text::TextContent {
    match &node.kind {
        NodeKind::Text { content } => content,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alto_core::PixelFormat;
    use alto_doc::{BlendMode, Node, NodeKind};
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

    fn dot(engine: &mut Engine, handle: DocHandle, layer: NodeId, x: i32, y: i32, color: [u8; 4]) {
        let mut pixels = vec![0; 256 * 256 * 4];
        let i = (y.rem_euclid(256) as usize * 256 + x.rem_euclid(256) as usize) * 4;
        pixels[i..i + 4].copy_from_slice(&color);
        let hash = engine
            .put_tile(TileData::new(PixelFormat::Rgba8, pixels).unwrap())
            .unwrap();
        let key = TileKey {
            tx: x.div_euclid(256),
            ty: y.div_euclid(256),
        };
        let revision = engine.document(handle).unwrap().revision;
        engine
            .transact(
                handle,
                "paint",
                vec![Op::SetTiles {
                    layer,
                    changes: vec![TileChange {
                        key,
                        before: None,
                        after: Some(hash),
                    }],
                }],
                revision,
            )
            .unwrap();
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
        assert_eq!(value["meta"]["format_version"], 3);
        assert_eq!(value["meta"]["render_version"], 2);
        let reopened = engine.open_snapshot(&snapshot).unwrap();
        assert_eq!(engine.document(reopened).unwrap().meta.format_version, 3);
        engine.undo(handle).unwrap();
        let snapshot = engine.snapshot(handle).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
        // The document is still render version 2, so removing levels does not
        // drop the file back to a format a pre-#10 build would accept.
        assert_eq!(value["meta"]["format_version"], 3);
        assert_eq!(value["meta"]["render_version"], 2);
        engine.open_snapshot(&snapshot).unwrap();
    }

    #[test]
    fn render_version_one_levels_stay_format_two_and_resave_identically() {
        let mut engine = Engine::new();
        let mut meta = DocumentMeta::srgb_u8(32, 32);
        meta.render_version = alto_core::RENDER_VERSION_F32;
        meta.format_version = 1;
        let handle = engine.create_document(meta).unwrap();
        let id = layer(&mut engine, handle);
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
        assert_eq!(value["meta"]["format_version"], 2);
        assert!(value["meta"].get("render_version").is_none());
        let reopened = engine.open_snapshot(&snapshot).unwrap();
        let again = engine.document(reopened).unwrap().snapshot_bytes().unwrap();
        assert_eq!(again, snapshot);
        engine.undo(handle).unwrap();
        let snapshot = engine.snapshot(handle).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
        assert_eq!(value["meta"]["format_version"], 1);
        assert!(value["meta"].get("render_version").is_none());
        let reopened = engine.open_snapshot(&snapshot).unwrap();
        let again = engine.document(reopened).unwrap().snapshot_bytes().unwrap();
        assert_eq!(again, snapshot);
    }

    #[test]
    fn render_upgrade_is_one_undoable_edit_and_load_save_stays_on_version_one() {
        let mut engine = Engine::new();
        let mut meta = DocumentMeta::srgb_u8(32, 32);
        meta.render_version = alto_core::RENDER_VERSION_F32;
        meta.format_version = 1;
        let handle = engine.create_document(meta).unwrap();
        let before = engine.snapshot(handle).unwrap();
        let opened = engine.open_snapshot(&before).unwrap();
        assert_eq!(engine.snapshot(opened).unwrap(), before);
        assert_eq!(engine.history_state(opened).unwrap(), (0, 0));
        assert_eq!(
            engine.document(opened).unwrap().meta.render_version,
            alto_core::RENDER_VERSION_F32
        );
        let value: serde_json::Value = serde_json::from_slice(&before).unwrap();
        assert!(value["meta"].get("render_version").is_none());
        assert_eq!(value["meta"]["format_version"], 1);

        engine.upgrade_render_version(handle).unwrap();
        assert_eq!(engine.history_state(handle).unwrap(), (1, 0));
        assert_eq!(
            engine.document(handle).unwrap().meta.render_version,
            alto_core::RENDER_VERSION
        );
        assert_eq!(engine.document(handle).unwrap().meta.format_version, 3);
        let upgraded = engine.snapshot(handle).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&upgraded).unwrap();
        assert_eq!(value["meta"]["render_version"], 2);
        assert_eq!(value["meta"]["format_version"], 3);
        let reopened = engine.open_snapshot(&upgraded).unwrap();
        assert_eq!(
            engine.document(reopened).unwrap().snapshot_bytes().unwrap(),
            upgraded
        );

        engine.undo(handle).unwrap();
        assert_eq!(engine.history_state(handle).unwrap(), (0, 1));
        assert_eq!(engine.snapshot(handle).unwrap(), before);
        assert_eq!(
            engine.document(handle).unwrap().meta.render_version,
            alto_core::RENDER_VERSION_F32
        );

        engine.redo(handle).unwrap();
        assert_eq!(engine.snapshot(handle).unwrap(), upgraded);
        let err = engine.upgrade_render_version(handle).unwrap_err();
        assert!(err.to_string().contains("already current"), "{err}");
        assert_eq!(engine.history_state(handle).unwrap(), (1, 0));
    }

    /// The browser upgrade test opens this package. It is a version-1 512×512
    /// document with the scale-bilinear ramp, so production readTile can be
    /// compared with `renderCase("scale-bilinear")` after the explicit upgrade.
    #[test]
    fn render_v1_bilinear_fixture_is_version_one_and_load_save_does_not_upgrade() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/render-v1-bilinear.alto");
        let bytes = std::fs::read(&path).unwrap();
        let mut engine = Engine::new();
        let (handle, warnings) = engine.open_package(&bytes).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(engine.history_state(handle).unwrap(), (0, 0));
        let (transform, tile_hash) = {
            let doc = engine.document(handle).unwrap();
            assert_eq!(doc.meta.width, 512);
            assert_eq!(doc.meta.height, 512);
            assert_eq!(doc.meta.render_version, alto_core::RENDER_VERSION_F32);
            assert_eq!(doc.meta.format_version, 1);
            let root = doc.node(doc.root).unwrap();
            let children = root.children().unwrap();
            assert_eq!(children.len(), 1);
            let layer = doc.node(children[0]).unwrap();
            let transform = layer.props.transform;
            let NodeKind::Raster { tiles } = &layer.kind else {
                panic!("fixture layer is not a raster");
            };
            assert_eq!(tiles.tiles.len(), 1);
            let (key, hash) = tiles.tiles.iter().next().unwrap();
            assert_eq!(*key, TileKey { tx: 0, ty: 0 });
            (transform, *hash)
        };
        assert_eq!(
            transform,
            alto_core::Affine2([2.0, 0.0, 0.0, 2.0, 0.0, 0.0])
        );
        let tile = engine.require_tile(&tile_hash).unwrap();
        for y in 0..256u32 {
            for x in 0..256u32 {
                let i = ((y * 256 + x) * 4) as usize;
                assert_eq!(tile.pixels[i], (x & 255) as u8);
                assert_eq!(tile.pixels[i + 1], (y & 255) as u8);
                assert_eq!(tile.pixels[i + 2], ((x * 3 + y * 5) & 255) as u8);
                assert_eq!(tile.pixels[i + 3], 255);
            }
        }
        drop(tile);
        let snapshot = engine.snapshot(handle).unwrap();
        let text = String::from_utf8(snapshot.clone()).unwrap();
        assert!(!text.contains("render_version"));
        assert!(text.contains("\"format_version\": 1"));
        let opened = engine.open_snapshot(&snapshot).unwrap();
        assert_eq!(engine.snapshot(opened).unwrap(), snapshot);
        let again = engine.save_package(handle).unwrap();
        let (reopened, warnings) = engine.open_package(&again).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let resaved = String::from_utf8(engine.snapshot(reopened).unwrap()).unwrap();
        assert_eq!(
            engine.document(reopened).unwrap().meta.render_version,
            alto_core::RENDER_VERSION_F32
        );
        assert!(!resaved.contains("render_version"));
        assert!(resaved.contains("\"format_version\": 1"));
    }

    #[test]
    fn crop_fill_clone_and_sample_undo() {
        let mut engine = Engine::new();
        let handle = engine.create_document(DocumentMeta::srgb_u8(8, 8)).unwrap();
        let id = layer(&mut engine, handle);
        dot(&mut engine, handle, id, 2, 1, [10, 20, 30, 255]);
        engine.crop(handle, 2, 1, 4, 4).unwrap();
        assert_eq!(engine.document(handle).unwrap().meta.width, 4);
        assert_eq!(
            engine.sample_pixel(handle, 0, 0).unwrap(),
            [10, 20, 30, 255]
        );
        engine.undo(handle).unwrap();
        assert_eq!(engine.document(handle).unwrap().meta.width, 8);
        assert_eq!(
            engine.sample_pixel(handle, 2, 1).unwrap(),
            [10, 20, 30, 255]
        );
        engine.fill(handle, id, [1, 2, 3, 255]).unwrap();
        assert_eq!(engine.sample_pixel(handle, 2, 1).unwrap(), [1, 2, 3, 255]);
        engine.undo(handle).unwrap();
        assert_eq!(
            engine.sample_pixel(handle, 2, 1).unwrap(),
            [10, 20, 30, 255]
        );
        engine
            .clone_region(handle, id, (2, 1), (5, 6), (1, 1))
            .unwrap();
        assert_eq!(
            engine.sample_pixel(handle, 5, 6).unwrap(),
            [10, 20, 30, 255]
        );
        engine.undo(handle).unwrap();
        assert_eq!(engine.sample_pixel(handle, 5, 6).unwrap(), [0, 0, 0, 0]);
        let bins = engine.histogram(handle, id).unwrap();
        assert_eq!(bins[0][10], 1);
    }
}
