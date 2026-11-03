//! Whole-document geometry and layer baking. Layer-local pixel edits live in `image`.
//!
//! A canvas edit moves the root node's transform and resizes the canvas, so every
//! raster, mask, vector and text follows without touching a pixel and one Undo restores
//! it. Only what is stored in document coordinates has to be rewritten.
use crate::image::{EDIT_BUDGET, FlipAxis, err, over, read};
use crate::{DocHandle, Engine, EngineError};
use alto_core::{Affine2, NodeId, PixelFormat, TILE, TileKey};
use alto_doc::{Effect, ImageBounds, Isolation, Node, NodeKind, Op, PropPatch};
use alto_raster::{TileData, TileMap};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const T: i32 = TILE as i32;
/// Selections are capped at this many tiles wherever they are generated.
const MAX_SELECTION_TILES: usize = 4096;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CanvasEdit {
    /// The rectangle becomes the canvas. It may reach past the old canvas, and
    /// content outside it is kept, only no longer visible.
    Crop {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    /// Scales everything to the new canvas size.
    Resize {
        width: u32,
        height: u32,
    },
    Rotate {
        quarter_turns: u8,
    },
    Flip {
        axis: FlipAxis,
    },
}
impl CanvasEdit {
    pub fn intent(&self) -> &'static str {
        match self {
            Self::Crop { .. } => "canvas.crop",
            Self::Resize { .. } => "canvas.resize",
            Self::Rotate { .. } => "canvas.rotate",
            Self::Flip { .. } => "canvas.flip",
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct CanvasEditResult {
    pub width: u32,
    pub height: u32,
    pub no_op: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LayerEdit {
    MergeDown { node: NodeId },
    Rasterize { node: NodeId },
}
impl LayerEdit {
    pub fn intent(&self) -> &'static str {
        match self {
            Self::MergeDown { .. } => "layer.merge",
            Self::Rasterize { .. } => "layer.rasterize",
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct LayerEditResult {
    pub node: NodeId,
    pub bounds: ImageBounds,
    pub no_op: bool,
}
/// Output hashes stay rooted by `stage` until the ops are applied or dropped.
pub struct PreparedOps<R> {
    pub stage: u64,
    pub ops: Vec<Op>,
    pub result: R,
}

#[derive(Clone, Debug, Serialize)]
pub struct Histogram {
    pub r: Vec<u64>,
    pub g: Vec<u64>,
    pub b: Vec<u64>,
    pub a: Vec<u64>,
    pub pixels: u64,
}

fn unsupported(message: &str) -> EngineError {
    err("UNSUPPORTED", message)
}

/// The integer offset of a pure whole-pixel translation.
fn whole_translation(m: Affine2) -> Option<(i32, i32)> {
    let m = m.0;
    let near = |v: f64, to: f64| (v - to).abs() <= 1e-9;
    if !(near(m[0], 1.0) && near(m[1], 0.0) && near(m[2], 0.0) && near(m[3], 1.0)) {
        return None;
    }
    let (x, y) = (m[4].round(), m[5].round());
    (near(m[4], x)
        && near(m[5], y)
        && x.abs() < f64::from(i32::MAX)
        && y.abs() < f64::from(i32::MAX))
    .then_some((x as i32, y as i32))
}

/// Nothing that would be lost or change meaning when the pixels are baked into one layer.
fn plain_raster(node: &Node) -> Result<(ImageBounds, TileMap), EngineError> {
    let NodeKind::Raster { bounds, tiles } = &node.kind else {
        return Err(unsupported("only raster layers can be baked"));
    };
    let p = &node.props;
    if p.mask.is_some() || p.vector_mask.is_some() || p.clip_path.is_some() {
        return Err(unsupported("bake does not apply masks or clipping paths"));
    }
    if !p.effects.is_empty() {
        return Err(unsupported("bake does not apply effects"));
    }
    if p.clip_to_below {
        return Err(unsupported("bake does not apply clipping"));
    }
    if !p.visible {
        return Err(unsupported("a hidden layer would be lost"));
    }
    if p.blend != alto_doc::BlendMode::Normal {
        return Err(unsupported("bake supports the Normal blend mode"));
    }
    Ok((*bounds, tiles.clone()))
}

fn unlocked(engine: &Engine, h: DocHandle, id: NodeId) -> Result<(), EngineError> {
    let doc = engine.document(h)?;
    let mut next = Some(id);
    while let Some(id) = next {
        let n = doc.node(id)?;
        if n.props.locked {
            return Err(err("INVALID_ARGUMENT", "layer is locked"));
        }
        next = n.parent;
    }
    Ok(())
}

fn budget(tiles: usize) -> Result<(), EngineError> {
    if tiles as u64 * u64::from(TILE) * u64::from(TILE) * 4 > EDIT_BUDGET {
        return Err(err(
            "RESOURCE_LIMIT",
            "layer edit working-set budget exceeded",
        ));
    }
    Ok(())
}

/// Composites `source` (at `offset`, with `opacity`) over `out` inside `bounds`.
fn composite_into(
    engine: &Engine,
    out: &mut BTreeMap<TileKey, Vec<u8>>,
    source: &TileMap,
    offset: (i32, i32),
    opacity: f64,
) -> Result<(), EngineError> {
    for (key, hash) in &source.tiles {
        let tile = engine.require_tile(hash)?;
        if tile.format != PixelFormat::Rgba8 {
            return Err(err("CORRUPT_DATA", "tile format mismatch"));
        }
        for (i, p) in tile.pixels.as_chunks::<4>().0.iter().enumerate() {
            if p[3] == 0 {
                continue;
            }
            let x = key.tx * T + (i % 256) as i32 + offset.0;
            let y = key.ty * T + (i / 256) as i32 + offset.1;
            let tile_key = TileKey {
                tx: x.div_euclid(T),
                ty: y.div_euclid(T),
            };
            let j = (y.rem_euclid(T) as usize * 256 + x.rem_euclid(T) as usize) * 4;
            let base = out.get(&tile_key).map_or([0; 4], |t| {
                let mut b = [0; 4];
                b.copy_from_slice(&t[j..j + 4]);
                b
            });
            let src = [
                f64::from(p[0]) / 255.0,
                f64::from(p[1]) / 255.0,
                f64::from(p[2]) / 255.0,
                f64::from(p[3]) / 255.0 * opacity,
            ];
            write_exact(out, tile_key, j, over(base, src, 255));
        }
        budget(out.len())?;
    }
    Ok(())
}

/// Unlike `write`, stores a zero result too, because a pixel may be erased here.
fn write_exact(out: &mut BTreeMap<TileKey, Vec<u8>>, key: TileKey, i: usize, p: [u8; 4]) {
    let tile = out.entry(key).or_insert_with(|| vec![0; 262144]);
    tile[i..i + 4].copy_from_slice(&p);
}

/// A map whose alpha is scaled by `opacity`, or a plain decode when it is one.
fn decoded(
    engine: &Engine,
    map: &TileMap,
    opacity: f64,
) -> Result<BTreeMap<TileKey, Vec<u8>>, EngineError> {
    budget(map.tiles.len())?;
    let mut out = BTreeMap::new();
    for (key, hash) in &map.tiles {
        let mut pixels = engine.require_tile(hash)?.pixels.to_vec();
        if opacity < 1.0 {
            for p in pixels.as_chunks_mut::<4>().0 {
                p[3] = (f64::from(p[3]) * opacity).round() as u8;
                if p[3] == 0 {
                    *p = [0; 4];
                }
            }
        }
        out.insert(*key, pixels);
    }
    Ok(out)
}

fn stage_map(
    engine: &mut Engine,
    stage: u64,
    out: BTreeMap<TileKey, Vec<u8>>,
) -> Result<TileMap, EngineError> {
    let mut tiles = TileMap::new(PixelFormat::Rgba8);
    for (key, pixels) in out {
        if let Some(hash) = engine.stage_tile(stage, TileData::new(PixelFormat::Rgba8, pixels)?)? {
            tiles.tiles.insert(key, hash);
        }
    }
    Ok(tiles)
}

fn shifted(b: ImageBounds, dx: i32, dy: i32) -> Result<ImageBounds, EngineError> {
    let moved = ImageBounds {
        x: b.x
            .checked_add(dx)
            .ok_or_else(|| err("RESOURCE_LIMIT", "image position out of range"))?,
        y: b.y
            .checked_add(dy)
            .ok_or_else(|| err("RESOURCE_LIMIT", "image position out of range"))?,
        ..b
    };
    moved
        .validate()
        .map_err(|e| err("RESOURCE_LIMIT", &e.to_string()))?;
    Ok(moved)
}

/// The R8 selection scaled to a `new`-sized canvas, sampled at destination pixel centers.
fn scaled_selection(
    engine: &Engine,
    map: &TileMap,
    old: (u32, u32),
    new: (u32, u32),
) -> Result<BTreeMap<TileKey, Vec<u8>>, EngineError> {
    let mut out: BTreeMap<TileKey, Vec<u8>> = BTreeMap::new();
    let Some(x0) = map.tiles.keys().map(|k| k.tx).min() else {
        return Ok(out);
    };
    let y0 = map.tiles.keys().map(|k| k.ty).min().unwrap_or(0);
    let x1 = map.tiles.keys().map(|k| k.tx).max().unwrap_or(0);
    let y1 = map.tiles.keys().map(|k| k.ty).max().unwrap_or(0);
    let down = |v: i64, to: u32, from: u32| (v * i64::from(to)).div_euclid(i64::from(from));
    let up = |v: i64, to: u32, from: u32| {
        (v * i64::from(to) + i64::from(from) - 1).div_euclid(i64::from(from))
    };
    // One source pixel of margin: bilinear reads reach the next pixel.
    let dx0 = down(i64::from(x0) * 256 - 1, new.0, old.0);
    let dy0 = down(i64::from(y0) * 256 - 1, new.1, old.1);
    let dx1 = up((i64::from(x1) + 1) * 256 + 1, new.0, old.0);
    let dy1 = up((i64::from(y1) + 1) * 256 + 1, new.1, old.1);
    let tiles = ((dx1 - 1).div_euclid(256) - dx0.div_euclid(256) + 1)
        * ((dy1 - 1).div_euclid(256) - dy0.div_euclid(256) + 1);
    if tiles > MAX_SELECTION_TILES as i64 {
        return Err(err("RESOURCE_LIMIT", "scaled selection exceeds 4096 tiles"));
    }
    let q = |d: i64, to: u32, from: u32| {
        ((2 * d + 1) * i64::from(from) * 256 - 256 * i64::from(to)).div_euclid(2 * i64::from(to))
    };
    for v in dy0..dy1 {
        let qy = q(v, new.1, old.1);
        let (iy, fy) = (qy.div_euclid(256), qy.rem_euclid(256) as u64);
        for u in dx0..dx1 {
            let qx = q(u, new.0, old.0);
            let (ix, fx) = (qx.div_euclid(256), qx.rem_euclid(256) as u64);
            let mut acc = 0u64;
            for (dx, dy, w) in [
                (0, 0, (256 - fx) * (256 - fy)),
                (1, 0, fx * (256 - fy)),
                (0, 1, (256 - fx) * fy),
                (1, 1, fx * fy),
            ] {
                acc += u64::from(read(engine, map, (ix + dx) as i32, (iy + dy) as i32)?[3]) * w;
            }
            let value = ((acc + 32768) / 65536) as u8;
            if value != 0 {
                let key = TileKey {
                    tx: u.div_euclid(256) as i32,
                    ty: v.div_euclid(256) as i32,
                };
                out.entry(key).or_insert_with(|| vec![0; 65536])
                    [v.rem_euclid(256) as usize * 256 + u.rem_euclid(256) as usize] = value;
            }
        }
    }
    Ok(out)
}

/// A selection moved by a whole-pixel `place`. The result is the same pixels at new positions.
fn moved_selection(
    engine: &Engine,
    map: &TileMap,
    place: impl Fn(i32, i32) -> (i32, i32),
) -> Result<BTreeMap<TileKey, Vec<u8>>, EngineError> {
    let mut out: BTreeMap<TileKey, Vec<u8>> = BTreeMap::new();
    for (key, hash) in &map.tiles {
        let tile = engine.require_tile(hash)?;
        if tile.format != PixelFormat::R8 {
            return Err(err("CORRUPT_DATA", "selection requires R8"));
        }
        for (i, value) in tile.pixels.iter().enumerate() {
            if *value == 0 {
                continue;
            }
            let (x, y) = place(key.tx * T + (i % 256) as i32, key.ty * T + (i / 256) as i32);
            let tile_key = TileKey {
                tx: x.div_euclid(T),
                ty: y.div_euclid(T),
            };
            out.entry(tile_key).or_insert_with(|| vec![0; 65536])
                [y.rem_euclid(T) as usize * 256 + x.rem_euclid(T) as usize] = *value;
        }
        if out.len() > MAX_SELECTION_TILES {
            return Err(err("RESOURCE_LIMIT", "selection exceeds 4096 tiles"));
        }
    }
    Ok(out)
}

/// Blur radii are in document pixels, so a scale changes them. A non-uniform scale
/// has no equivalent radius, and refusing beats a different picture.
fn scale_effects(effects: &[Effect], sx: f64, sy: f64) -> Result<Option<Vec<Effect>>, EngineError> {
    if !effects
        .iter()
        .any(|e| matches!(e, Effect::GaussianBlur { .. }))
    {
        return Ok(None);
    }
    if (sx - sy).abs() > 1e-9 {
        return Err(unsupported(
            "a non-uniform canvas resize cannot keep a Gaussian blur",
        ));
    }
    let mut out = effects.to_vec();
    for effect in &mut out {
        if let Effect::GaussianBlur { radius, .. } = effect {
            *radius *= sx;
            if *radius > 64.0 {
                return Err(unsupported("the resized blur radius would exceed 64"));
            }
        }
    }
    Ok(Some(out))
}

impl Engine {
    pub fn prepare_canvas_edit(
        &mut self,
        h: DocHandle,
        edit: CanvasEdit,
    ) -> Result<PreparedOps<CanvasEditResult>, EngineError> {
        let stage = self.begin_tile_stage();
        let result = self.prepare_canvas_inner(h, edit, stage);
        if result.is_err() {
            self.release_staged_tiles(stage);
        }
        result
    }
    fn stage_r8(
        &mut self,
        stage: u64,
        out: BTreeMap<TileKey, Vec<u8>>,
    ) -> Result<TileMap, EngineError> {
        let mut tiles = TileMap::new(PixelFormat::R8);
        for (key, pixels) in out {
            if let Some(hash) = self.stage_tile(stage, TileData::new(PixelFormat::R8, pixels)?)? {
                tiles.tiles.insert(key, hash);
            }
        }
        Ok(tiles)
    }
    fn prepare_canvas_inner(
        &mut self,
        h: DocHandle,
        edit: CanvasEdit,
        stage: u64,
    ) -> Result<PreparedOps<CanvasEditResult>, EngineError> {
        let doc = self.document(h)?;
        let root = doc.root;
        if doc.node(root)?.props.locked {
            return Err(err("INVALID_ARGUMENT", "document is locked"));
        }
        let (w, ht) = (doc.meta.width, doc.meta.height);
        let (fw, fh) = (f64::from(w), f64::from(ht));
        let (width, height, map) = match &edit {
            CanvasEdit::Crop {
                x,
                y,
                width,
                height,
            } => (
                *width,
                *height,
                Affine2([1.0, 0.0, 0.0, 1.0, -f64::from(*x), -f64::from(*y)]),
            ),
            CanvasEdit::Resize { width, height } => {
                if *width == 0 || *height == 0 {
                    return Err(err("INVALID_ARGUMENT", "canvas size must be positive"));
                }
                (
                    *width,
                    *height,
                    Affine2([
                        f64::from(*width) / fw,
                        0.0,
                        0.0,
                        f64::from(*height) / fh,
                        0.0,
                        0.0,
                    ]),
                )
            }
            CanvasEdit::Rotate { quarter_turns: q } => match q {
                1 => (ht, w, Affine2([0.0, 1.0, -1.0, 0.0, fh, 0.0])),
                2 => (w, ht, Affine2([-1.0, 0.0, 0.0, -1.0, fw, fh])),
                3 => (ht, w, Affine2([0.0, -1.0, 1.0, 0.0, 0.0, fw])),
                _ => {
                    return Err(err("INVALID_ARGUMENT", "quarterTurns must be 1, 2, or 3"));
                }
            },
            CanvasEdit::Flip { axis } => (
                w,
                ht,
                match axis {
                    FlipAxis::Horizontal => Affine2([-1.0, 0.0, 0.0, 1.0, fw, 0.0]),
                    FlipAxis::Vertical => Affine2([1.0, 0.0, 0.0, -1.0, 0.0, fh]),
                },
            ),
        };
        if width == 0 || height == 0 || width > 65536 || height > 65536 {
            return Err(err("INVALID_ARGUMENT", "canvas size must be 1 to 65536"));
        }
        let identity = map == Affine2::default() && (width, height) == (w, ht);
        if identity {
            return Ok(PreparedOps {
                stage,
                ops: vec![],
                result: CanvasEditResult {
                    width,
                    height,
                    no_op: true,
                },
            });
        }
        let mut ops = vec![];
        let nodes: Vec<Node> = doc.nodes.values().cloned().collect();
        let selection = doc.selection.clone();
        let mut meta = doc.meta.clone();
        for node in &nodes {
            let mut patch = PropPatch::default();
            if node.id == root {
                patch.transform = Some(map.concat(node.props.transform));
            }
            // An unlinked mask is placed in document coordinates, outside the node chain.
            if !node.props.mask_linked && node.props.mask.is_some() {
                patch.mask_transform = Some(map.concat(node.props.mask_transform));
            }
            if let CanvasEdit::Resize { .. } = edit {
                patch.effects = scale_effects(&node.props.effects, map.0[0], map.0[3])?;
                if let NodeKind::Adjustment { adjustment } = &node.kind
                    && let Some(scaled) =
                        scale_effects(std::slice::from_ref(adjustment), map.0[0], map.0[3])?
                {
                    ops.push(Op::SetAdjustment {
                        id: node.id,
                        adjustment: scaled.into_iter().next().expect("one adjustment"),
                    });
                }
            }
            if patch != PropPatch::default() {
                ops.push(Op::SetProps { id: node.id, patch });
            }
        }
        if let Some(selection) = &selection {
            let out = match &edit {
                CanvasEdit::Crop { x, y, .. } => {
                    let (x, y) = (*x, *y);
                    moved_selection(self, selection, |px, py| (px - x, py - y))?
                }
                CanvasEdit::Resize { .. } => {
                    scaled_selection(self, selection, (w, ht), (width, height))?
                }
                CanvasEdit::Rotate { quarter_turns: q } => {
                    let (wi, hi) = (w as i32, ht as i32);
                    match q {
                        1 => moved_selection(self, selection, |x, y| (hi - 1 - y, x))?,
                        2 => moved_selection(self, selection, |x, y| (wi - 1 - x, hi - 1 - y))?,
                        _ => moved_selection(self, selection, |x, y| (y, wi - 1 - x))?,
                    }
                }
                CanvasEdit::Flip { axis } => {
                    let (wi, hi) = (w as i32, ht as i32);
                    match axis {
                        FlipAxis::Horizontal => {
                            moved_selection(self, selection, |x, y| (wi - 1 - x, y))?
                        }
                        FlipAxis::Vertical => {
                            moved_selection(self, selection, |x, y| (x, hi - 1 - y))?
                        }
                    }
                }
            };
            let moved = self.stage_r8(stage, out)?;
            ops.push(Op::SetSelection {
                selection: (!moved.tiles.is_empty()).then_some(moved),
            });
        }
        meta.width = width;
        meta.height = height;
        ops.push(Op::SetMeta { meta });
        Ok(PreparedOps {
            stage,
            ops,
            result: CanvasEditResult {
                width,
                height,
                no_op: false,
            },
        })
    }

    pub fn prepare_layer_edit(
        &mut self,
        h: DocHandle,
        edit: LayerEdit,
    ) -> Result<PreparedOps<LayerEditResult>, EngineError> {
        let stage = self.begin_tile_stage();
        let result = match edit {
            LayerEdit::MergeDown { node } => self.prepare_merge_down(h, node, stage),
            LayerEdit::Rasterize { node } => self.prepare_rasterize(h, node, stage),
        };
        if result.is_err() {
            self.release_staged_tiles(stage);
        }
        result
    }

    fn prepare_merge_down(
        &mut self,
        h: DocHandle,
        id: NodeId,
        stage: u64,
    ) -> Result<PreparedOps<LayerEditResult>, EngineError> {
        unlocked(self, h, id)?;
        let doc = self.document(h)?;
        let upper = doc.node(id)?.clone();
        let parent = upper
            .parent
            .ok_or_else(|| err("INVALID_ARGUMENT", "the root cannot merge down"))?;
        let siblings = doc
            .node(parent)?
            .children()
            .ok_or_else(|| err("CORRUPT_DATA", "parent has no children"))?
            .to_vec();
        let index = siblings
            .iter()
            .position(|child| *child == id)
            .ok_or_else(|| err("CORRUPT_DATA", "parent link broken"))?;
        if index == 0 {
            return Err(err("INVALID_ARGUMENT", "there is no layer below"));
        }
        let lower = doc.node(siblings[index - 1])?.clone();
        if lower.props.locked {
            return Err(err("INVALID_ARGUMENT", "the layer below is locked"));
        }
        // A layer clipped to the upper layer would lose its base.
        if let Some(above) = siblings.get(index + 1)
            && doc.node(*above)?.props.clip_to_below
        {
            return Err(unsupported("a layer above is clipped to this layer"));
        }
        let (upper_bounds, upper_tiles) = plain_raster(&upper)?;
        let (lower_bounds, lower_tiles) = plain_raster(&lower)?;
        let inverse = lower
            .props
            .transform
            .inverse()
            .ok_or_else(|| unsupported("the layer below has a singular transform"))?;
        let (dx, dy) = whole_translation(inverse.concat(upper.props.transform))
            .ok_or_else(|| unsupported("the layers are not whole pixels apart"))?;
        let moved = shifted(upper_bounds, dx, dy)?;
        let bounds = lower_bounds
            .union(moved)
            .map_err(|e| err("RESOURCE_LIMIT", &e.to_string()))?;
        let lower_opacity = f64::from(lower.props.opacity);
        let mut out = decoded(self, &lower_tiles, lower_opacity)?;
        composite_into(
            self,
            &mut out,
            &upper_tiles,
            (dx, dy),
            f64::from(upper.props.opacity),
        )?;
        let tiles = stage_map(self, stage, out)?;
        let mut node = lower;
        node.props.opacity = 1.0;
        node.kind = NodeKind::Raster { bounds, tiles };
        let result = LayerEditResult {
            node: node.id,
            bounds,
            no_op: false,
        };
        Ok(PreparedOps {
            stage,
            ops: vec![Op::ReplaceNode { id: node.id, node }, Op::RemoveNode { id }],
            result,
        })
    }

    fn prepare_rasterize(
        &mut self,
        h: DocHandle,
        id: NodeId,
        stage: u64,
    ) -> Result<PreparedOps<LayerEditResult>, EngineError> {
        unlocked(self, h, id)?;
        let doc = self.document(h)?;
        let node = doc.node(id)?.clone();
        if node.parent.is_none() {
            return Err(err("INVALID_ARGUMENT", "the root cannot be rasterized"));
        }
        match &node.kind {
            NodeKind::Raster { bounds, tiles } => {
                let p = &node.props;
                if p.mask.is_some() || p.vector_mask.is_some() || p.clip_path.is_some() {
                    return Err(unsupported("rasterize does not apply masks"));
                }
                if !p.effects.is_empty() {
                    return Err(unsupported("rasterize does not apply effects"));
                }
                let (dx, dy) = whole_translation(p.transform)
                    .ok_or_else(|| unsupported("rasterize needs a whole-pixel translation"))?;
                if (dx, dy) == (0, 0) {
                    return Ok(PreparedOps {
                        stage,
                        ops: vec![],
                        result: LayerEditResult {
                            node: id,
                            bounds: *bounds,
                            no_op: true,
                        },
                    });
                }
                let new_bounds = shifted(*bounds, dx, dy)?;
                let mut out = BTreeMap::new();
                composite_into(self, &mut out, tiles, (dx, dy), 1.0)?;
                let new_tiles = stage_map(self, stage, out)?;
                let mut next = node.clone();
                next.props.transform = Affine2::default();
                next.kind = NodeKind::Raster {
                    bounds: new_bounds,
                    tiles: new_tiles,
                };
                Ok(PreparedOps {
                    stage,
                    ops: vec![Op::ReplaceNode { id, node: next }],
                    result: LayerEditResult {
                        node: id,
                        bounds: new_bounds,
                        no_op: false,
                    },
                })
            }
            NodeKind::Group {
                children,
                isolation,
            } => {
                let p = &node.props;
                if *isolation == Isolation::PassThrough
                    && (p.opacity != 1.0 || p.blend != alto_doc::BlendMode::Normal)
                {
                    return Err(unsupported(
                        "a pass-through group with opacity or a blend mode needs isolation",
                    ));
                }
                if children.is_empty() {
                    return Err(err("INVALID_ARGUMENT", "the group is empty"));
                }
                let mut layers = vec![];
                let mut bounds: Option<ImageBounds> = None;
                for child in children {
                    let child = doc.node(*child)?;
                    if child.props.locked {
                        return Err(err("INVALID_ARGUMENT", "a child layer is locked"));
                    }
                    let (b, tiles) = plain_raster(child)?;
                    let (dx, dy) = whole_translation(child.props.transform)
                        .ok_or_else(|| unsupported("a child is not at a whole-pixel offset"))?;
                    let moved = shifted(b, dx, dy)?;
                    bounds = Some(match bounds {
                        Some(a) => a
                            .union(moved)
                            .map_err(|e| err("RESOURCE_LIMIT", &e.to_string()))?,
                        None => moved,
                    });
                    layers.push((tiles, (dx, dy), f64::from(child.props.opacity)));
                }
                let bounds = bounds.ok_or_else(|| err("INVALID_ARGUMENT", "the group is empty"))?;
                let remove: Vec<NodeId> = children.clone();
                let mut out = BTreeMap::new();
                for (tiles, offset, opacity) in &layers {
                    composite_into(self, &mut out, tiles, *offset, *opacity)?;
                }
                let tiles = stage_map(self, stage, out)?;
                let mut next = node.clone();
                next.kind = NodeKind::Raster { bounds, tiles };
                let mut ops: Vec<Op> = remove.into_iter().map(|id| Op::RemoveNode { id }).collect();
                ops.push(Op::ReplaceNode { id, node: next });
                Ok(PreparedOps {
                    stage,
                    ops,
                    result: LayerEditResult {
                        node: id,
                        bounds,
                        no_op: false,
                    },
                })
            }
            _ => Err(unsupported(
                "rasterize supports a raster or a group of rasters",
            )),
        }
    }

    /// Stored pixels per channel, as they are before opacity, masks and effects.
    /// Transparent pixels inside the range count as zero, tile or no tile.
    pub fn image_histogram(
        &self,
        h: DocHandle,
        id: NodeId,
        range: Option<ImageBounds>,
    ) -> Result<Histogram, EngineError> {
        let NodeKind::Raster { bounds, tiles } = &self.document(h)?.node(id)?.kind else {
            return Err(err("INVALID_ARGUMENT", "raster required"));
        };
        let range = range.unwrap_or(*bounds);
        range.validate()?;
        if range.x < bounds.x
            || range.y < bounds.y
            || range.right() > bounds.right()
            || range.bottom() > bounds.bottom()
        {
            return Err(err(
                "INVALID_ARGUMENT",
                "range must lie inside image bounds",
            ));
        }
        let mut bins = vec![vec![0u64; 256]; 4];
        let total = u64::from(range.width) * u64::from(range.height);
        let mut counted = 0u64;
        for (key, hash) in &tiles.tiles {
            let x0 = (key.tx * T).max(range.x);
            let y0 = (key.ty * T).max(range.y);
            let x1 = (key.tx * T + T).min(range.right());
            let y1 = (key.ty * T + T).min(range.bottom());
            if x0 >= x1 || y0 >= y1 {
                continue;
            }
            let tile = self.require_tile(hash)?;
            for y in y0..y1 {
                let row = (y - key.ty * T) as usize * 256;
                for x in x0..x1 {
                    let i = (row + (x - key.tx * T) as usize) * 4;
                    for (c, bin) in bins.iter_mut().enumerate() {
                        bin[tile.pixels[i + c] as usize] += 1;
                    }
                    counted += 1;
                }
            }
        }
        for bin in &mut bins {
            bin[0] += total - counted;
        }
        let a = bins.pop().unwrap_or_default();
        let b = bins.pop().unwrap_or_default();
        let g = bins.pop().unwrap_or_default();
        let r = bins.pop().unwrap_or_default();
        Ok(Histogram {
            r,
            g,
            b,
            a,
            pixels: total,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{ImageEdit, SelectionMode, Stroke};
    use alto_doc::{DocumentMeta, Node};

    fn doc(width: u32, height: u32) -> (Engine, DocHandle) {
        let mut e = Engine::new();
        let h = e
            .create_document(DocumentMeta::srgb_u8(width, height))
            .unwrap();
        (e, h)
    }
    fn layer(e: &mut Engine, h: DocHandle, b: ImageBounds) -> NodeId {
        let mut n = Node::raster("layer");
        n.kind = NodeKind::Raster {
            bounds: b,
            tiles: TileMap::new(PixelFormat::Rgba8),
        };
        let id = n.id;
        let root = e.document(h).unwrap().root;
        let revision = e.document(h).unwrap().revision;
        let index = e
            .document(h)
            .unwrap()
            .node(root)
            .unwrap()
            .children()
            .unwrap()
            .len();
        e.transact(
            h,
            "add",
            vec![Op::InsertNode {
                node: n,
                parent: root,
                index,
            }],
            revision,
        )
        .unwrap();
        id
    }
    fn bounds(x: i32, y: i32, width: u32, height: u32) -> ImageBounds {
        ImageBounds {
            x,
            y,
            width,
            height,
        }
    }
    fn paint(e: &mut Engine, h: DocHandle, id: NodeId, pixels: &[(i32, i32, [u8; 4])]) {
        let n = e.document(h).unwrap().node(id).unwrap().clone();
        let NodeKind::Raster {
            bounds: b,
            tiles: map,
        } = &n.kind
        else {
            panic!()
        };
        let mut out = BTreeMap::new();
        for (key, hash) in &map.tiles {
            out.insert(*key, e.require_tile(hash).unwrap().pixels.to_vec());
        }
        for (x, y, p) in pixels {
            assert!(b.contains(*x, *y));
            let key = TileKey {
                tx: x.div_euclid(T),
                ty: y.div_euclid(T),
            };
            let i = (y.rem_euclid(T) as usize * 256 + x.rem_euclid(T) as usize) * 4;
            write_exact(&mut out, key, i, *p);
        }
        let mut tiles = TileMap::new(PixelFormat::Rgba8);
        for (key, data) in out {
            if let Some(hash) = e.put_tile(TileData::new(PixelFormat::Rgba8, data).unwrap()) {
                tiles.tiles.insert(key, hash);
            }
        }
        let mut next = n.clone();
        next.kind = NodeKind::Raster { bounds: *b, tiles };
        let revision = e.document(h).unwrap().revision;
        e.transact(
            h,
            "paint",
            vec![Op::ReplaceNode { id, node: next }],
            revision,
        )
        .unwrap();
    }
    fn canvas(e: &mut Engine, h: DocHandle, edit: CanvasEdit) -> CanvasEditResult {
        let intent = edit.intent();
        let p = e.prepare_canvas_edit(h, edit).unwrap();
        let revision = e.document(h).unwrap().revision;
        if !p.ops.is_empty() {
            e.transact(h, intent, p.ops, revision).unwrap();
        }
        e.release_staged_tiles(p.stage);
        p.result
    }
    fn layer_edit(
        e: &mut Engine,
        h: DocHandle,
        edit: LayerEdit,
    ) -> Result<LayerEditResult, EngineError> {
        let intent = edit.intent();
        let p = e.prepare_layer_edit(h, edit)?;
        let revision = e.document(h).unwrap().revision;
        if !p.ops.is_empty() {
            e.transact(h, intent, p.ops, revision).unwrap();
        }
        e.release_staged_tiles(p.stage);
        Ok(p.result)
    }
    fn code(error: EngineError) -> &'static str {
        match error {
            EngineError::Operation { code, .. } => code,
            other => panic!("{other}"),
        }
    }
    /// Where the document places a node-local point.
    fn placed(e: &Engine, h: DocHandle, id: NodeId, x: f64, y: f64) -> (f64, f64) {
        let doc = e.document(h).unwrap();
        let mut m = Affine2::default();
        let mut next = Some(id);
        while let Some(n) = next {
            let node = doc.node(n).unwrap();
            m = node.props.transform.concat(m);
            next = node.parent;
        }
        let a = m.0;
        (a[0] * x + a[2] * y + a[4], a[1] * x + a[3] * y + a[5])
    }

    #[test]
    fn canvas_edits_move_the_root_and_keep_every_hash() {
        let (mut e, h) = doc(300, 200);
        let id = layer(&mut e, h, bounds(0, 0, 300, 200));
        paint(&mut e, h, id, &[(10, 20, [9, 8, 7, 255])]);
        let before = e.snapshot(h).unwrap();
        let tile_count = |e: &Engine| match &e.document(h).unwrap().node(id).unwrap().kind {
            NodeKind::Raster { tiles, .. } => tiles.clone(),
            _ => panic!(),
        };
        let tiles = tile_count(&e);

        let r = canvas(
            &mut e,
            h,
            CanvasEdit::Crop {
                x: 5,
                y: 10,
                width: 100,
                height: 50,
            },
        );
        assert_eq!((r.width, r.height), (100, 50));
        assert_eq!(placed(&e, h, id, 10.5, 20.5), (5.5, 10.5));
        assert_eq!(tile_count(&e), tiles, "no pixel is rewritten");
        e.undo(h).unwrap();
        assert_eq!(e.snapshot(h).unwrap(), before);

        canvas(&mut e, h, CanvasEdit::Rotate { quarter_turns: 1 });
        assert_eq!(e.document(h).unwrap().meta.width, 200);
        assert_eq!(placed(&e, h, id, 10.5, 20.5), (179.5, 10.5));
        canvas(&mut e, h, CanvasEdit::Rotate { quarter_turns: 3 });
        assert_eq!(placed(&e, h, id, 10.5, 20.5), (10.5, 20.5));
        e.undo(h).unwrap();
        e.undo(h).unwrap();
        assert_eq!(e.snapshot(h).unwrap(), before);

        canvas(
            &mut e,
            h,
            CanvasEdit::Flip {
                axis: FlipAxis::Horizontal,
            },
        );
        assert_eq!(placed(&e, h, id, 10.5, 20.5), (289.5, 20.5));
        e.undo(h).unwrap();
        canvas(
            &mut e,
            h,
            CanvasEdit::Resize {
                width: 600,
                height: 100,
            },
        );
        assert_eq!(placed(&e, h, id, 10.5, 20.5), (21.0, 10.25));
        assert_eq!(e.document(h).unwrap().meta.height, 100);

        let saved = e.save_package(h).unwrap();
        let (reopened, warnings) = e.open_package(&saved).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(e.snapshot(reopened).unwrap(), e.snapshot(h).unwrap());
    }

    #[test]
    fn canvas_edit_failures_change_nothing() {
        let (mut e, h) = doc(300, 200);
        let before = e.snapshot(h).unwrap();
        for bad in [
            CanvasEdit::Crop {
                x: 0,
                y: 0,
                width: 0,
                height: 4,
            },
            CanvasEdit::Crop {
                x: 0,
                y: 0,
                width: 65537,
                height: 4,
            },
            CanvasEdit::Resize {
                width: 0,
                height: 4,
            },
            CanvasEdit::Rotate { quarter_turns: 4 },
        ] {
            assert_eq!(
                code(e.prepare_canvas_edit(h, bad).err().unwrap()),
                "INVALID_ARGUMENT"
            );
        }
        assert_eq!(e.snapshot(h).unwrap(), before);
        let same = canvas(
            &mut e,
            h,
            CanvasEdit::Crop {
                x: 0,
                y: 0,
                width: 300,
                height: 200,
            },
        );
        assert!(same.no_op);
        assert_eq!(
            e.history_state(h).unwrap().0,
            0,
            "a no-op is not an Undo step"
        );
    }

    #[test]
    fn canvas_edits_rewrite_selection_and_unlinked_masks_and_scale_blur() {
        let (mut e, h) = doc(8, 4);
        let id = layer(&mut e, h, bounds(0, 0, 8, 4));
        let mut pixels = vec![0u8; 65536];
        pixels[3 * 256 + 7] = 200; // (7, 3)
        let hash = e
            .put_tile(TileData::new(PixelFormat::R8, pixels).unwrap())
            .unwrap();
        let mut selection = TileMap::new(PixelFormat::R8);
        selection.tiles.insert(TileKey { tx: 0, ty: 0 }, hash);
        let revision = e.document(h).unwrap().revision;
        e.transact(
            h,
            "selection",
            vec![
                Op::SetSelection {
                    selection: Some(selection.clone()),
                },
                Op::SetMask {
                    layer: id,
                    mask: Some(selection),
                },
                Op::SetProps {
                    id,
                    patch: PropPatch {
                        mask_linked: Some(false),
                        effects: Some(vec![Effect::GaussianBlur {
                            radius: 2.0,
                            selection: None,
                            extra: Default::default(),
                        }]),
                        ..Default::default()
                    },
                },
            ],
            revision,
        )
        .unwrap();
        let before = e.snapshot(h).unwrap();
        canvas(&mut e, h, CanvasEdit::Rotate { quarter_turns: 1 });
        let d = e.document(h).unwrap();
        let moved = d.selection.clone().unwrap();
        assert_eq!(
            read(&e, &moved, 0, 7).unwrap()[3],
            200,
            "(7,3) -> (4-1-3,7)"
        );
        let props = &d.node(id).unwrap().props;
        assert_eq!(props.mask_transform.0, [0.0, 1.0, -1.0, 0.0, 4.0, 0.0]);
        e.undo(h).unwrap();
        assert_eq!(e.snapshot(h).unwrap(), before);

        canvas(
            &mut e,
            h,
            CanvasEdit::Resize {
                width: 16,
                height: 8,
            },
        );
        let d = e.document(h).unwrap();
        assert!(matches!(
            d.node(id).unwrap().props.effects[0],
            Effect::GaussianBlur { radius, .. } if radius == 4.0
        ));
        assert!(!d.selection.as_ref().unwrap().tiles.is_empty());
        e.undo(h).unwrap();
        let refused = e.prepare_canvas_edit(
            h,
            CanvasEdit::Resize {
                width: 16,
                height: 4,
            },
        );
        assert_eq!(code(refused.err().unwrap()), "UNSUPPORTED");
        assert_eq!(e.snapshot(h).unwrap(), before);
    }

    #[test]
    fn merge_down_composites_and_refuses_what_it_cannot_keep() {
        let (mut e, h) = doc(600, 64);
        let lower = layer(&mut e, h, bounds(0, 0, 4, 4));
        let upper = layer(&mut e, h, bounds(0, 0, 4, 4));
        paint(&mut e, h, lower, &[(1, 1, [200, 0, 0, 255])]);
        paint(
            &mut e,
            h,
            upper,
            &[(1, 1, [0, 0, 200, 255]), (2, 2, [0, 200, 0, 255])],
        );
        // The upper layer sits 300 pixels right, across a tile boundary.
        let revision = e.document(h).unwrap().revision;
        e.transact(
            h,
            "move",
            vec![Op::SetProps {
                id: upper,
                patch: PropPatch {
                    transform: Some(Affine2([1.0, 0.0, 0.0, 1.0, 300.0, 0.0])),
                    opacity: Some(0.5),
                    ..Default::default()
                },
            }],
            revision,
        )
        .unwrap();
        let before = e.snapshot(h).unwrap();
        let r = layer_edit(&mut e, h, LayerEdit::MergeDown { node: upper }).unwrap();
        assert_eq!(r.node, lower);
        assert_eq!(r.bounds, bounds(0, 0, 304, 4));
        let d = e.document(h).unwrap();
        assert!(d.node(upper).is_err(), "upper layer is removed");
        assert_eq!(
            e.pick_color(h, lower, [1.0, 1.0]).unwrap().bytes,
            [200, 0, 0, 255]
        );
        let moved = e.pick_color(h, lower, [301.0, 1.0]).unwrap().bytes;
        assert_eq!(moved, [0, 0, 200, 128], "opacity is baked");
        e.undo(h).unwrap();
        assert_eq!(e.snapshot(h).unwrap(), before);

        // A blend mode, a mask or a fractional offset is refused, with nothing changed.
        let revision = e.document(h).unwrap().revision;
        e.transact(
            h,
            "fraction",
            vec![Op::SetProps {
                id: upper,
                patch: PropPatch {
                    transform: Some(Affine2([1.0, 0.0, 0.0, 1.0, 0.5, 0.0])),
                    ..Default::default()
                },
            }],
            revision,
        )
        .unwrap();
        let unchanged = e.snapshot(h).unwrap();
        let refused = layer_edit(&mut e, h, LayerEdit::MergeDown { node: upper });
        assert_eq!(code(refused.err().unwrap()), "UNSUPPORTED");
        assert_eq!(e.snapshot(h).unwrap(), unchanged);
        let bottom = layer_edit(&mut e, h, LayerEdit::MergeDown { node: lower });
        assert_eq!(code(bottom.err().unwrap()), "INVALID_ARGUMENT");
    }

    #[test]
    fn rasterize_bakes_offsets_and_flattens_raster_groups() {
        let (mut e, h) = doc(600, 64);
        let id = layer(&mut e, h, bounds(0, 0, 4, 4));
        paint(&mut e, h, id, &[(1, 1, [1, 2, 3, 255])]);
        assert!(
            layer_edit(&mut e, h, LayerEdit::Rasterize { node: id })
                .unwrap()
                .no_op
        );
        let revision = e.document(h).unwrap().revision;
        e.transact(
            h,
            "move",
            vec![Op::SetProps {
                id,
                patch: PropPatch {
                    transform: Some(Affine2([1.0, 0.0, 0.0, 1.0, 255.0, -1.0])),
                    ..Default::default()
                },
            }],
            revision,
        )
        .unwrap();
        let r = layer_edit(&mut e, h, LayerEdit::Rasterize { node: id }).unwrap();
        assert_eq!(r.bounds, bounds(255, -1, 4, 4));
        assert_eq!(
            e.pick_color(h, id, [256.0, 0.0]).unwrap().bytes,
            [1, 2, 3, 255]
        );
        assert_eq!(placed(&e, h, id, 256.5, 0.5), (256.5, 0.5));
        e.undo(h).unwrap();

        let group = Node::group("g");
        let gid = group.id;
        let root = e.document(h).unwrap().root;
        let revision = e.document(h).unwrap().revision;
        e.transact(
            h,
            "group",
            vec![
                Op::InsertNode {
                    node: group,
                    parent: root,
                    index: 1,
                },
                Op::MoveNode {
                    id,
                    parent: gid,
                    index: 0,
                },
            ],
            revision,
        )
        .unwrap();
        let before = e.snapshot(h).unwrap();
        let r = layer_edit(&mut e, h, LayerEdit::Rasterize { node: gid }).unwrap();
        assert_eq!(r.bounds, bounds(255, -1, 4, 4));
        let d = e.document(h).unwrap();
        assert!(d.node(id).is_err());
        assert!(matches!(d.node(gid).unwrap().kind, NodeKind::Raster { .. }));
        assert_eq!(
            e.pick_color(h, gid, [256.0, 0.0]).unwrap().bytes,
            [1, 2, 3, 255]
        );
        e.undo(h).unwrap();
        assert_eq!(e.snapshot(h).unwrap(), before);

        let vector = Node::vector("v", vec![]);
        let vid = vector.id;
        let revision = e.document(h).unwrap().revision;
        e.transact(
            h,
            "vector",
            vec![Op::InsertNode {
                node: vector,
                parent: gid,
                index: 1,
            }],
            revision,
        )
        .unwrap();
        let refused = layer_edit(&mut e, h, LayerEdit::Rasterize { node: gid });
        assert_eq!(code(refused.err().unwrap()), "UNSUPPORTED");
        let refused = layer_edit(&mut e, h, LayerEdit::Rasterize { node: vid });
        assert_eq!(code(refused.err().unwrap()), "UNSUPPORTED");
    }

    #[test]
    fn histogram_counts_every_pixel_in_range_with_or_without_tiles() {
        let (mut e, h) = doc(600, 600);
        let id = layer(&mut e, h, bounds(250, 0, 10, 2));
        paint(
            &mut e,
            h,
            id,
            &[(255, 0, [7, 8, 9, 255]), (256, 0, [7, 0, 0, 128])],
        );
        let all = e.image_histogram(h, id, None).unwrap();
        assert_eq!(all.pixels, 20);
        assert_eq!(
            (all.r[7], all.g[8], all.b[9], all.a[255], all.a[128]),
            (2, 1, 1, 1, 1)
        );
        assert_eq!(
            all.a[0], 18,
            "transparent pixels and padding outside the range"
        );
        let part = e
            .image_histogram(h, id, Some(bounds(256, 0, 1, 1)))
            .unwrap();
        assert_eq!((part.pixels, part.a[128]), (1, 1));
        assert!(e.image_histogram(h, id, Some(bounds(0, 0, 1, 1))).is_err());
    }

    fn apply(e: &mut Engine, h: DocHandle, edit: ImageEdit) -> crate::image::ImageEditResult {
        let intent = edit.intent();
        let p = e.prepare_image_edit(h, edit).unwrap();
        e.apply_image_edit(h, p, intent).unwrap()
    }
    fn stroke(points: Vec<[f64; 2]>, radius: f64) -> Stroke {
        Stroke {
            points,
            radius,
            hardness: 1.0,
        }
    }

    #[test]
    fn clone_stamp_crosses_tiles_writes_new_tiles_and_stays_in_bounds() {
        let (mut e, h) = doc(600, 64);
        let id = layer(&mut e, h, bounds(0, 0, 400, 8));
        paint(
            &mut e,
            h,
            id,
            &[(2, 2, [50, 60, 70, 255]), (3, 2, [1, 1, 1, 255])],
        );
        let before = e.snapshot(h).unwrap();
        let r = apply(
            &mut e,
            h,
            ImageEdit::CloneStamp {
                node: id,
                source: [2.0, 2.0],
                stroke: stroke(vec![[300.0, 2.0]], 2.0),
                opacity: 1.0,
                selection: SelectionMode::Ignore,
            },
        );
        assert_eq!(r.changed_tiles, 1, "a tile that did not exist is created");
        assert_eq!(
            e.pick_color(h, id, [300.0, 2.0]).unwrap().bytes,
            [50, 60, 70, 255]
        );
        assert_eq!(
            e.pick_color(h, id, [301.0, 2.0]).unwrap().bytes,
            [1, 1, 1, 255]
        );
        e.undo(h).unwrap();
        assert_eq!(e.snapshot(h).unwrap(), before);

        // Near the edge, the tip stays inside the image range and the tile stays valid.
        apply(
            &mut e,
            h,
            ImageEdit::CloneStamp {
                node: id,
                source: [2.0, 2.0],
                stroke: stroke(vec![[399.0, 7.0]], 3.0),
                opacity: 1.0,
                selection: SelectionMode::Ignore,
            },
        );
        let saved = e.save_package(h).unwrap();
        assert!(Engine::new().open_package(&saved).is_ok());
        e.undo(h).unwrap();

        let refused = e.prepare_image_edit(
            h,
            ImageEdit::CloneStamp {
                node: id,
                source: [0.0, 0.0],
                stroke: stroke(vec![], 2.0),
                opacity: 1.0,
                selection: SelectionMode::Ignore,
            },
        );
        assert_eq!(code(refused.err().unwrap()), "INVALID_ARGUMENT");
        assert_eq!(e.snapshot(h).unwrap(), before);
    }

    #[test]
    fn clone_stamp_reads_sources_before_the_edit_and_overlaps_once() {
        let (mut e, h) = doc(64, 8);
        let id = layer(&mut e, h, bounds(0, 0, 64, 8));
        paint(
            &mut e,
            h,
            id,
            &[(0, 0, [10, 20, 30, 128]), (1, 0, [40, 50, 60, 255])],
        );
        // Every destination is one pixel right of its source, and the stroke passes
        // over its own output. Reading the output would repeat pixel (0,0).
        apply(
            &mut e,
            h,
            ImageEdit::CloneStamp {
                node: id,
                source: [0.0, 0.0],
                stroke: stroke(vec![[1.0, 0.0], [3.0, 0.0]], 1.0),
                opacity: 1.0,
                selection: SelectionMode::Ignore,
            },
        );
        assert_eq!(
            e.pick_color(h, id, [2.0, 0.0]).unwrap().bytes,
            [40, 50, 60, 255],
            "pixel 1 is copied from before the edit"
        );
    }

    #[test]
    fn smudge_carries_pixels_and_respects_selection() {
        let (mut e, h) = doc(32, 8);
        let id = layer(&mut e, h, bounds(0, 0, 32, 8));
        let mut pixels = vec![];
        for x in 0..4 {
            pixels.push((x, 4, [255, 0, 0, 255]));
        }
        paint(&mut e, h, id, &pixels);
        let before = e.snapshot(h).unwrap();
        let smudge = |strength: f64, selection| ImageEdit::Smudge {
            node: id,
            stroke: stroke(vec![[1.0, 4.0], [12.0, 4.0]], 2.0),
            strength,
            selection,
        };
        apply(&mut e, h, smudge(1.0, SelectionMode::Ignore));
        let far = e.pick_color(h, id, [9.0, 4.0]).unwrap().bytes;
        assert_eq!(
            far,
            [255, 0, 0, 255],
            "full strength carries the color along"
        );
        e.undo(h).unwrap();
        apply(&mut e, h, smudge(0.5, SelectionMode::Ignore));
        let weak = e.pick_color(h, id, [9.0, 4.0]).unwrap().bytes;
        assert!(weak[3] < 255, "half strength fades: {weak:?}");
        e.undo(h).unwrap();
        assert!(apply(&mut e, h, smudge(0.0, SelectionMode::Ignore)).no_op);
        assert_eq!(e.snapshot(h).unwrap(), before);

        // A selection elsewhere keeps the stroke out.
        let mut sel = vec![0u8; 65536];
        sel[7 * 256 + 31] = 255;
        let hash = e
            .put_tile(TileData::new(PixelFormat::R8, sel).unwrap())
            .unwrap();
        let mut map = TileMap::new(PixelFormat::R8);
        map.tiles.insert(TileKey { tx: 0, ty: 0 }, hash);
        let revision = e.document(h).unwrap().revision;
        e.transact(
            h,
            "sel",
            vec![Op::SetSelection {
                selection: Some(map),
            }],
            revision,
        )
        .unwrap();
        let unchanged = e.snapshot(h).unwrap();
        let refused = e.prepare_image_edit(h, smudge(1.0, SelectionMode::Current));
        assert_eq!(code(refused.err().unwrap()), "EMPTY_SELECTION");
        assert_eq!(e.snapshot(h).unwrap(), unchanged);
    }
}
