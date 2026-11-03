//! CPU-owned, reversible image edits. Prepared output stays rooted until applied or released.
use crate::{DocHandle, Engine, EngineError};
use alto_core::{Affine2, NodeId, PixelFormat, TileKey};
use alto_doc::{ImageBounds, NodeKind, Op};
use alto_raster::{TileData, TileMap};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
/// A polyline in node-local pixels, drawn with a round tip. Inside `radius * hardness`
/// the tip has full coverage, and it falls off linearly to the radius.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    pub points: Vec<[f64; 2]>,
    pub radius: f64,
    #[serde(default = "one")]
    pub hardness: f64,
}
const MAX_STROKE_POINTS: usize = 4096;
const MAX_STROKE_RADIUS: f64 = 1024.0;
/// Pixels touched across all dabs of one smudge.
const SMUDGE_OPS: u64 = 64 * 1024 * 1024;
impl Stroke {
    fn validate(&self) -> Result<Vec<[i32; 2]>, EngineError> {
        if self.points.is_empty() || self.points.len() > MAX_STROKE_POINTS {
            return Err(err("INVALID_ARGUMENT", "a stroke has 1 to 4096 points"));
        }
        if !self.radius.is_finite() || !(1.0..=MAX_STROKE_RADIUS).contains(&self.radius) {
            return Err(err("INVALID_ARGUMENT", "stroke radius must be 1 to 1024"));
        }
        if !self.hardness.is_finite() || !(0.0..=1.0).contains(&self.hardness) {
            return Err(err("INVALID_ARGUMENT", "stroke hardness must be 0 to 1"));
        }
        self.points.iter().map(|p| point(*p)).collect()
    }
    /// Tip positions in order: the points, and steps of a quarter radius between them.
    fn dabs(&self, points: &[[i32; 2]]) -> Result<Vec<[i32; 2]>, EngineError> {
        let spacing = (self.radius / 4.0).max(1.0);
        let mut out = vec![points[0]];
        for pair in points.windows(2) {
            let (dx, dy) = (
                f64::from(pair[1][0]) - f64::from(pair[0][0]),
                f64::from(pair[1][1]) - f64::from(pair[0][1]),
            );
            let steps = (dx.hypot(dy) / spacing).ceil() as u64;
            if out.len() as u64 + steps > SMUDGE_OPS {
                return Err(err("RESOURCE_LIMIT", "stroke is too long"));
            }
            for s in 1..=steps {
                let t = s as f64 / steps as f64;
                out.push([
                    (f64::from(pair[0][0]) + dx * t).round() as i32,
                    (f64::from(pair[0][1]) + dy * t).round() as i32,
                ]);
            }
        }
        Ok(out)
    }
    /// Tip coverage, 0 to 255, at `(dx, dy)` pixels from the center.
    fn coverage(&self, dx: i64, dy: i64) -> u8 {
        let d = ((dx * dx + dy * dy) as f64).sqrt();
        let inner = self.radius * self.hardness;
        if d >= self.radius {
            0
        } else if d <= inner {
            255
        } else {
            (255.0 * (self.radius - d) / (self.radius - inner)).round() as u8
        }
    }
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
fn write(out: &mut BTreeMap<TileKey, Vec<u8>>, x: i32, y: i32, p: [u8; 4]) {
    if p == [0; 4] {
        return;
    }
    let key = TileKey {
        tx: x.div_euclid(256),
        ty: y.div_euclid(256),
    };
    let tile = out.entry(key).or_insert_with(|| vec![0; 262144]);
    let i = (y.rem_euclid(256) as usize * 256 + x.rem_euclid(256) as usize) * 4;
    tile[i..i + 4].copy_from_slice(&p);
}
/// Reads one map, keeping the tile of the last read. Neighboring pixels nearly always
/// share it, and a map lookup per pixel was most of the cost of a large edit.
struct Reader<'a> {
    engine: &'a Engine,
    map: &'a TileMap,
    last: Option<(TileKey, Option<std::sync::Arc<TileData>>)>,
}
impl<'a> Reader<'a> {
    fn new(engine: &'a Engine, map: &'a TileMap) -> Self {
        Self {
            engine,
            map,
            last: None,
        }
    }
    fn get(&mut self, x: i32, y: i32) -> Result<[u8; 4], EngineError> {
        let key = TileKey {
            tx: x.div_euclid(256),
            ty: y.div_euclid(256),
        };
        if self.last.as_ref().is_none_or(|(k, _)| *k != key) {
            let tile = match self.map.tiles.get(&key) {
                Some(hash) => {
                    let tile = self.engine.require_tile(hash)?;
                    if tile.format != self.map.format {
                        return Err(err("CORRUPT_DATA", "tile format mismatch"));
                    }
                    Some(tile)
                }
                None => None,
            };
            self.last = Some((key, tile));
        }
        let Some((_, Some(tile))) = &self.last else {
            return Ok([0; 4]);
        };
        let i = (y.rem_euclid(256) as usize * 256 + x.rem_euclid(256) as usize)
            * self.map.format.bytes_per_pixel();
        Ok(if self.map.format == PixelFormat::R8 {
            [0, 0, 0, tile.pixels[i]]
        } else {
            tile.pixels[i..i + 4].try_into().unwrap()
        })
    }
}
/// Writes pixels, keeping the tile of the last write out of the map.
#[derive(Default)]
struct Writer {
    tiles: BTreeMap<TileKey, Vec<u8>>,
    current: Option<(TileKey, Vec<u8>)>,
}
impl Writer {
    fn put(&mut self, x: i32, y: i32, p: [u8; 4]) {
        if p == [0; 4] {
            return;
        }
        let key = TileKey {
            tx: x.div_euclid(256),
            ty: y.div_euclid(256),
        };
        if self.current.as_ref().is_none_or(|(k, _)| *k != key) {
            if let Some((k, tile)) = self.current.take() {
                self.tiles.insert(k, tile);
            }
            let tile = self.tiles.remove(&key).unwrap_or_else(|| vec![0; 262144]);
            self.current = Some((key, tile));
        }
        let (_, tile) = self.current.as_mut().expect("just set");
        let i = (y.rem_euclid(256) as usize * 256 + x.rem_euclid(256) as usize) * 4;
        tile[i..i + 4].copy_from_slice(&p);
    }
    fn finish(mut self) -> BTreeMap<TileKey, Vec<u8>> {
        if let Some((k, tile)) = self.current.take() {
            self.tiles.insert(k, tile);
        }
        self.tiles
    }
}
fn rounded(n: i128, d: i128) -> i64 {
    let a = n.unsigned_abs();
    let d = d as u128;
    let q = (a / d + u128::from(a % d >= d.div_ceil(2))) as i64;
    if n < 0 { -q } else { q }
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
fn selection(
    engine: &Engine,
    map: Option<&TileMap>,
    m: alto_core::FixedAffine,
    x: i32,
    y: i32,
) -> Result<u8, EngineError> {
    let Some(map) = map else { return Ok(255) };
    // Pixel centers mapped with v3's composed Q32/Q16 transform, then quantized to Q8.
    let cx = 2 * i128::from(x) + 1;
    let cy = 2 * i128::from(y) + 1;
    let qx = rounded(
        i128::from(m.m[0]) * cx + i128::from(m.m[2]) * cy + i128::from(m.t[0]) * (1 << 17)
            - (1 << 32),
        1 << 25,
    );
    let qy = rounded(
        i128::from(m.m[1]) * cx + i128::from(m.m[3]) * cy + i128::from(m.t[1]) * (1 << 17)
            - (1 << 32),
        1 << 25,
    );
    if qx.div_euclid(256) < i64::from(i32::MIN)
        || qx.div_euclid(256) >= i64::from(i32::MAX)
        || qy.div_euclid(256) < i64::from(i32::MIN)
        || qy.div_euclid(256) >= i64::from(i32::MAX)
    {
        return Err(err("INVALID_ARGUMENT", "selection mapping out of range"));
    }
    let x = qx.div_euclid(256) as i32;
    let y = qy.div_euclid(256) as i32;
    let fx = qx.rem_euclid(256) as u64;
    let fy = qy.rem_euclid(256) as u64;
    let mut n = 0u64;
    for (dx, dy, w) in [
        (0, 0, (256 - fx) * (256 - fy)),
        (1, 0, fx * (256 - fy)),
        (0, 1, (256 - fx) * fy),
        (1, 1, fx * fy),
    ] {
        n += u64::from(read(engine, map, x + dx, y + dy)?[3]) * w;
    }
    Ok(((n + 32768) / 65536) as u8)
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
fn linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
fn srgb(c: f64) -> f64 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}
fn gradient(options: &GradientOptions, p: [f64; 2]) -> [f64; 4] {
    let dx = options.end[0] - options.start[0];
    let dy = options.end[1] - options.start[1];
    let u = match options.kind {
        GradientKind::Linear => {
            ((p[0] - options.start[0]) * dx + (p[1] - options.start[1]) * dy) / (dx * dx + dy * dy)
        }
        GradientKind::Radial => {
            (p[0] - options.start[0]).hypot(p[1] - options.start[1]) / dx.hypot(dy)
        }
    }
    .clamp(0.0, 1.0);
    let stops = &options.stops;
    let hi = stops.partition_point(|s| s.offset < u);
    let (a, b, t) = if hi == 0 {
        (&stops[0], &stops[0], 0.0)
    } else if hi == stops.len() {
        (&stops[hi - 1], &stops[hi - 1], 0.0)
    } else {
        let (a, b) = (&stops[hi - 1], &stops[hi]);
        (a, b, (u - a.offset) / (b.offset - a.offset))
    };
    let alpha = a.color[3] * (1.0 - t) + b.color[3] * t;
    let mut c = [0.0; 4];
    c[3] = alpha * options.opacity;
    if alpha > 0.0 {
        for (i, channel) in c.iter_mut().enumerate().take(3) {
            *channel = srgb(
                ((linear(a.color[i]) * a.color[3]) * (1.0 - t)
                    + (linear(b.color[i]) * b.color[3]) * t)
                    / alpha,
            )
            .clamp(0.0, 1.0);
        }
    }
    c
}
fn resize_pixel(
    reader: &mut Reader,
    b: ImageBounds,
    dst: ImageBounds,
    x: i32,
    y: i32,
    filter: ImageFilter,
) -> Result<[u8; 4], EngineError> {
    let u = i128::from(x) - i128::from(dst.x);
    let v = i128::from(y) - i128::from(dst.y);
    if matches!(filter, ImageFilter::Nearest) {
        return reader.get(
            b.x + (((2 * u + 1) * i128::from(b.width)) / (2 * i128::from(dst.width))) as i32,
            b.y + (((2 * v + 1) * i128::from(b.height)) / (2 * i128::from(dst.height))) as i32,
        );
    }
    if matches!(filter, ImageFilter::Area) {
        return area_pixel(reader, b, dst, u, v);
    }
    let coordinate = |origin: i32, u: i128, old: u32, new: u32| {
        rounded(
            i128::from(origin) * 256 * 2 * i128::from(new) + (2 * u + 1) * i128::from(old) * 256
                - 256 * i128::from(new),
            2 * i128::from(new),
        )
    };
    let qx = coordinate(b.x, u, b.width, dst.width);
    let qy = coordinate(b.y, v, b.height, dst.height);
    let ix = qx.div_euclid(256) as i32;
    let iy = qy.div_euclid(256) as i32;
    let fx = qx.rem_euclid(256) as u64;
    let fy = qy.rem_euclid(256) as u64;
    if matches!(filter, ImageFilter::CatmullRom) {
        return catmull_rom_pixel(reader, b, [ix, iy], [fx as i128, fy as i128]);
    }
    let mut acc = [0u64; 4];
    for (dx, dy, w) in [
        (0, 0, (256 - fx) * (256 - fy)),
        (1, 0, fx * (256 - fy)),
        (0, 1, (256 - fx) * fy),
        (1, 1, fx * fy),
    ] {
        let p = reader.get(
            (ix + dx).clamp(b.x, b.right() - 1),
            (iy + dy).clamp(b.y, b.bottom() - 1),
        )?;
        acc[3] += ((u64::from(p[3]) * 16384 + 127) / 255) * w;
        for c in 0..3 {
            acc[c] += ((u64::from(p[c]) * u64::from(p[3]) * 16384 + 32512) / 65025) * w;
        }
    }
    let a = (acc[3] + 32768) / 65536;
    let mut out = [0; 4];
    out[3] = ((a * 255 + 8192) / 16384) as u8;
    for c in 0..3 {
        let p = (acc[c] + 32768) / 65536;
        out[c] = (p * 255 + a / 2).checked_div(a).unwrap_or(0).min(255) as u8;
    }
    Ok(out)
}
/// Premultiplied Q14 RGBA from straight RGBA8, with the bilinear filter's rounding.
fn premultiplied_q14(p: [u8; 4]) -> [i128; 4] {
    let a = (u64::from(p[3]) * 16384 + 127) / 255;
    let mut out = [0i128; 4];
    out[3] = i128::from(a);
    for c in 0..3 {
        out[c] = i128::from((u64::from(p[c]) * u64::from(p[3]) * 16384 + 32512) / 65025);
    }
    out
}
/// Back to straight RGBA8. Colors stay inside alpha, because a filter can overshoot.
fn straight_from_q14(p: [i128; 4]) -> [u8; 4] {
    let a = p[3].clamp(0, 16384) as u64;
    let mut out = [0; 4];
    out[3] = ((a * 255 + 8192) / 16384) as u8;
    for c in 0..3 {
        let v = p[c].clamp(0, a as i128) as u64;
        out[c] = (v * 255 + a / 2).checked_div(a).unwrap_or(0).min(255) as u8;
    }
    out
}
/// Catmull-Rom in exact integers: the weights of `t = f / 256` over `2 * 256^3`,
/// premultiplied, clamped to the source range at the edges.
fn catmull_rom_pixel(
    reader: &mut Reader,
    b: ImageBounds,
    at: [i32; 2],
    f: [i128; 2],
) -> Result<[u8; 4], EngineError> {
    let weights = |t: i128| {
        const D: i128 = 256;
        [
            -t * D * D + 2 * t * t * D - t * t * t,
            2 * D * D * D - 5 * t * t * D + 3 * t * t * t,
            t * D * D + 4 * t * t * D - 3 * t * t * t,
            -t * t * D + t * t * t,
        ]
    };
    let (wx, wy) = (weights(f[0]), weights(f[1]));
    let mut acc = [0i128; 4];
    for (j, wj) in wy.iter().enumerate() {
        if *wj == 0 {
            continue;
        }
        for (i, wi) in wx.iter().enumerate() {
            if *wi == 0 {
                continue;
            }
            let p = premultiplied_q14(reader.get(
                (at[0] + i as i32 - 1).clamp(b.x, b.right() - 1),
                (at[1] + j as i32 - 1).clamp(b.y, b.bottom() - 1),
            )?);
            for c in 0..4 {
                acc[c] += p[c] * wi * wj;
            }
        }
    }
    // The weights of each axis sum to 2 * 256^3, so the product is 2^50.
    Ok(straight_from_q14(acc.map(|v| (v + (1 << 49)) >> 50)))
}
/// One new pixel's source area, in exact rational weights.
fn area_pixel(
    reader: &mut Reader,
    b: ImageBounds,
    dst: ImageBounds,
    u: i128,
    v: i128,
) -> Result<[u8; 4], EngineError> {
    let spans = |d: i128, old: u32, new: u32| {
        let (old, new) = (i128::from(old), i128::from(new));
        let (start, end) = (d * old, (d + 1) * old);
        let first = start.div_euclid(new);
        let last = (end - 1).div_euclid(new);
        (first..=last).map(move |s| {
            let weight = end.min((s + 1) * new) - start.max(s * new);
            (s as i32, weight)
        })
    };
    let mut acc = [0i128; 4];
    for (sy, wy) in spans(v, b.height, dst.height) {
        for (sx, wx) in spans(u, b.width, dst.width) {
            let p = premultiplied_q14(reader.get(b.x + sx, b.y + sy)?);
            for c in 0..4 {
                acc[c] += p[c] * wx * wy;
            }
        }
    }
    // Weights sum to the source size on each axis.
    let total = i128::from(b.width) * i128::from(b.height);
    Ok(straight_from_q14(acc.map(|v| (v + total / 2) / total)))
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

/// Pixels of a raster being edited by a stroke. Untouched tiles keep their hashes.
struct Working<'a> {
    engine: &'a Engine,
    map: &'a TileMap,
    tiles: BTreeMap<TileKey, Vec<u8>>,
}
impl Working<'_> {
    fn get(&mut self, x: i32, y: i32) -> Result<[u8; 4], EngineError> {
        let key = TileKey {
            tx: x.div_euclid(256),
            ty: y.div_euclid(256),
        };
        let i = (y.rem_euclid(256) as usize * 256 + x.rem_euclid(256) as usize) * 4;
        if let Some(tile) = self.tiles.get(&key) {
            return Ok(tile[i..i + 4].try_into().unwrap());
        }
        read(self.engine, self.map, x, y)
    }
    fn put(&mut self, x: i32, y: i32, p: [u8; 4]) -> Result<(), EngineError> {
        let key = TileKey {
            tx: x.div_euclid(256),
            ty: y.div_euclid(256),
        };
        if !self.tiles.contains_key(&key) {
            let pixels = match self.map.tiles.get(&key) {
                Some(hash) => self.engine.require_tile(hash)?.pixels.to_vec(),
                None => vec![0; 262144],
            };
            self.tiles.insert(key, pixels);
            if self.tiles.len() as u64 * 262144 > EDIT_BUDGET {
                return Err(err(
                    "RESOURCE_LIMIT",
                    "image edit working-set budget exceeded",
                ));
            }
        }
        let i = (y.rem_euclid(256) as usize * 256 + x.rem_euclid(256) as usize) * 4;
        self.tiles.get_mut(&key).expect("just inserted")[i..i + 4].copy_from_slice(&p);
        Ok(())
    }
}
/// Q8 interpolation in premultiplied space, returned as straight RGBA8.
fn lerp_premultiplied(from: [u8; 4], to: [u8; 4], weight: u32) -> [u8; 4] {
    let inverse = 65025 - weight;
    let channel = |a: u8, aa: u8, b: u8, ba: u8| {
        u64::from(a) * u64::from(aa) * u64::from(inverse)
            + u64::from(b) * u64::from(ba) * u64::from(weight)
    };
    let alpha = u64::from(from[3]) * u64::from(inverse) + u64::from(to[3]) * u64::from(weight);
    let mut out = [0; 4];
    out[3] = ((alpha + 32512) / 65025) as u8;
    for c in 0..3 {
        let p = channel(from[c], from[3], to[c], to[3]);
        out[c] = (p + alpha / 2).checked_div(alpha).unwrap_or(0).min(255) as u8;
    }
    if out[3] == 0 {
        return [0; 4];
    }
    out
}
impl Engine {
    #[allow(clippy::too_many_arguments)]
    fn prepare_stroke(
        &mut self,
        h: DocHandle,
        id: NodeId,
        mut node: alto_doc::Node,
        edit: ImageEdit,
        mask: Option<TileMap>,
        m: alto_core::FixedAffine,
        stage: u64,
        revision: u64,
    ) -> Result<PreparedImageEdit, EngineError> {
        let NodeKind::Raster { bounds, tiles: map } = node.kind.clone() else {
            return Err(err("INVALID_ARGUMENT", "raster required"));
        };
        let mut work = Working {
            engine: self,
            map: &map,
            tiles: BTreeMap::new(),
        };
        let mut selected = mask.is_none();
        match &edit {
            ImageEdit::CloneStamp {
                source,
                stroke,
                opacity,
                ..
            } => {
                if !opacity.is_finite() || !(0.0..=1.0).contains(opacity) {
                    return Err(err("INVALID_ARGUMENT", "stamp opacity must be 0 to 1"));
                }
                let points = stroke.validate()?;
                let [sx, sy] = point(*source)?;
                let (dx, dy) = (
                    i64::from(sx) - i64::from(points[0][0]),
                    i64::from(sy) - i64::from(points[0][1]),
                );
                let r = stroke.radius.ceil() as i64;
                // Each pixel takes its largest coverage once, so overlapping dabs do not
                // build up a semi-transparent source.
                let mut coverage: BTreeMap<(i32, i32), u8> = BTreeMap::new();
                for dab in stroke.dabs(&points)? {
                    for y in i64::from(dab[1]) - r..=i64::from(dab[1]) + r {
                        for x in i64::from(dab[0]) - r..=i64::from(dab[0]) + r {
                            let (Ok(px), Ok(py)) = (i32::try_from(x), i32::try_from(y)) else {
                                continue;
                            };
                            if !bounds.contains(px, py) {
                                continue;
                            }
                            let c = stroke.coverage(x - i64::from(dab[0]), y - i64::from(dab[1]));
                            if c > 0 {
                                let slot = coverage.entry((px, py)).or_insert(0);
                                *slot = (*slot).max(c);
                            }
                        }
                    }
                    if coverage.len() as u64 > 16 * 1024 * 1024 {
                        return Err(err("RESOURCE_LIMIT", "stroke covers too many pixels"));
                    }
                }
                for ((x, y), c) in coverage {
                    let s = selection(self, mask.as_ref(), m, x, y)?;
                    if s == 0 {
                        continue;
                    }
                    selected = true;
                    let from = (i64::from(x) + dx, i64::from(y) + dy);
                    let src = match (i32::try_from(from.0), i32::try_from(from.1)) {
                        (Ok(fx), Ok(fy)) if bounds.contains(fx, fy) => read(self, &map, fx, fy)?,
                        _ => [0; 4],
                    };
                    let color = [
                        f64::from(src[0]) / 255.0,
                        f64::from(src[1]) / 255.0,
                        f64::from(src[2]) / 255.0,
                        f64::from(src[3]) / 255.0 * opacity,
                    ];
                    let coverage = ((u32::from(c) * u32::from(s) + 127) / 255) as u8;
                    let p = over(work.get(x, y)?, color, coverage);
                    work.put(x, y, p)?;
                }
            }
            ImageEdit::Smudge {
                stroke, strength, ..
            } => {
                if !strength.is_finite() || !(0.0..=1.0).contains(strength) {
                    return Err(err("INVALID_ARGUMENT", "smudge strength must be 0 to 1"));
                }
                let points = stroke.validate()?;
                let dabs = stroke.dabs(&points)?;
                let r = stroke.radius.ceil() as i64;
                let span = (2 * r + 1) as usize;
                if dabs.len() as u64 * (span * span) as u64 > SMUDGE_OPS {
                    return Err(err("RESOURCE_LIMIT", "smudge touches too many pixels"));
                }
                let strength = (strength * 255.0).round() as u32;
                let inside = |x: i64, y: i64| -> Option<(i32, i32)> {
                    let (px, py) = (i32::try_from(x).ok()?, i32::try_from(y).ok()?);
                    bounds.contains(px, py).then_some((px, py))
                };
                // The pixels the tip carries, picked up where the stroke starts.
                let mut carried = vec![[0u8; 4]; span * span];
                for v in -r..=r {
                    for u in -r..=r {
                        if let Some((x, y)) =
                            inside(i64::from(dabs[0][0]) + u, i64::from(dabs[0][1]) + v)
                        {
                            carried[(v + r) as usize * span + (u + r) as usize] = work.get(x, y)?;
                        }
                    }
                }
                for dab in &dabs[1..] {
                    for v in -r..=r {
                        for u in -r..=r {
                            let c = stroke.coverage(u, v);
                            let Some((x, y)) = inside(i64::from(dab[0]) + u, i64::from(dab[1]) + v)
                            else {
                                continue;
                            };
                            if c == 0 {
                                continue;
                            }
                            let s = selection(self, mask.as_ref(), m, x, y)?;
                            if s == 0 {
                                continue;
                            }
                            selected = true;
                            let slot = (v + r) as usize * span + (u + r) as usize;
                            let weight = u32::from(c) * u32::from(s) * strength / 255;
                            let next = lerp_premultiplied(work.get(x, y)?, carried[slot], weight);
                            work.put(x, y, next)?;
                            carried[slot] = next;
                        }
                    }
                }
            }
            _ => unreachable!("only strokes reach prepare_stroke"),
        }
        if !selected {
            return Err(err(
                "EMPTY_SELECTION",
                "selection does not cover the stroke",
            ));
        }
        let touched = work.tiles;
        let mut tiles = map.clone();
        for (key, pixels) in touched {
            match self.stage_tile(stage, TileData::new(PixelFormat::Rgba8, pixels)?)? {
                Some(hash) => {
                    tiles.tiles.insert(key, hash);
                }
                None => {
                    tiles.tiles.remove(&key);
                }
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
        let no_op = changed_tiles == 0;
        node.kind = NodeKind::Raster { bounds, tiles };
        Ok(PreparedImageEdit {
            document: h,
            stage,
            revision,
            ops: if no_op {
                vec![]
            } else {
                vec![Op::ReplaceNode { id, node }]
            },
            result: ImageEditResult {
                node: id,
                bounds,
                changed_tiles,
                no_op,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alto_doc::{DocumentMeta, Node};
    fn setup() -> (Engine, DocHandle, NodeId) {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(300, 10)).unwrap();
        let mut n = Node::raster("test");
        let id = n.id;
        let b = ImageBounds {
            x: 254,
            y: -1,
            width: 3,
            height: 2,
        };
        let mut data = BTreeMap::new();
        for (x, y, p) in [
            (254, -1, [255, 0, 0, 255]),
            (255, -1, [0, 255, 0, 255]),
            (256, -1, [0, 0, 255, 255]),
            (254, 0, [255, 255, 255, 255]),
        ] {
            write(&mut data, x, y, p);
        }
        let mut tiles = TileMap::new(PixelFormat::Rgba8);
        for (k, v) in data {
            let hash = e
                .put_tile(TileData::new(PixelFormat::Rgba8, v).unwrap())
                .unwrap();
            tiles.tiles.insert(k, hash);
        }
        n.kind = NodeKind::Raster { bounds: b, tiles };
        let root = e.document(h).unwrap().root;
        e.transact(
            h,
            "add",
            vec![Op::InsertNode {
                node: n,
                parent: root,
                index: 0,
            }],
            0,
        )
        .unwrap();
        (e, h, id)
    }
    fn edit(e: &mut Engine, h: DocHandle, request: ImageEdit) -> ImageEditResult {
        let intent = request.intent();
        let p = e.prepare_image_edit(h, request).unwrap();
        e.apply_image_edit(h, p, intent).unwrap()
    }
    #[test]
    fn rotate_crop_flip_across_tiles_and_negative_origin_undo_save() {
        let (mut e, h, id) = setup();
        let before = e.snapshot(h).unwrap();
        let r = edit(
            &mut e,
            h,
            ImageEdit::Rotate {
                node: id,
                quarter_turns: 1,
            },
        );
        assert_eq!((r.bounds.width, r.bounds.height), (2, 3));
        assert_eq!(
            e.pick_color(h, id, [255.0, -1.0]).unwrap().bytes,
            [255, 0, 0, 255]
        );
        assert_eq!(
            e.pick_color(h, id, [255.0, 1.0]).unwrap().bytes,
            [0, 0, 255, 255]
        );
        e.undo(h).unwrap();
        assert_eq!(e.snapshot(h).unwrap(), before);
        let b = ImageBounds {
            x: 255,
            y: -1,
            width: 2,
            height: 1,
        };
        edit(
            &mut e,
            h,
            ImageEdit::Crop {
                node: id,
                bounds: b,
            },
        );
        assert!(e.pick_color(h, id, [254.0, -1.0]).is_err());
        let c = e.commit(h, "cropped", 1, None, false).unwrap();
        let bytes = e.save_package(h).unwrap();
        let (d, w) = e.open_package(&bytes).unwrap();
        assert!(w.is_empty());
        assert_eq!(e.version_status(d).unwrap().head, Some(c.hash));
        assert_eq!(
            e.pick_color(d, id, [256.0, -1.0]).unwrap().bytes,
            [0, 0, 255, 255]
        );
        e.undo(h).unwrap();
        assert_eq!(e.snapshot(h).unwrap(), before);
        edit(
            &mut e,
            h,
            ImageEdit::Flip {
                node: id,
                axis: FlipAxis::Horizontal,
            },
        );
        assert_eq!(
            e.pick_color(h, id, [254.0, -1.0]).unwrap().bytes,
            [0, 0, 255, 255]
        );
    }
    #[test]
    fn resize_transparency_and_failed_stale_prepare_are_atomic() {
        let (mut e, h, id) = setup();
        edit(
            &mut e,
            h,
            ImageEdit::Resize {
                node: id,
                width: 1,
                height: 1,
                filter: ImageFilter::Bilinear,
            },
        );
        assert_eq!(
            e.pick_color(h, id, [254.0, -1.0]).unwrap().bytes,
            [0, 255, 0, 128]
        );
        let before = e.snapshot(h).unwrap();
        assert!(
            e.prepare_image_edit(
                h,
                ImageEdit::Resize {
                    node: id,
                    width: 65536,
                    height: 65536,
                    filter: ImageFilter::Nearest
                }
            )
            .is_err()
        );
        assert_eq!(e.snapshot(h).unwrap(), before);
        let prepared = e
            .prepare_image_edit(
                h,
                ImageEdit::Flip {
                    node: id,
                    axis: FlipAxis::Horizontal,
                },
            )
            .unwrap();
        e.undo(h).unwrap();
        assert!(e.apply_image_edit(h, prepared, "stale").is_err());
    }
    /// A one-row image of the given pixels at the origin.
    fn row(pixels: &[[u8; 4]]) -> (Engine, DocHandle, NodeId) {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(300, 10)).unwrap();
        let mut n = Node::raster("row");
        let id = n.id;
        let mut data = BTreeMap::new();
        for (x, p) in pixels.iter().enumerate() {
            write(&mut data, x as i32, 0, *p);
        }
        let mut tiles = TileMap::new(PixelFormat::Rgba8);
        for (k, v) in data {
            let hash = e
                .put_tile(TileData::new(PixelFormat::Rgba8, v).unwrap())
                .unwrap();
            tiles.tiles.insert(k, hash);
        }
        n.kind = NodeKind::Raster {
            bounds: ImageBounds {
                x: 0,
                y: 0,
                width: pixels.len() as u32,
                height: 1,
            },
            tiles,
        };
        let root = e.document(h).unwrap().root;
        e.transact(
            h,
            "add",
            vec![Op::InsertNode {
                node: n,
                parent: root,
                index: 0,
            }],
            0,
        )
        .unwrap();
        (e, h, id)
    }
    fn resize_row(pixels: &[[u8; 4]], width: u32, filter: ImageFilter) -> Vec<[u8; 4]> {
        let (mut e, h, id) = row(pixels);
        edit(
            &mut e,
            h,
            ImageEdit::Resize {
                node: id,
                width,
                height: 1,
                filter,
            },
        );
        (0..width)
            .map(|x| e.pick_color(h, id, [f64::from(x), 0.0]).unwrap().bytes)
            .collect()
    }
    #[test]
    fn area_filter_averages_premultiplied_source_exactly() {
        let black = [0, 0, 0, 255];
        let white = [255, 255, 255, 255];
        assert_eq!(
            resize_row(&[black, white, black, white], 2, ImageFilter::Area),
            [[128, 128, 128, 255]; 2],
            "bilinear would alias this pattern"
        );
        // Three source pixels into two: 1.5 source pixels each, with exact weights.
        assert_eq!(
            resize_row(&[black, white, black], 2, ImageFilter::Area),
            [[85, 85, 85, 255]; 2]
        );
        // Transparent pixels do not darken the color.
        assert_eq!(
            resize_row(&[[255, 0, 0, 255], [0, 0, 0, 0]], 1, ImageFilter::Area),
            [[255, 0, 0, 128]]
        );
    }
    #[test]
    fn catmull_rom_keeps_flat_areas_and_stays_in_range() {
        let flat = [[10, 20, 30, 255]; 4];
        assert_eq!(
            resize_row(&flat, 9, ImageFilter::CatmullRom),
            [[10, 20, 30, 255]; 9]
        );
        // Enlarging an edge overshoots only up to the clamp, and passes through source
        // values where the destination center lands exactly on a source center.
        let edge = [
            [0, 0, 0, 255],
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [255, 255, 255, 255],
        ];
        let up = resize_row(&edge, 12, ImageFilter::CatmullRom);
        assert_eq!(up[0], [0, 0, 0, 255]);
        assert_eq!(up[11], [255, 255, 255, 255]);
        assert!(up.windows(2).all(|w| w[0][3] == 255 && w[1][3] == 255));
        let bilinear = resize_row(&edge, 12, ImageFilter::Bilinear);
        assert_ne!(up, bilinear, "the kernels differ");
        // A half-transparent pixel never gets more color than its alpha allows.
        let soft = resize_row(
            &[[255, 0, 0, 255], [0, 0, 0, 0], [255, 0, 0, 255]],
            7,
            ImageFilter::CatmullRom,
        );
        assert!(soft.iter().all(|p| p[3] == 0 || p[0] == 255), "{soft:?}");
    }
    #[test]
    fn filters_are_reproducible_and_undo_and_save() {
        let pixels: Vec<[u8; 4]> = (0..40u8).map(|i| [i * 6, 255 - i, i, 200 + i]).collect();
        for filter in [ImageFilter::CatmullRom, ImageFilter::Area] {
            assert_eq!(
                resize_row(&pixels, 13, filter),
                resize_row(&pixels, 13, filter)
            );
        }
        let (mut e, h, id) = row(&pixels);
        let before = e.snapshot(h).unwrap();
        edit(
            &mut e,
            h,
            ImageEdit::Resize {
                node: id,
                width: 13,
                height: 1,
                filter: ImageFilter::Area,
            },
        );
        let bytes = e.save_package(h).unwrap();
        assert!(Engine::new().open_package(&bytes).is_ok());
        e.undo(h).unwrap();
        assert_eq!(e.snapshot(h).unwrap(), before);
    }
    #[test]
    fn fill_uses_four_neighbors_and_seed_comparison_and_journal() {
        let (mut e, h, id) = setup();
        let before = e.save_package(h).unwrap();
        let edit = ImageEdit::Fill {
            node: id,
            point: [256.0, 0.0],
            color: [1.0, 0.0, 1.0, 1.0],
            tolerance: 0,
            selection: SelectionMode::Ignore,
        };
        let p = e.prepare_image_edit(h, edit).unwrap();
        let ops = p.ops.clone();
        let result = e.apply_image_edit(h, p, "image.fill").unwrap();
        assert!(!result.no_op);
        assert_eq!(
            e.pick_color(h, id, [255.0, 0.0]).unwrap().bytes,
            [255, 0, 255, 255]
        );
        assert_eq!(
            e.pick_color(h, id, [256.0, -1.0]).unwrap().bytes,
            [0, 0, 255, 255]
        );
        let journal = e
            .journal_record(h, crate::JournalKind::Transact, "image.fill", &ops)
            .unwrap();
        let (mut f, d) = {
            let mut f = Engine::new();
            let (d, _) = f.open_package(&before).unwrap();
            (f, d)
        };
        f.replay_record(d, &journal).unwrap();
        assert_eq!(f.snapshot(d).unwrap(), e.snapshot(h).unwrap());
    }
    #[test]
    fn gradient_is_premultiplied_and_empty_image_bounds_survive() {
        let (mut e, h, id) = setup();
        let options = GradientOptions {
            kind: GradientKind::Linear,
            start: [254.0, 0.0],
            end: [257.0, 0.0],
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: [1.0, 0.0, 0.0, 0.0],
                },
                GradientStop {
                    offset: 1.0,
                    color: [0.0, 1.0, 0.0, 1.0],
                },
            ],
            opacity: 1.0,
            selection: SelectionMode::Ignore,
        };
        edit(&mut e, h, ImageEdit::Gradient { node: id, options });
        let px = e.pick_color(h, id, [255.0, 0.0]).unwrap().bytes;
        assert_eq!(px, [0, 255, 0, 128]);
        let b = ImageBounds {
            x: 256,
            y: 0,
            width: 1,
            height: 1,
        };
        e.undo(h).unwrap();
        edit(
            &mut e,
            h,
            ImageEdit::Crop {
                node: id,
                bounds: b,
            },
        );
        assert_eq!(e.pick_color(h, id, [256.0, 0.0]).unwrap().bytes, [0; 4]);
        let bytes = e.save_package(h).unwrap();
        let (d, _) = e.open_package(&bytes).unwrap();
        let NodeKind::Raster { bounds, .. } = e.document(d).unwrap().node(id).unwrap().kind else {
            panic!()
        };
        assert_eq!(bounds, b);
    }
}

#[cfg(test)]
mod reference_validation_tests {
    use super::*;
    use alto_doc::{DocumentMeta, Node};
    fn crafted_package(key: TileKey, outside_pixel: bool) -> BTreeMap<String, Vec<u8>> {
        let mut engine = Engine::new();
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(512, 256))
            .unwrap();
        let node = Node::raster("crafted");
        let id = node.id;
        let mut pixels = vec![0; 262144];
        pixels[0..4].copy_from_slice(&[255, 0, 0, 255]);
        if outside_pixel {
            pixels[4..8].copy_from_slice(&[0, 255, 0, 255]);
        }
        let hash = engine
            .put_tile(TileData::new(PixelFormat::Rgba8, pixels).unwrap())
            .unwrap();
        engine
            .transact(
                doc,
                "source",
                vec![
                    Op::InsertNode {
                        node,
                        parent: engine.document(doc).unwrap().root,
                        index: 0,
                    },
                    Op::SetTiles {
                        layer: id,
                        changes: vec![alto_doc::TileChange {
                            key,
                            before: None,
                            after: Some(hash),
                        }],
                    },
                ],
                0,
            )
            .unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&engine.snapshot(doc).unwrap()).unwrap();
        let node = value["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|n| n["id"].as_str() == Some(id.0.to_string().as_str()))
            .unwrap();
        node["kind"]["bounds"] = serde_json::json!({"x":0,"y":0,"width":1,"height":1});
        let snapshot = alto_core::json::canonical_bytes(&value).unwrap();
        let bytes = alto_store::encode_package(
            &snapshot,
            BTreeMap::from([(hash, engine.encoded(hash).unwrap().unwrap())]),
        )
        .unwrap();
        alto_store::read_entries(&bytes).unwrap()
    }
    #[test]
    fn normal_and_recovery_open_reject_nonzero_boundary_padding() {
        let entries = crafted_package(TileKey { tx: 0, ty: 0 }, true);
        for deferred in [false, true] {
            let mut engine = Engine::new();
            let result = if deferred {
                engine.open_recovery_entries(entries.clone(), &Default::default())
            } else {
                engine.open_entries(entries.clone())
            };
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("nonzero pixels outside image bounds")
            );
            assert!(engine.documents.is_empty());
        }
        let valid = crafted_package(TileKey { tx: 0, ty: 0 }, false);
        for deferred in [false, true] {
            let mut engine = Engine::new();
            let (doc, warnings) = if deferred {
                engine
                    .open_recovery_entries(valid.clone(), &Default::default())
                    .unwrap()
            } else {
                engine.open_entries(valid.clone()).unwrap()
            };
            assert!(warnings.is_empty());
            let saved = engine.save_package(doc).unwrap();
            assert!(Engine::new().open_package(&saved).is_ok());
        }
    }
    #[test]
    fn snapshot_rejects_tiles_disjoint_from_image_bounds() {
        let entries = crafted_package(TileKey { tx: 1, ty: 0 }, false);
        let package = alto_store::decode_entries(entries.clone()).unwrap();
        assert!(
            alto_doc::Document::from_snapshot_bytes(&package.snapshot)
                .unwrap_err()
                .to_string()
                .contains("does not intersect image bounds")
        );
        assert!(Engine::new().open_entries(entries.clone()).is_err());
        assert!(
            Engine::new()
                .open_recovery_entries(entries, &Default::default())
                .is_err()
        );
    }
    #[test]
    fn rgba_tile_cannot_be_attached_as_an_r8_mask() {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(16, 16)).unwrap();
        let n = Node::raster("raster");
        let id = n.id;
        let root = e.document(h).unwrap().root;
        e.transact(
            h,
            "add",
            vec![Op::InsertNode {
                node: n,
                parent: root,
                index: 0,
            }],
            0,
        )
        .unwrap();
        let hash = e
            .put_tile(TileData::new(PixelFormat::Rgba8, vec![255; 262144]).unwrap())
            .unwrap();
        let before = e.snapshot(h).unwrap();
        let revision = e.document(h).unwrap().revision;
        let history = e.history_state(h).unwrap();
        let mut mask = TileMap::new(PixelFormat::R8);
        mask.tiles.insert(TileKey { tx: 0, ty: 0 }, hash);
        let effect = alto_doc::Effect::Invert {
            selection: Some(mask.clone()),
            extra: Default::default(),
        };
        let invalid = [
            Op::SetMask {
                layer: id,
                mask: Some(mask),
            },
            Op::SetProps {
                id,
                patch: alto_doc::PropPatch {
                    effects: Some(vec![effect.clone()]),
                    ..Default::default()
                },
            },
            Op::SetAdjustment {
                id,
                adjustment: effect,
            },
        ];
        for op in invalid {
            let error = e
                .transact(h, "invalid mask reference", vec![op], revision)
                .unwrap_err();
            assert!(
                error.to_string().contains("mask tile requires R8"),
                "{error}"
            );
        }
        assert_eq!(e.snapshot(h).unwrap(), before);
        assert_eq!(e.document(h).unwrap().revision, revision);
        assert_eq!(e.history_state(h).unwrap(), history);
    }
}
