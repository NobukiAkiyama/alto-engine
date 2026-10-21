//! Selection and mask commands operate on canonical R8 content, never on GPU caches.
use super::*;
use alto_core::{NodeId, PixelFormat, TileKey};
use alto_doc::{DocError, NodeKind, PropPatch};
use alto_gpu::{REffect, REffectKind, RMask};
use alto_raster::{TileData, TileMap};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub fn render_mask(map: &TileMap, linked: bool) -> RMask {
    RMask {
        linked,
        transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        tiles: map
            .tiles
            .iter()
            .map(|(key, hash)| RenderTile {
                key: *key,
                hash: *hash,
            })
            .collect(),
    }
}
pub fn render_effect(effect: &alto_doc::Effect) -> REffect {
    let f = |n: f64| n as f32;
    let kind = match effect {
        alto_doc::Effect::GaussianBlur { radius, .. } => {
            REffectKind::GaussianBlur { radius: f(*radius) }
        }
        alto_doc::Effect::Levels {
            in_black,
            in_white,
            gamma,
            out_black,
            out_white,
            ..
        } => REffectKind::Levels {
            in_black: f(*in_black),
            in_white: f(*in_white),
            gamma: f(*gamma),
            out_black: f(*out_black),
            out_white: f(*out_white),
        },
        alto_doc::Effect::Curves { points, .. } => REffectKind::Curves {
            points: points.iter().map(|p| (f(p.x), f(p.y))).collect(),
        },
        alto_doc::Effect::HueSaturation {
            hue,
            saturation,
            lightness,
            ..
        } => REffectKind::HueSaturation {
            hue: f(*hue),
            saturation: f(*saturation),
            lightness: f(*lightness),
        },
        alto_doc::Effect::Invert { .. } => REffectKind::Invert,
        alto_doc::Effect::ColorAdjust {
            brightness,
            contrast,
            saturation,
            ..
        } => REffectKind::ColorAdjust {
            brightness: f(*brightness),
            contrast: f(*contrast),
            saturation: f(*saturation),
        },
    };
    REffect {
        kind,
        selection: effect.selection().map(|m| render_mask(m, true)),
    }
}
pub fn effect_hashes(effect: &alto_doc::Effect, hashes: &mut BTreeSet<ContentHash>) {
    if let Some(m) = effect.selection() {
        hashes.extend(m.tiles.values().copied());
    }
}
fn invalid(message: &str) -> EngineError {
    DocError::Invalid(message.into()).into()
}
fn empty() -> TileMap {
    TileMap {
        format: PixelFormat::R8,
        tiles: BTreeMap::new(),
    }
}
type Decoded = BTreeMap<TileKey, Arc<TileData>>;
pub fn decode(map: &TileMap, store: &dyn TileStore) -> Result<Decoded, EngineError> {
    let mut decoded = Decoded::new();
    for (key, hash) in &map.tiles {
        if let Some(tile) = store
            .get_checked(hash)
            .map_err(alto_store::StoreError::Corrupt)?
        {
            decoded.insert(*key, tile);
        }
    }
    Ok(decoded)
}
fn generated_selection(
    engine: &mut Engine,
    handle: DocHandle,
    spec: &Value,
    kind: &str,
) -> Result<Vec<Op>, EngineError> {
    let mode = spec["mode"].as_str().unwrap_or("replace");
    if !["replace", "add", "subtract", "intersect"].contains(&mode) {
        return Err(invalid("Unknown selection combination"));
    }
    let tolerance = spec["tolerance"].as_u64().unwrap_or(0);
    if tolerance > 255 {
        return Err(invalid("Color tolerance must be 0..255"));
    }
    let tolerance = tolerance as u8;
    let layer = spec["layer"]
        .as_str()
        .ok_or_else(|| invalid("Color selection requires a layer"))?;
    let layer = NodeId(
        layer
            .parse()
            .map_err(|_| invalid("Color selection requires a layer"))?,
    );
    let (map, offset) = {
        let doc = engine.document(handle)?;
        let node = doc.node(layer)?;
        let NodeKind::Raster { tiles } = &node.kind else {
            return Err(invalid("Color selection requires a raster layer"));
        };
        if tiles.tiles.len() > 32 {
            return Err(invalid("Color selection exceeds the pixel budget"));
        }
        let linear = node.props.transform.0;
        if (linear[0] - 1.0).abs() > 1e-6
            || linear[1].abs() > 1e-6
            || linear[2].abs() > 1e-6
            || (linear[3] - 1.0).abs() > 1e-6
        {
            return Err(invalid("Color selection requires a translation-only layer"));
        }
        (
            tiles.clone(),
            (linear[4].round() as i32, linear[5].round() as i32),
        )
    };
    let previous = engine.document(handle)?.selection.clone();
    let old = previous
        .as_ref()
        .map(|m| decode(m, engine))
        .transpose()?
        .unwrap_or_default();
    let chosen = {
        let read = |x: i32, y: i32| -> Result<Option<[u8; 4]>, EngineError> {
            let key = TileKey {
                tx: x.div_euclid(256),
                ty: y.div_euclid(256),
            };
            let Some(hash) = map.tiles.get(&key) else {
                return Ok(None);
            };
            let Some(tile) = engine
                .get_checked(hash)
                .map_err(alto_store::StoreError::Corrupt)?
            else {
                return Ok(None);
            };
            let i = (y.rem_euclid(256) * 256 + x.rem_euclid(256)) as usize * 4;
            Ok(Some(
                tile.pixels[i..i + 4].try_into().unwrap_or([0, 0, 0, 0]),
            ))
        };
        let mut chosen = Vec::new();
        if kind == "contiguous" {
            let sx = spec["x"].as_i64().ok_or_else(|| invalid("Seed required"))? as i32;
            let sy = spec["y"].as_i64().ok_or_else(|| invalid("Seed required"))? as i32;
            let seed = read(sx - offset.0, sy - offset.1)?
                .ok_or_else(|| invalid("Seed is outside the layer"))?;
            let mut queue = vec![(sx, sy)];
            let mut seen = BTreeSet::new();
            while let Some((x, y)) = queue.pop() {
                if !seen.insert((x, y)) {
                    continue;
                }
                let Some(color) = read(x - offset.0, y - offset.1)? else {
                    continue;
                };
                if color
                    .iter()
                    .zip(seed)
                    .any(|(a, b)| a.abs_diff(b) > tolerance)
                {
                    continue;
                }
                if chosen.len() >= 1_048_576 {
                    return Err(invalid("Contiguous selection exceeds the pixel budget"));
                }
                chosen.push((x, y));
                queue.extend([(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)]);
            }
        } else {
            let target = [
                spec["r"].as_u64().unwrap_or(0) as u8,
                spec["g"].as_u64().unwrap_or(0) as u8,
                spec["b"].as_u64().unwrap_or(0) as u8,
                spec["a"].as_u64().unwrap_or(255) as u8,
            ];
            for (key, hash) in &map.tiles {
                let tile = engine
                    .get_checked(hash)
                    .map_err(alto_store::StoreError::Corrupt)?
                    .ok_or(EngineError::MissingTile(*hash))?;
                for i in 0..(256 * 256) {
                    let px = &tile.pixels[i * 4..i * 4 + 4];
                    if px
                        .iter()
                        .zip(target)
                        .any(|(a, b)| a.abs_diff(b) > tolerance)
                    {
                        continue;
                    }
                    chosen.push((
                        key.tx * 256 + (i % 256) as i32 + offset.0,
                        key.ty * 256 + (i / 256) as i32 + offset.1,
                    ));
                }
            }
        }
        chosen
    };
    let mut keys = std::collections::BTreeSet::new();
    for (x, y) in &chosen {
        keys.insert(TileKey {
            tx: x.div_euclid(256),
            ty: y.div_euclid(256),
        });
    }
    if mode != "replace" {
        keys.extend(old.keys().copied());
    }
    if keys.len() > 4096 {
        return Err(invalid("Selection/mask exceeds 4096 tiles"));
    }
    let chosen: std::collections::BTreeSet<_> = chosen.into_iter().collect();
    let mut out = empty();
    for key in keys {
        let mut pixels = vec![0u8; 65536];
        for iy in 0..256 {
            for ix in 0..256 {
                let x = key.tx * 256 + ix;
                let y = key.ty * 256 + iy;
                let hit = chosen.contains(&(x, y));
                let prior = sample(&old, x, y) as f32 / 255.0;
                let amount = f32::from(hit);
                let coverage = match mode {
                    "add" => prior + amount - prior * amount,
                    "subtract" => prior * (1.0 - amount),
                    "intersect" => prior * amount,
                    _ => amount,
                };
                pixels[(iy * 256 + ix) as usize] = (coverage * 255.0).round() as u8;
            }
        }
        if let Some(hash) = engine.put_tile(TileData::new(PixelFormat::R8, pixels).expect("R8")) {
            out.tiles.insert(key, hash);
        }
    }
    Ok(vec![Op::SetSelection {
        selection: (!out.tiles.is_empty()).then_some(out),
    }])
}
pub fn sample(tiles: &Decoded, x: i32, y: i32) -> u8 {
    tiles
        .get(&TileKey {
            tx: x.div_euclid(256),
            ty: y.div_euclid(256),
        })
        .map_or(0, |t| {
            t.pixels[(y.rem_euclid(256) * 256 + x.rem_euclid(256)) as usize]
        })
}
fn tile_bounds(map: &TileMap) -> Option<[f64; 4]> {
    if map.tiles.is_empty() {
        return None;
    }
    Some([
        map.tiles.keys().map(|k| k.tx * 256).min()? as f64,
        map.tiles.keys().map(|k| k.ty * 256).min()? as f64,
        map.tiles.keys().map(|k| (k.tx + 1) * 256).max()? as f64,
        map.tiles.keys().map(|k| (k.ty + 1) * 256).max()? as f64,
    ])
}
fn bounds_points(points: &[[f64; 2]]) -> [f64; 4] {
    [
        points.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min),
        points.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min),
        points
            .iter()
            .map(|p| p[0])
            .fold(f64::NEG_INFINITY, f64::max),
        points
            .iter()
            .map(|p| p[1])
            .fold(f64::NEG_INFINITY, f64::max),
    ]
}
fn corners(b: [f64; 4]) -> [[f64; 2]; 4] {
    [[b[0], b[1]], [b[2], b[1]], [b[0], b[3]], [b[2], b[3]]]
}
fn checked_bounds(b: [f64; 4]) -> Result<[i32; 4], EngineError> {
    if b.iter().any(|n| !n.is_finite() || n.abs() > 16_777_216.0) {
        return Err(invalid("Mask coordinates exceed supported range"));
    }
    let r = [
        (b[0] / 256.0).floor() as i32,
        (b[1] / 256.0).floor() as i32,
        (b[2] / 256.0).ceil() as i32,
        (b[3] / 256.0).ceil() as i32,
    ];
    if (r[2] - r[0]).max(0) as u64 * (r[3] - r[1]).max(0) as u64 > 4096 {
        return Err(invalid("Selection/mask exceeds 4096 tiles"));
    }
    Ok(r)
}
#[derive(Default, Clone)]
struct LassoRow {
    intersections: Vec<f64>,
    edges: Vec<([f64; 2], [f64; 2])>,
}
fn lasso_rows(
    points: &[[f64; 2]],
    height: i32,
    feather: f64,
) -> Result<Vec<LassoRow>, EngineError> {
    let mut rows = vec![LassoRow::default(); height as usize];
    let mut work = 0.0;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        let y0 = (a[1].min(b[1]) - feather / 2.0).floor().max(0.0) as i32;
        let y1 = (a[1].max(b[1]) + feather / 2.0).ceil().min(height as f64) as i32;
        work += (y1 - y0).max(0) as f64 * ((a[0] - b[0]).abs() + feather).max(1.0);
        if work > 100_000_000.0 {
            return Err(invalid(
                "Lasso is too complex; reduce the points or feather width",
            ));
        }
        for y in y0..y1 {
            let py = y as f64 + 0.5;
            let row = &mut rows[y as usize];
            if (a[1] > py) != (b[1] > py) {
                row.intersections
                    .push((b[0] - a[0]) * (py - a[1]) / (b[1] - a[1]) + a[0]);
            }
            if feather > 0.0 {
                row.edges.push((a, b));
            }
        }
    }
    for row in &mut rows {
        row.intersections.sort_by(f64::total_cmp);
    }
    Ok(rows)
}
fn lasso_coverage(row: &LassoRow, p: [f64; 2], feather: f64) -> f64 {
    let inside = row.intersections.partition_point(|x| *x < p[0]) % 2 == 1;
    if feather == 0.0 {
        return f64::from(inside);
    }
    let mut distance = feather / 2.0;
    for (a, b) in &row.edges {
        if p[0] < a[0].min(b[0]) - distance || p[0] > a[0].max(b[0]) + distance {
            continue;
        }
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (dx * dx + dy * dy).max(0.000001))
            .clamp(0.0, 1.0);
        distance = distance.min((p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy));
    }
    (0.5 + if inside {
        distance / feather
    } else {
        -distance / feather
    })
    .clamp(0.0, 1.0)
}
impl Engine {
    pub fn node_world(&self, h: DocHandle, id: NodeId) -> Result<[f64; 6], EngineError> {
        let doc = self.document(h)?;
        let mut chain = vec![];
        let mut next = Some(id);
        while let Some(id) = next {
            let n = doc.node(id)?;
            chain.push(n.props.transform.0);
            next = n.parent;
        }
        Ok(chain
            .into_iter()
            .rev()
            .fold([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], alto_gpu::renderer::multiply))
    }
    pub fn selection_ops(&mut self, h: DocHandle, spec: &Value) -> Result<Vec<Op>, EngineError> {
        if !spec.is_object() {
            return Err(invalid("Selection options must be an object"));
        }
        let doc = self.document(h)?;
        let w = doc.meta.width as i32;
        let height = doc.meta.height as i32;
        let kind = spec["kind"].as_str().unwrap_or("rectangle");
        if kind == "clear" {
            return Ok(vec![Op::SetSelection { selection: None }]);
        }
        if kind == "color" || kind == "contiguous" {
            return generated_selection(self, h, spec, kind);
        }
        let mode = spec["mode"].as_str().unwrap_or("replace");
        if !["replace", "add", "subtract", "intersect"].contains(&mode) {
            return Err(invalid("Unknown selection combination"));
        }
        let previous = doc.selection.clone();
        let old = previous
            .as_ref()
            .map(|m| decode(m, self))
            .transpose()?
            .unwrap_or_default();
        let number = |k: &str, default: f64| {
            spec.get(k)
                .map_or(Some(default), |v| v.as_f64())
                .filter(|n| n.is_finite())
                .ok_or_else(|| invalid("Selection coordinates must be finite"))
        };
        let x = number("x", 0.0)?;
        let y = number("y", 0.0)?;
        let width = number("width", 0.0)?;
        let hh = number("height", 0.0)?;
        let feather = number("feather", 0.0)?;
        if !(0.0..=64.0).contains(&feather) {
            return Err(invalid("Selection feather must be 0..64"));
        }
        let rect = [
            x.min(x + width),
            y.min(y + hh),
            x.max(x + width),
            y.max(y + hh),
        ];
        let points: Vec<[f64; 2]> = if kind == "lasso" {
            serde_json::from_value(spec["points"].clone())
                .map_err(|_| invalid("Lasso points required"))?
        } else {
            vec![]
        };
        if kind == "lasso"
            && (points.len() < 3
                || points.len() > 8192
                || points.iter().flatten().any(|v| !v.is_finite()))
        {
            return Err(invalid("Lasso requires 3..8192 finite points"));
        }
        let mut bounds = match kind {
            "all" | "invert" => [0.0, 0.0, w as f64, height as f64],
            "rectangle" | "ellipse" => rect,
            "lasso" => bounds_points(&points),
            _ => return Err(invalid("Unknown selection shape")),
        };
        if ["rectangle", "ellipse", "lasso"].contains(&kind) {
            bounds = [
                bounds[0] - feather / 2.0,
                bounds[1] - feather / 2.0,
                bounds[2] + feather / 2.0,
                bounds[3] + feather / 2.0,
            ];
        }
        let lasso = if kind == "lasso" {
            Some(lasso_rows(&points, height, feather)?)
        } else {
            None
        };
        if mode != "replace"
            && let Some(b) = previous.as_ref().and_then(tile_bounds)
        {
            bounds = [
                bounds[0].min(b[0]),
                bounds[1].min(b[1]),
                bounds[2].max(b[2]),
                bounds[3].max(b[3]),
            ];
        }
        bounds = [
            bounds[0].max(0.0),
            bounds[1].max(0.0),
            bounds[2].min(w as f64),
            bounds[3].min(height as f64),
        ];
        let r = checked_bounds(bounds)?;
        let mut map = empty();
        for ty in r[1]..r[3] {
            for tx in r[0]..r[2] {
                let mut pixels = vec![0; 65536];
                for iy in 0..256 {
                    for ix in 0..256 {
                        let px = tx * 256 + ix;
                        let py = ty * 256 + iy;
                        if px < 0 || py < 0 || px >= w || py >= height {
                            continue;
                        }
                        let p = [px as f64 + 0.5, py as f64 + 0.5];
                        let prior = sample(&old, px, py) as f64 / 255.0;
                        let amount = match kind {
                            "all" => 1.0,
                            "invert" => 1.0 - if previous.is_some() { prior } else { 1.0 },
                            "rectangle" => {
                                let d = (p[0] - rect[0])
                                    .min(rect[2] - p[0])
                                    .min(p[1] - rect[1])
                                    .min(rect[3] - p[1]);
                                if feather > 0.0 {
                                    (d / feather + 0.5).clamp(0.0, 1.0)
                                } else {
                                    f64::from(d >= 0.0)
                                }
                            }
                            "ellipse" => {
                                let rx = (rect[2] - rect[0]) / 2.0;
                                let ry = (rect[3] - rect[1]) / 2.0;
                                let qx = (p[0] - (rect[0] + rx)) / rx.max(0.001);
                                let qy = (p[1] - (rect[1] + ry)) / ry.max(0.001);
                                let length = qx.hypot(qy);
                                let gradient = (qx / rx.max(0.001)).hypot(qy / ry.max(0.001));
                                let d = if gradient > 0.000001 {
                                    (1.0 - length) * length / gradient
                                } else {
                                    rx.min(ry)
                                };
                                if feather > 0.0 {
                                    (d / feather + 0.5).clamp(0.0, 1.0)
                                } else {
                                    f64::from(d >= 0.0)
                                }
                            }
                            _ => lasso_coverage(
                                &lasso.as_ref().expect("validated lasso")[py as usize],
                                p,
                                feather,
                            ),
                        };
                        let coverage = if kind == "invert" {
                            amount
                        } else {
                            match mode {
                                "add" => prior + amount - prior * amount,
                                "subtract" => prior * (1.0 - amount),
                                "intersect" => prior * amount,
                                _ => amount,
                            }
                        };
                        pixels[(iy * 256 + ix) as usize] = (coverage * 255.0).round() as u8;
                    }
                }
                if let Some(hash) =
                    self.put_tile(TileData::new(PixelFormat::R8, pixels).expect("fixed R8 size"))
                {
                    map.tiles.insert(TileKey { tx, ty }, hash);
                }
            }
        }
        Ok(vec![Op::SetSelection {
            selection: Some(map),
        }])
    }
    pub fn mask_for_node(
        &mut self,
        h: DocHandle,
        id: NodeId,
        white: bool,
        linked: bool,
    ) -> Result<TileMap, EngineError> {
        let doc = self.document(h)?;
        let w = doc.meta.width as f64;
        let height = doc.meta.height as f64;
        let selection = if white { None } else { doc.selection.clone() };
        let world = if linked {
            self.node_world(h, id)?
        } else {
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
        };
        let inv = alto_gpu::renderer::inverse(world)
            .ok_or_else(|| invalid("Mask transform is singular"))?;
        let source_bounds = selection
            .as_ref()
            .and_then(tile_bounds)
            .unwrap_or([0.0, 0.0, w, height]);
        if selection.as_ref().is_some_and(|s| s.tiles.is_empty()) {
            return Ok(empty());
        }
        let local =
            bounds_points(&corners(source_bounds).map(|p| alto_gpu::renderer::point(inv, p)));
        let r = checked_bounds(local)?;
        let decoded = selection
            .as_ref()
            .map(|m| decode(m, self))
            .transpose()?
            .unwrap_or_default();
        let mut mask = empty();
        for ty in r[1]..r[3] {
            for tx in r[0]..r[2] {
                let mut pixels = vec![0; 65536];
                for iy in 0..256 {
                    for ix in 0..256 {
                        let p = alto_gpu::renderer::point(
                            world,
                            [(tx * 256 + ix) as f64 + 0.5, (ty * 256 + iy) as f64 + 0.5],
                        );
                        pixels[(iy * 256 + ix) as usize] =
                            if p[0] < 0.0 || p[1] < 0.0 || p[0] >= w || p[1] >= height {
                                0
                            } else if selection.is_some() {
                                sample(&decoded, p[0].floor() as i32, p[1].floor() as i32)
                            } else {
                                255
                            };
                    }
                }
                if let Some(hash) =
                    self.put_tile(TileData::new(PixelFormat::R8, pixels).expect("fixed R8 size"))
                {
                    mask.tiles.insert(TileKey { tx, ty }, hash);
                }
            }
        }
        Ok(mask)
    }
    pub fn effect_ops(
        &mut self,
        h: DocHandle,
        id: NodeId,
        mut effects: Vec<alto_doc::Effect>,
        use_selection: bool,
    ) -> Result<Vec<Op>, EngineError> {
        if effects.len() > 32 {
            return Err(invalid("At most 32 effects per node"));
        }
        for e in &effects {
            alto_doc::validate_effect(e)?;
        }
        if use_selection && self.document(h)?.selection.is_some() {
            let mask = self.mask_for_node(h, id, false, true)?;
            for e in effects.iter_mut().filter(|e| e.selection().is_none()) {
                *e.selection_mut() = Some(mask.clone());
            }
        }
        for e in &effects {
            alto_doc::validate_effect(e)?;
        }
        Ok(vec![Op::SetProps {
            id,
            patch: PropPatch {
                effects: Some(effects),
                ..Default::default()
            },
        }])
    }
    pub fn selection_outline(&self, h: DocHandle) -> Result<String, EngineError> {
        let Some(map) = &self.document(h)?.selection else {
            return Ok(String::new());
        };
        let decoded = decode(map, self)?;
        let mut path = String::new();
        use std::fmt::Write;
        for (key, tile) in &decoded {
            for y in 0..256 {
                for x in 0..256 {
                    if tile.pixels[(y * 256 + x) as usize] < 128 {
                        continue;
                    }
                    let px = key.tx * 256 + x;
                    let py = key.ty * 256 + y;
                    if sample(&decoded, px, py - 1) < 128 {
                        let _ = write!(path, "M{px},{py}h1");
                    }
                    if sample(&decoded, px, py + 1) < 128 {
                        let _ = write!(path, "M{px},{}h1", py + 1);
                    }
                    if sample(&decoded, px - 1, py) < 128 {
                        let _ = write!(path, "M{px},{py}v1");
                    }
                    if sample(&decoded, px + 1, py) < 128 {
                        let _ = write!(path, "M{},{py}v1", px + 1);
                    }
                }
            }
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alto_doc::{DocumentMeta, Node};
    use serde_json::json;
    fn commit(e: &mut Engine, h: DocHandle, ops: Vec<Op>) {
        let rev = e.document(h).unwrap().revision;
        e.transact(h, "M5", ops, rev).unwrap();
    }
    #[test]
    fn selection_masks_effects_survive_history_gc_package_and_journal() {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(512, 256)).unwrap();
        let n = Node::raster("M5");
        let id = n.id;
        let root = e.document(h).unwrap().root;
        commit(
            &mut e,
            h,
            vec![Op::InsertNode {
                node: n,
                parent: root,
                index: 0,
            }],
        );
        let base = e.save_package(h).unwrap();
        let mut records = vec![];
        let mut apply = |e: &mut Engine, ops: Vec<Op>| {
            commit(e, h, ops.clone());
            records.push(e.journal_record("M5", &ops).unwrap());
        };
        let ops = e
            .selection_ops(
                h,
                &json!({"kind":"rectangle","x":248,"y":10,"width":16,"height":220}),
            )
            .unwrap();
        apply(&mut e, ops);
        let mask = e.mask_for_node(h, id, false, true).unwrap();
        let mask_hashes: Vec<_> = mask.tiles.values().copied().collect();
        apply(
            &mut e,
            vec![Op::SetMask {
                layer: id,
                mask: Some(mask),
            }],
        );
        let ops = e
            .effect_ops(
                h,
                id,
                vec![alto_doc::Effect::GaussianBlur {
                    radius: 12.0,
                    selection: None,
                    extra: Default::default(),
                }],
                true,
            )
            .unwrap();
        apply(&mut e, ops);
        apply(&mut e, vec![Op::SetSelection { selection: None }]);
        let snapshot = e.snapshot(h).unwrap();
        e.undo(h).unwrap();
        e.redo(h).unwrap();
        assert_eq!(snapshot, e.snapshot(h).unwrap());
        for hash in &mask_hashes {
            assert_eq!(e.tile(hash).unwrap().format, PixelFormat::R8);
        }
        let package = e.save_package(h).unwrap();
        let mut fresh = Engine::new();
        let (loaded, warnings) = fresh.open_package(&package).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(snapshot, fresh.snapshot(loaded).unwrap());
        let mut recovered = Engine::new();
        let (r, _) = recovered.open_package(&base).unwrap();
        for record in records {
            recovered.replay_record(r, &record).unwrap();
        }
        assert_eq!(snapshot, recovered.snapshot(r).unwrap());
        for hash in mask_hashes {
            assert!(recovered.tile(&hash).is_some());
        }
    }
    #[test]
    fn feather_crosses_tile_boundary_for_rectangle_and_lasso() {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(512, 256)).unwrap();
        for spec in [
            json!({"kind":"rectangle","x":256,"y":0,"width":144,"height":200,"feather":8}),
            json!({"kind":"lasso","points":[[256,0],[400,0],[400,200],[256,200]],"feather":8}),
        ] {
            let ops = e.selection_ops(h, &spec).unwrap();
            commit(&mut e, h, ops);
            let map = e.document(h).unwrap().selection.as_ref().unwrap();
            let decoded = decode(map, &e).unwrap();
            assert_eq!(sample(&decoded, 255, 100), 112);
            assert_eq!(sample(&decoded, 256, 100), 143);
            assert_eq!(sample(&decoded, 240, 100), 0);
            assert_eq!(sample(&decoded, 270, 100), 255);
        }
    }
    #[test]
    fn invalid_effect_and_rgba_mask_are_atomic() {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(256, 256)).unwrap();
        let n = Node::raster("layer");
        let id = n.id;
        let root = e.document(h).unwrap().root;
        commit(
            &mut e,
            h,
            vec![Op::InsertNode {
                node: n,
                parent: root,
                index: 0,
            }],
        );
        let before = e.snapshot(h).unwrap();
        let revision = e.document(h).unwrap().revision;
        for ops in [
            vec![Op::SetMask {
                layer: id,
                mask: Some(TileMap::new(PixelFormat::Rgba8)),
            }],
            vec![Op::SetProps {
                id,
                patch: PropPatch {
                    effects: Some(vec![alto_doc::Effect::GaussianBlur {
                        radius: -1.0,
                        selection: None,
                        extra: Default::default(),
                    }]),
                    ..Default::default()
                },
            }],
        ] {
            assert!(e.transact(h, "bad", ops, revision).is_err());
            assert_eq!(before, e.snapshot(h).unwrap());
        }
    }
}
