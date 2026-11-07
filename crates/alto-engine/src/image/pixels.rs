//! Tile-backed pixel reads and writes, and gradient colour math.
use super::*;

pub(super) fn write(out: &mut BTreeMap<TileKey, Vec<u8>>, x: i32, y: i32, p: [u8; 4]) {
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
pub(super) struct Reader<'a> {
    engine: &'a Engine,
    map: &'a TileMap,
    last: Option<(TileKey, Option<std::sync::Arc<TileData>>)>,
}
impl<'a> Reader<'a> {
    pub(super) fn new(engine: &'a Engine, map: &'a TileMap) -> Self {
        Self {
            engine,
            map,
            last: None,
        }
    }
    pub(super) fn get(&mut self, x: i32, y: i32) -> Result<[u8; 4], EngineError> {
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
pub(super) struct Writer {
    tiles: BTreeMap<TileKey, Vec<u8>>,
    current: Option<(TileKey, Vec<u8>)>,
}
impl Writer {
    pub(super) fn put(&mut self, x: i32, y: i32, p: [u8; 4]) {
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
    pub(super) fn finish(mut self) -> BTreeMap<TileKey, Vec<u8>> {
        if let Some((k, tile)) = self.current.take() {
            self.tiles.insert(k, tile);
        }
        self.tiles
    }
}
pub(super) fn rounded(n: i128, d: i128) -> i64 {
    let a = n.unsigned_abs();
    let d = d as u128;
    let q = (a / d + u128::from(a % d >= d.div_ceil(2))) as i64;
    if n < 0 { -q } else { q }
}

pub(super) fn linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
pub(super) fn srgb(c: f64) -> f64 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}
pub(super) fn gradient(options: &GradientOptions, p: [f64; 2]) -> [f64; 4] {
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
