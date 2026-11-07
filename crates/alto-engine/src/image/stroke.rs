//! Brush-like edits drawn along a stroke: validation, dabs and smudge.
use super::*;

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
pub(super) const MAX_STROKE_POINTS: usize = 4096;
pub(super) const MAX_STROKE_RADIUS: f64 = 1024.0;
/// Pixels touched across all dabs of one stroke.
pub(super) const SMUDGE_OPS: u64 = 64 * 1024 * 1024;
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

/// Pixels of a raster being edited by a stroke. Untouched tiles keep their hashes.
pub(super) struct Working<'a> {
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
pub(super) fn lerp_premultiplied(from: [u8; 4], to: [u8; 4], weight: u32) -> [u8; 4] {
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
    pub(super) fn prepare_stroke(
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
                let dabs = stroke.dabs(&points)?;
                let span = (2 * r + 1) as u64;
                if dabs.len() as u64 * span * span > SMUDGE_OPS {
                    return Err(err("RESOURCE_LIMIT", "stamp touches too many pixels"));
                }
                for dab in dabs {
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
                    // The map costs far more per pixel than a tile does.
                    if coverage.len() as u64 > 4 * 1024 * 1024 {
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
