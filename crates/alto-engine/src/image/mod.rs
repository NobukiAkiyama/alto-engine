//! CPU-owned, reversible image edits. Prepared output stays rooted until applied or released.
use crate::{DocHandle, Engine, EngineError};
use alto_core::{Affine2, NodeId, PixelFormat, TileKey};
use alto_doc::{ImageBounds, NodeKind, Op};
use alto_raster::{TileData, TileMap};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
mod pixels;
mod resample;
mod selection;
mod stroke;
use pixels::*;
use resample::*;
use selection::*;
pub use stroke::Stroke;
#[cfg(test)]
mod reference_validation_tests;
#[cfg(test)]
mod tests;

pub(crate) fn err(code: &'static str, message: &str) -> EngineError {
    EngineError::Operation {
        code,
        message: message.into(),
    }
}
pub(crate) const EDIT_BUDGET: u64 = 256 * 1024 * 1024;
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFilter {
    Nearest,
    #[default]
    Bilinear,
    /// Sharper than bilinear when enlarging or mildly shrinking.
    CatmullRom,
    /// The exact average of the source area under each new pixel, for large reductions.
    Area,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    #[default]
    Current,
    Ignore,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GradientStop {
    pub offset: f64,
    pub color: [f64; 4],
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GradientKind {
    Linear,
    Radial,
}
fn one() -> f64 {
    1.0
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GradientOptions {
    pub kind: GradientKind,
    pub start: [f64; 2],
    pub end: [f64; 2],
    pub stops: Vec<GradientStop>,
    #[serde(default = "one")]
    pub opacity: f64,
    #[serde(default)]
    pub selection: SelectionMode,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageEdit {
    Crop {
        node: NodeId,
        bounds: ImageBounds,
    },
    Resize {
        node: NodeId,
        width: u32,
        height: u32,
        #[serde(default)]
        filter: ImageFilter,
    },
    Rotate {
        node: NodeId,
        quarter_turns: u8,
    },
    Flip {
        node: NodeId,
        axis: FlipAxis,
    },
    Gradient {
        node: NodeId,
        options: GradientOptions,
    },
    Fill {
        node: NodeId,
        point: [f64; 2],
        color: [f64; 4],
        #[serde(default)]
        tolerance: u8,
        #[serde(default)]
        selection: SelectionMode,
    },
    /// Copies the same layer's pixels along a stroke. `source` is the point sampled
    /// for the stroke's first point. Sources are read before the edit, so a stroke
    /// that crosses its own source does not copy its own output.
    CloneStamp {
        node: NodeId,
        source: [f64; 2],
        stroke: Stroke,
        #[serde(default = "one")]
        opacity: f64,
        #[serde(default)]
        selection: SelectionMode,
    },
    /// Carries the pixels under the brush along a stroke.
    Smudge {
        node: NodeId,
        stroke: Stroke,
        #[serde(default = "half")]
        strength: f64,
        #[serde(default)]
        selection: SelectionMode,
    },
}
fn half() -> f64 {
    0.5
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlipAxis {
    Horizontal,
    Vertical,
}
impl ImageEdit {
    fn node(&self) -> NodeId {
        match self {
            Self::Crop { node, .. }
            | Self::Resize { node, .. }
            | Self::Rotate { node, .. }
            | Self::Flip { node, .. }
            | Self::Gradient { node, .. }
            | Self::Fill { node, .. }
            | Self::CloneStamp { node, .. }
            | Self::Smudge { node, .. } => *node,
        }
    }
    pub fn intent(&self) -> &'static str {
        match self {
            Self::Crop { .. } => "image.crop",
            Self::Resize { .. } => "image.resize",
            Self::Rotate { .. } => "image.rotate",
            Self::Flip { .. } => "image.flip",
            Self::Gradient { .. } => "image.gradient",
            Self::Fill { .. } => "image.fill",
            Self::CloneStamp { .. } => "image.clone_stamp",
            Self::Smudge { .. } => "image.smudge",
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct ImageEditResult {
    pub node: NodeId,
    pub bounds: ImageBounds,
    pub changed_tiles: usize,
    pub no_op: bool,
}
pub struct PreparedImageEdit {
    document: DocHandle,
    pub stage: u64,
    pub revision: u64,
    pub ops: Vec<Op>,
    pub result: ImageEditResult,
}
#[derive(Debug, Serialize)]
pub struct PickedColor {
    pub bytes: [u8; 4],
    pub color: [f64; 4],
}
fn point(p: [f64; 2]) -> Result<[i32; 2], EngineError> {
    if p.iter()
        .any(|v| !v.is_finite() || v.floor() < i32::MIN as f64 || v.floor() > i32::MAX as f64)
    {
        return Err(err("INVALID_ARGUMENT", "invalid pixel coordinates"));
    }
    Ok([p[0].floor() as i32, p[1].floor() as i32])
}
fn color(c: [f64; 4]) -> Result<(), EngineError> {
    if c.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
        Err(err("INVALID_ARGUMENT", "invalid srgb-straight color"))
    } else {
        Ok(())
    }
}
pub(crate) fn read(engine: &Engine, map: &TileMap, x: i32, y: i32) -> Result<[u8; 4], EngineError> {
    let key = TileKey {
        tx: x.div_euclid(256),
        ty: y.div_euclid(256),
    };
    let Some(hash) = map.tiles.get(&key) else {
        return Ok([0; 4]);
    };
    let tile = engine.require_tile(hash)?;
    if tile.format != map.format {
        return Err(err("CORRUPT_DATA", "tile format mismatch"));
    }
    let i = (y.rem_euclid(256) as usize * 256 + x.rem_euclid(256) as usize)
        * map.format.bytes_per_pixel();
    Ok(if map.format == PixelFormat::R8 {
        [0, 0, 0, tile.pixels[i]]
    } else {
        tile.pixels[i..i + 4].try_into().unwrap()
    })
}
pub(crate) fn world(
    engine: &Engine,
    h: DocHandle,
    id: NodeId,
) -> Result<alto_core::FixedAffine, EngineError> {
    let doc = engine.document(h)?;
    let mut chain = vec![];
    let mut next = Some(id);
    while let Some(id) = next {
        let n = doc.node(id)?;
        chain.push(n.props.transform);
        next = n.parent;
    }
    let mut out = alto_core::FixedAffine::IDENTITY;
    for local in chain.into_iter().rev() {
        let local = alto_core::FixedAffine::from_affine(local)
            .ok_or_else(|| err("INVALID_ARGUMENT", "image transform out of range"))?;
        out = out.concat(local).ok_or_else(|| {
            err(
                "INVALID_ARGUMENT",
                "image transform composition out of range",
            )
        })?;
    }
    Ok(out)
}
pub(crate) fn over(dst: [u8; 4], src: [f64; 4], coverage: u8) -> [u8; 4] {
    // Q14 premultiplied normal source-over, followed by straight RGBA8 encoding.
    let q = 16384u64;
    let a = (src[3] * coverage as f64 / 255.0 * q as f64).round() as u64;
    if a == 0 {
        return dst;
    }
    let da = (u64::from(dst[3]) * q + 127) / 255;
    let oa = a + (da * (q - a) + q / 2) / q;
    let mut out = [0; 4];
    out[3] = ((oa * 255 + q / 2) / q) as u8;
    for c in 0..3 {
        let s = (src[c] * q as f64).round() as u64;
        let dp = (u64::from(dst[c]) * u64::from(dst[3]) * q + 32512) / 65025;
        let p = (s * a + q / 2) / q + (dp * (q - a) + q / 2) / q;
        out[c] = ((p * 255 + oa / 2) / oa).min(255) as u8;
    }
    out
}
impl Engine {
    pub fn pick_color(
        &self,
        h: DocHandle,
        id: NodeId,
        p: [f64; 2],
    ) -> Result<PickedColor, EngineError> {
        let n = self.document(h)?.node(id)?;
        let NodeKind::Raster { bounds, tiles } = &n.kind else {
            return Err(err("INVALID_ARGUMENT", "raster required"));
        };
        let [x, y] = point(p)?;
        if !bounds.contains(x, y) {
            return Err(err("NOT_FOUND", "pixel outside image"));
        }
        let bytes = read(self, tiles, x, y)?;
        Ok(PickedColor {
            bytes,
            color: bytes.map(|b| b as f64 / 255.0),
        })
    }
    pub fn prepare_image_edit(
        &mut self,
        h: DocHandle,
        edit: ImageEdit,
    ) -> Result<PreparedImageEdit, EngineError> {
        let stage = self.begin_tile_stage();
        let result = self.prepare_image_inner(h, edit, stage);
        if result.is_err() {
            self.release_staged_tiles(stage);
        }
        result
    }
    pub fn apply_image_edit(
        &mut self,
        h: DocHandle,
        prepared: PreparedImageEdit,
        intent: &str,
    ) -> Result<ImageEditResult, EngineError> {
        if prepared.document != h {
            self.release_staged_tiles(prepared.stage);
            return Err(err(
                "CONFLICT",
                "prepared image edit belongs to another document",
            ));
        }
        let result = self
            .transact(h, intent, prepared.ops, prepared.revision)
            .map(|_| prepared.result);
        self.release_staged_tiles(prepared.stage);
        result
    }
    fn prepare_image_inner(
        &mut self,
        h: DocHandle,
        mut edit: ImageEdit,
        stage: u64,
    ) -> Result<PreparedImageEdit, EngineError> {
        let id = edit.node();
        let doc = self.document(h)?;
        let mut node = doc.node(id)?.clone();
        let revision = doc.revision;
        self.validate_node_content(&node)?;
        let mut next = Some(id);
        while let Some(id) = next {
            let n = doc.node(id)?;
            if n.props.locked {
                return Err(err("INVALID_ARGUMENT", "layer is locked"));
            }
            next = n.parent;
        }
        let NodeKind::Raster {
            bounds: old,
            tiles: map,
        } = &node.kind
        else {
            return Err(err("INVALID_ARGUMENT", "raster required"));
        };
        let old = *old;
        let map = map.clone();
        let mut bounds = old;
        let mut transform = Affine2::default();
        let mut geometry = false;
        let selected = match &edit {
            ImageEdit::Gradient { options, .. } => options.selection,
            ImageEdit::Fill { selection, .. }
            | ImageEdit::CloneStamp { selection, .. }
            | ImageEdit::Smudge { selection, .. } => *selection,
            _ => SelectionMode::Ignore,
        };
        let mask = if matches!(selected, SelectionMode::Current) {
            doc.selection.clone()
        } else {
            None
        };
        let m = world(self, h, id)?;
        if mask.as_ref().is_some_and(|m| m.tiles.is_empty()) {
            return Err(err("EMPTY_SELECTION", "selection is empty"));
        }
        match &mut edit {
            ImageEdit::Crop { bounds: b, .. } => {
                b.validate()?;
                if b.x < old.x
                    || b.y < old.y
                    || b.right() > old.right()
                    || b.bottom() > old.bottom()
                {
                    return Err(err("INVALID_ARGUMENT", "crop must lie inside image bounds"));
                }
                bounds = *b;
            }
            ImageEdit::Resize { width, height, .. } => {
                bounds.width = *width;
                bounds.height = *height;
                bounds.validate()?;
                let sx = *width as f64 / old.width as f64;
                let sy = *height as f64 / old.height as f64;
                transform = Affine2([
                    sx,
                    0.0,
                    0.0,
                    sy,
                    old.x as f64 * (1.0 - sx),
                    old.y as f64 * (1.0 - sy),
                ]);
                geometry = true;
            }
            ImageEdit::Rotate {
                quarter_turns: q, ..
            } => {
                if !(1..=3).contains(q) {
                    return Err(err("INVALID_ARGUMENT", "quarterTurns must be 1, 2, or 3"));
                }
                if *q % 2 == 1 {
                    bounds.width = old.height;
                    bounds.height = old.width;
                }
                bounds.validate()?;
                let x = old.x as f64;
                let y = old.y as f64;
                let w = old.width as f64;
                let ht = old.height as f64;
                transform = match q {
                    1 => Affine2([0.0, 1.0, -1.0, 0.0, x + y + ht, y - x]),
                    2 => Affine2([-1.0, 0.0, 0.0, -1.0, 2.0 * x + w, 2.0 * y + ht]),
                    _ => Affine2([0.0, -1.0, 1.0, 0.0, x - y, x + y + w]),
                };
                geometry = true;
            }
            ImageEdit::Flip { axis, .. } => {
                transform = match axis {
                    FlipAxis::Horizontal => Affine2([
                        -1.0,
                        0.0,
                        0.0,
                        1.0,
                        2.0 * old.x as f64 + old.width as f64,
                        0.0,
                    ]),
                    FlipAxis::Vertical => Affine2([
                        1.0,
                        0.0,
                        0.0,
                        -1.0,
                        0.0,
                        2.0 * old.y as f64 + old.height as f64,
                    ]),
                };
                geometry = true;
            }
            ImageEdit::Gradient { options, .. } => {
                if options
                    .start
                    .iter()
                    .chain(options.end.iter())
                    .any(|v| !v.is_finite() || v.abs() > i32::MAX as f64)
                    || options.start == options.end
                    || !options.opacity.is_finite()
                    || !(0.0..=1.0).contains(&options.opacity)
                    || !(2..=16).contains(&options.stops.len())
                {
                    return Err(err("INVALID_ARGUMENT", "invalid gradient"));
                }
                for stop in &options.stops {
                    color(stop.color)?;
                    if !stop.offset.is_finite() || !(0.0..=1.0).contains(&stop.offset) {
                        return Err(err("INVALID_ARGUMENT", "invalid gradient offset"));
                    }
                }
                options.stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
                if options.stops.windows(2).any(|w| w[0].offset == w[1].offset) {
                    return Err(err("INVALID_ARGUMENT", "duplicate gradient offsets"));
                }
            }
            ImageEdit::Fill {
                point: p, color: c, ..
            } => {
                color(*c)?;
                let [x, y] = point(*p)?;
                if !old.contains(x, y) {
                    return Err(err("NOT_FOUND", "fill point outside image"));
                }
                if selection(self, mask.as_ref(), m, x, y)? == 0 {
                    return Err(err("EMPTY_SELECTION", "fill point outside selection"));
                }
            }
            ImageEdit::CloneStamp { .. } | ImageEdit::Smudge { .. } => {
                let edit = edit.clone();
                return self.prepare_stroke(h, id, node, edit, mask, m, stage, revision);
            }
        }
        let unchanged_geometry = match &edit {
            ImageEdit::Resize { width, height, .. } => *width == old.width && *height == old.height,
            ImageEdit::Crop { bounds, .. } => *bounds == old,
            _ => false,
        };
        let transparent = mask.is_none()
            && match &edit {
                ImageEdit::Fill { color, .. } => color[3] == 0.0,
                ImageEdit::Gradient { options, .. } => {
                    options.opacity == 0.0 || options.stops.iter().all(|s| s.color[3] == 0.0)
                }
                _ => false,
            };
        if unchanged_geometry || transparent {
            return Ok(PreparedImageEdit {
                document: h,
                stage,
                revision,
                ops: vec![],
                result: ImageEditResult {
                    node: id,
                    bounds: old,
                    changed_tiles: 0,
                    no_op: true,
                },
            });
        }
        let cols = i64::from((bounds.right() - 1).div_euclid(256))
            - i64::from(bounds.x.div_euclid(256))
            + 1;
        let rows = i64::from((bounds.bottom() - 1).div_euclid(256))
            - i64::from(bounds.y.div_euclid(256))
            + 1;
        let cost = u64::from(old.width) * u64::from(old.height) * 4
            + cols as u64 * rows as u64 * 262144
            + map.tiles.len() as u64 * 262144
            + node
                .props
                .mask
                .as_ref()
                .map_or(0, |m| m.tiles.len() as u64 * 65536);
        if cost > EDIT_BUDGET {
            return Err(err(
                "RESOURCE_LIMIT",
                "image edit working-set budget exceeded",
            ));
        }
        let mut out = BTreeMap::new();
        let mut selected_pixel = mask.is_none();
        if let ImageEdit::Fill {
            point: p,
            color: c,
            tolerance,
            ..
        } = &edit
        {
            // Scanline seed fill; membership always compares to the original seed color.
            let [sx, sy] = point(*p)?;
            let seed = read(self, &map, sx, sy)?;
            let premul = |p: [u8; 4]| -> [u8; 4] {
                [
                    (u16::from(p[0]) * u16::from(p[3]) + 127).div_euclid(255) as u8,
                    (u16::from(p[1]) * u16::from(p[3]) + 127).div_euclid(255) as u8,
                    (u16::from(p[2]) * u16::from(p[3]) + 127).div_euclid(255) as u8,
                    p[3],
                ]
            };
            let seed = premul(seed);
            let mut seen = vec![0u8; (old.width as usize * old.height as usize).div_ceil(8)];
            let mut stack = vec![(sx, sy)];
            let mut count = 0usize;
            let mut touched = BTreeSet::new();
            let index = |x: i32, y: i32| {
                (y as i64 - old.y as i64) as usize * old.width as usize
                    + (x as i64 - old.x as i64) as usize
            };
            let eligible = |x: i32, y: i32, seen: &[u8]| -> Result<bool, EngineError> {
                if !old.contains(x, y) {
                    return Ok(false);
                }
                let i = index(x, y);
                if seen[i / 8] & (1 << (i % 8)) != 0 {
                    return Ok(false);
                }
                let p = premul(read(self, &map, x, y)?);
                Ok((0..4).all(|i| p[i].abs_diff(seed[i]) <= *tolerance)
                    && selection(self, mask.as_ref(), m, x, y)? > 0)
            };
            while let Some((x, y)) = stack.pop() {
                if !eligible(x, y, &seen)? {
                    continue;
                }
                let mut left = x;
                while left > old.x && eligible(left - 1, y, &seen)? {
                    left -= 1;
                }
                let mut x = left;
                let (mut above, mut below) = (false, false);
                while x < old.right() && eligible(x, y, &seen)? {
                    let i = index(x, y);
                    seen[i / 8] |= 1 << (i % 8);
                    count += 1;
                    touched.insert(TileKey {
                        tx: x.div_euclid(256),
                        ty: y.div_euclid(256),
                    });
                    if count > 16 * 1024 * 1024
                        || touched.len() > 4096
                        || stack.len() as u64 * 8 + seen.len() as u64 + out.len() as u64 * 262144
                            > EDIT_BUDGET
                    {
                        return Err(err("RESOURCE_LIMIT", "flood fill limit exceeded"));
                    }
                    let p = over(
                        read(self, &map, x, y)?,
                        *c,
                        selection(self, mask.as_ref(), m, x, y)?,
                    );
                    write(&mut out, x, y, p);
                    for (dy, span) in [(-1, &mut above), (1, &mut below)] {
                        let yes = if (dy < 0 && y == i32::MIN) || (dy > 0 && y == i32::MAX) {
                            false
                        } else {
                            eligible(x, y + dy, &seen)?
                        };
                        if yes && !*span {
                            stack.push((x, y + dy));
                        }
                        *span = yes;
                    }
                    x += 1;
                }
            }
            // Preserve every unvisited pixel, including outside the connected component.
            for (key, hash) in &map.tiles {
                let tile = self.require_tile(hash)?;
                let ox = key.tx * 256;
                let oy = key.ty * 256;
                for v in 0..256 {
                    for u in 0..256 {
                        let x = ox + u;
                        let y = oy + v;
                        if old.contains(x, y) {
                            let i = index(x, y);
                            if seen[i / 8] & (1 << (i % 8)) == 0 {
                                let k = (v as usize * 256 + u as usize) * 4;
                                write(&mut out, x, y, tile.pixels[k..k + 4].try_into().unwrap());
                            }
                        }
                    }
                }
            }
        } else {
            let mut reader = Reader::new(self, &map);
            let mut writer = Writer::default();
            for y in bounds.y..bounds.bottom() {
                for x in bounds.x..bounds.right() {
                    let u = x - bounds.x;
                    let v = y - bounds.y;
                    let p = match &edit {
                        ImageEdit::Crop { .. } => reader.get(x, y)?,
                        ImageEdit::Resize { filter, .. } => {
                            resize_pixel(&mut reader, old, bounds, x, y, *filter)?
                        }
                        ImageEdit::Rotate {
                            quarter_turns: q, ..
                        } => {
                            let (a, b) = match q {
                                1 => (v, old.height as i32 - 1 - u),
                                2 => (old.width as i32 - 1 - u, old.height as i32 - 1 - v),
                                _ => (old.width as i32 - 1 - v, u),
                            };
                            reader.get(old.x + a, old.y + b)?
                        }
                        ImageEdit::Flip { axis, .. } => {
                            let (a, b) = match axis {
                                FlipAxis::Horizontal => (old.width as i32 - 1 - u, v),
                                FlipAxis::Vertical => (u, old.height as i32 - 1 - v),
                            };
                            reader.get(old.x + a, old.y + b)?
                        }
                        ImageEdit::Gradient { options, .. } => {
                            let coverage = selection(self, mask.as_ref(), m, x, y)?;
                            selected_pixel |= coverage > 0;
                            over(
                                reader.get(x, y)?,
                                gradient(options, [x as f64 + 0.5, y as f64 + 0.5]),
                                coverage,
                            )
                        }
                        ImageEdit::Fill { .. }
                        | ImageEdit::CloneStamp { .. }
                        | ImageEdit::Smudge { .. } => unreachable!(),
                    };
                    writer.put(x, y, p);
                }
            }
            out = writer.finish();
        }
        if matches!(&edit, ImageEdit::Gradient { .. }) && !selected_pixel {
            return Err(err("EMPTY_SELECTION", "selection does not cover the image"));
        }
        if geometry {
            if node.props.mask_linked && node.props.mask.is_some() {
                node.props.mask_transform = transform.concat(node.props.mask_transform);
            }
            if let Some(mask) = &mut node.props.vector_mask {
                mask.transform = transform.concat(mask.transform);
                mask.validate()
                    .map_err(|e| err("UNSUPPORTED", &e.to_string()))?;
            }
            if let Some(path) = &mut node.props.clip_path {
                path.transform = transform.concat(path.transform);
                path.validate()
                    .map_err(|e| err("UNSUPPORTED", &e.to_string()))?;
            }
            // Effect selections are node-local R8 maps without a transform of their own,
            // so their pixels move with the image's.
            let remap = match &edit {
                ImageEdit::Rotate { quarter_turns, .. } => Remap::Rotate(*quarter_turns),
                ImageEdit::Flip { axis, .. } => Remap::Flip(*axis),
                _ => Remap::Scale,
            };
            let mut effects = node.props.effects.clone();
            for effect in &mut effects {
                let Some(selection) = effect.selection_mut().as_mut() else {
                    continue;
                };
                let out = remap_selection(self, selection, remap, old, bounds)?;
                let mut moved = TileMap::new(PixelFormat::R8);
                for (key, pixels) in out {
                    if let Some(hash) =
                        self.stage_tile(stage, TileData::new(PixelFormat::R8, pixels)?)?
                    {
                        moved.tiles.insert(key, hash);
                    }
                }
                // An empty map still limits the effect to no pixels; `None` would mean all.
                *selection = moved;
            }
            node.props.effects = effects;
        }
        let mut tiles = TileMap::new(PixelFormat::Rgba8);
        for (key, pixels) in out {
            if let Some(hash) =
                self.stage_tile(stage, TileData::new(PixelFormat::Rgba8, pixels)?)?
            {
                tiles.tiles.insert(key, hash);
            }
        }
        let changed_tiles = map
            .tiles
            .keys()
            .chain(tiles.tiles.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|k| map.tiles.get(k) != tiles.tiles.get(k))
            .count();
        node.kind = NodeKind::Raster { bounds, tiles };
        let no_op = self.document(h)?.node(id)? == &node;
        let ops = if no_op {
            vec![]
        } else {
            vec![Op::ReplaceNode { id, node }]
        };
        Ok(PreparedImageEdit {
            document: h,
            stage,
            revision,
            ops,
            result: ImageEditResult {
                node: id,
                bounds,
                changed_tiles,
                no_op,
            },
        })
    }
}
