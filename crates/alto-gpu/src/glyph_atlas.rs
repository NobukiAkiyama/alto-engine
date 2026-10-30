//! Size-banded glyph atlas for small text (design §3.9: large glyphs take the vector
//! path, small ones the atlas).
//!
//! An entry is a glyph's coverage rasterized at the exact display scale: the key holds
//! the font-units -> output-pixel matrix quantized to 1/4 px per em (the size band, which
//! also covers rotation, tate-chu-yoko compression and horizontal scale) and the glyph
//! origin's sub-pixel phase quantized to 1/4 px. Drawing is then a 1:1 copy of atlas
//! texels to output pixels, so the atlas never magnifies a bitmap. Display levels are
//! cached separately and each level measures its own em size, so zooming in past the
//! threshold switches the glyphs back to the vector path.
//!
//! Like the display tile cache, everything here is rebuildable: the outlines travel
//! with the scene, so after device loss (or eviction) entries are rasterized again.
use alto_vector::{GlyphOutline, PathCommand};
use std::collections::HashMap;

/// Glyphs whose em is below this many output pixels on the display level use the atlas.
pub const ATLAS_MAX_EM_PX: f64 = 24.0;
/// Size-band steps per pixel per em.
pub const STEPS: f64 = 4.0;
/// Sub-pixel phases of the glyph origin per pixel on each axis.
pub const PHASES: f64 = 4.0;
/// Largest bitmap side (including padding); bigger glyphs fall back to the vector path.
pub const MAX_GLYPH_PX: u32 = 96;
pub const ATLAS_SIDE: u32 = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    /// Which rasterizer made the entry: 0 for v1 / v2 (`rasterize`, f64), 1 for
    /// v3 (`glyph_q8`, integer). The atlas texture is shared, so the key keeps
    /// the two apart (doc/design/2026-10-28-v3-integer-text.md "輪郭と鍵").
    pub raster: u8,
    /// v3: blake3 of the bake's outline record (`text_bake::outline_record`), so
    /// equal shapes share an entry; zero for v1 / v2.
    pub outline_hash: [u8; 32],
    /// v1 / v2: the outline's `(font, glyph)` identity; zero for v3.
    pub outline: u64,
    /// Font units -> output pixels, in 1/STEPS pixels per em: [a, b, c, d].
    pub matrix: [i32; 4],
    /// Sub-pixel phase of the glyph origin, 0..PHASES on each axis.
    pub phase: [u8; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Where a glyph instance lands on the output grid and which texels it copies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub key: GlyphKey,
    /// Integer output pixel of the bitmap's top-left corner.
    pub dst: [i64; 2],
    pub w: u32,
    pub h: u32,
    /// Font units -> bitmap pixels used to rasterize this entry.
    pub raster: [f64; 6],
}

/// Largest singular value of a 2x2 linear map [a, b, c, d] (column-major like Affine2).
pub fn max_scale(m: [f64; 4]) -> f64 {
    let [a, b, c, d] = m;
    let s = a * a + b * b + c * c + d * d;
    let det = a * d - b * c;
    ((s + (s * s - 4.0 * det * det).max(0.0).sqrt()) / 2.0).sqrt()
}

/// Places a glyph whose font-units -> output-pixel transform is `m`. None when the
/// transform is degenerate or the bitmap would exceed `MAX_GLYPH_PX`.
pub fn place(outline: &GlyphOutline, m: [f64; 6]) -> Option<Placement> {
    let upem = outline.upem;
    if !upem.is_finite() || upem <= 0.0 || m.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let mut q = [0i32; 4];
    for (i, v) in q.iter_mut().enumerate() {
        let band = (m[i] * upem * STEPS).round();
        if band.abs() > 1e6 {
            return None;
        }
        *v = band as i32;
    }
    let lin = q.map(|v| v as f64 / STEPS / upem);
    if (lin[0] * lin[3] - lin[1] * lin[2]).abs() < 1e-12 {
        return None;
    }
    let mut origin = [0i64; 2];
    let mut phase = [0u8; 2];
    for k in 0..2 {
        let o = m[4 + k];
        let mut i = o.floor();
        let mut p = ((o - i) * PHASES).round();
        if p >= PHASES {
            i += 1.0;
            p = 0.0;
        }
        if i.abs() > 1e15 {
            return None;
        }
        origin[k] = i as i64;
        phase[k] = p as u8;
    }
    let sub = phase.map(|p| p as f64 / PHASES);
    let b = outline.bounds;
    let corners = [[b[0], b[1]], [b[2], b[1]], [b[0], b[3]], [b[2], b[3]]]
        .map(|p| [lin[0] * p[0] + lin[2] * p[1], lin[1] * p[0] + lin[3] * p[1]]);
    let min = |k: usize| corners.iter().map(|c| c[k]).fold(f64::INFINITY, f64::min);
    let max = |k: usize| {
        corners
            .iter()
            .map(|c| c[k])
            .fold(f64::NEG_INFINITY, f64::max)
    };
    // One pixel of padding on each side keeps bilinear-free copies and the rasterizer's
    // accumulation inside the bitmap.
    let x0 = (min(0) + sub[0]).floor() - 1.0;
    let y0 = (min(1) + sub[1]).floor() - 1.0;
    let x1 = (max(0) + sub[0]).ceil() + 1.0;
    let y1 = (max(1) + sub[1]).ceil() + 1.0;
    let (w, h) = (x1 - x0, y1 - y0);
    if !(w >= 1.0 && h >= 1.0) || w > MAX_GLYPH_PX as f64 || h > MAX_GLYPH_PX as f64 {
        return None;
    }
    Some(Placement {
        key: GlyphKey {
            raster: 0,
            outline_hash: [0; 32],
            outline: outline.key,
            matrix: q,
            phase,
        },
        dst: [origin[0] + x0 as i64, origin[1] + y0 as i64],
        w: w as u32,
        h: h as u32,
        raster: [lin[0], lin[1], lin[2], lin[3], sub[0] - x0, sub[1] - y0],
    })
}

/// Exact-area coverage (signed-area accumulation, non-zero approximated by |winding|
/// clamped to 1) of an outline mapped by `m` into a `w` x `h` R8 bitmap.
pub fn rasterize(outline: &GlyphOutline, m: [f64; 6], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut acc = vec![0f32; w * h + 2];
    let tf = |x: f64, y: f64| [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]];
    let mut line = |p0: [f64; 2], p1: [f64; 2]| accumulate(&mut acc, w, h, p0, p1);
    let (mut start, mut cur) = ([0.0; 2], [0.0; 2]);
    for c in &outline.data.commands {
        match *c {
            PathCommand::Move { x, y } => {
                if cur != start {
                    line(cur, start);
                }
                start = tf(x, y);
                cur = start;
            }
            PathCommand::Line { x, y } => {
                let p = tf(x, y);
                line(cur, p);
                cur = p;
            }
            PathCommand::Quad { cx, cy, x, y } => {
                let (c, p) = (tf(cx, cy), tf(x, y));
                // Chord error of n uniform segments is |p0 - 2c + p| / (4 n^2): <= 0.025 px.
                let dd = (cur[0] - 2.0 * c[0] + p[0]).hypot(cur[1] - 2.0 * c[1] + p[1]);
                let n = (dd * 10.0).sqrt().ceil().clamp(1.0, 64.0) as usize;
                let p0 = cur;
                for i in 1..=n {
                    let t = i as f64 / n as f64;
                    let u = 1.0 - t;
                    let q = [
                        u * u * p0[0] + 2.0 * u * t * c[0] + t * t * p[0],
                        u * u * p0[1] + 2.0 * u * t * c[1] + t * t * p[1],
                    ];
                    line(cur, q);
                    cur = q;
                }
            }
            PathCommand::Cubic {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                let (a, b, p) = (tf(c1x, c1y), tf(c2x, c2y), tf(x, y));
                let p0 = cur;
                let dd = (p0[0] - 2.0 * a[0] + b[0])
                    .hypot(p0[1] - 2.0 * a[1] + b[1])
                    .max((a[0] - 2.0 * b[0] + p[0]).hypot(a[1] - 2.0 * b[1] + p[1]));
                let n = (dd * 30.0).sqrt().ceil().clamp(1.0, 64.0) as usize;
                for i in 1..=n {
                    let t = i as f64 / n as f64;
                    let u = 1.0 - t;
                    let (k0, k1, k2, k3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    let q = [
                        k0 * p0[0] + k1 * a[0] + k2 * b[0] + k3 * p[0],
                        k0 * p0[1] + k1 * a[1] + k2 * b[1] + k3 * p[1],
                    ];
                    line(cur, q);
                    cur = q;
                }
            }
            PathCommand::Close => {
                if cur != start {
                    line(cur, start);
                }
                cur = start;
            }
        }
    }
    if cur != start {
        line(cur, start);
    }
    let mut sum = 0f32;
    acc[..w * h]
        .iter()
        .map(|a| {
            sum += a;
            (sum.abs().min(1.0) * 255.0 + 0.5) as u8
        })
        .collect()
}

/// Adds one line's signed area to the accumulation buffer (font-rs style).
fn accumulate(acc: &mut [f32], w: usize, h: usize, p0: [f64; 2], p1: [f64; 2]) {
    if p0[1] == p1[1] {
        return;
    }
    let (dir, p0, p1) = if p0[1] < p1[1] {
        (1.0, p0, p1)
    } else {
        (-1.0, p1, p0)
    };
    let clampx = |x: f64| x.clamp(0.0, (w - 1) as f64);
    let dxdy = (p1[0] - p0[0]) / (p1[1] - p0[1]);
    let mut x = p0[0];
    let y_start = p0[1].max(0.0);
    if p0[1] < 0.0 {
        x -= p0[1] * dxdy;
    }
    let y_end = p1[1].min(h as f64);
    let mut y = y_start.floor() as usize;
    while (y as f64) < y_end {
        let ls = y * w;
        let dy = ((y + 1) as f64).min(p1[1]) - (y as f64).max(p0[1]);
        let xnext = x + dxdy * dy;
        let d = (dy * dir) as f32;
        let (x0, x1) = if x < xnext {
            (clampx(x), clampx(xnext))
        } else {
            (clampx(xnext), clampx(x))
        };
        let x0f = x0.floor();
        let x0i = x0f as usize;
        let x1c = x1.ceil();
        let x1i = x1c as usize;
        if x1i <= x0i + 1 {
            let xmf = (0.5 * (x0 + x1) - x0f) as f32;
            acc[ls + x0i] += d - d * xmf;
            acc[ls + x0i + 1] += d * xmf;
        } else {
            let s = (1.0 / (x1 - x0)) as f32;
            let x0r = (x0 - x0f) as f32;
            let a0 = 0.5 * s * (1.0 - x0r) * (1.0 - x0r);
            let x1r = (x1 - x1c + 1.0) as f32;
            let am = 0.5 * s * x1r * x1r;
            acc[ls + x0i] += d * a0;
            if x1i == x0i + 2 {
                acc[ls + x0i + 1] += d * (1.0 - a0 - am);
            } else {
                let a1 = s * (1.5 - x0r);
                acc[ls + x0i + 1] += d * (a1 - a0);
                for xi in x0i + 2..x1i - 1 {
                    acc[ls + xi] += d * s;
                }
                let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                acc[ls + x1i - 1] += d * (1.0 - a2 - am);
            }
            acc[ls + x1i] += d * am;
        }
        x = xnext;
        y += 1;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtlasStats {
    pub entries: usize,
    pub shelves: usize,
    /// Bitmaps rasterized and uploaded (misses).
    pub rasterized: u64,
    pub hits: u64,
    /// Entries dropped to make room.
    pub evicted: u64,
    /// Whole-atlas resets (device loss or a fully unpinned atlas out of space).
    pub resets: u64,
    /// Glyph instances drawn from the atlas / runs and glyphs sent to the vector path.
    pub atlas_glyphs: u64,
    pub vector_runs: u64,
    pub fallback_glyphs: u64,
}

struct Shelf {
    y: u32,
    h: u32,
    x: u32,
    /// Epoch of the last lookup or insert; entries used in the current epoch are pinned
    /// because their texels may be read by commands not yet submitted.
    used: u64,
    keys: Vec<GlyphKey>,
}

/// Shelf packer with whole-shelf LRU eviction for the R8 atlas texture.
pub struct GlyphAtlas {
    w: u32,
    h: u32,
    shelves: Vec<Shelf>,
    next_y: u32,
    map: HashMap<GlyphKey, (AtlasRect, usize)>,
    epoch: u64,
    pub stats: AtlasStats,
}

fn shelf_height(h: u32) -> u32 {
    // Height classes keep shelves reusable across nearby sizes.
    if h <= 32 {
        h.div_ceil(4) * 4
    } else {
        h.div_ceil(8) * 8
    }
}

impl GlyphAtlas {
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            shelves: vec![],
            next_y: 0,
            map: HashMap::new(),
            epoch: 0,
            stats: AtlasStats::default(),
        }
    }
    pub fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn stats(&self) -> AtlasStats {
        AtlasStats {
            entries: self.map.len(),
            shelves: self.shelves.len(),
            ..self.stats
        }
    }
    /// Starts a new epoch. Entries used in earlier epochs may be evicted.
    pub fn begin(&mut self, epoch: u64) {
        self.epoch = self.epoch.max(epoch);
    }
    pub fn get(&mut self, key: &GlyphKey) -> Option<AtlasRect> {
        let (rect, shelf) = *self.map.get(key)?;
        self.shelves[shelf].used = self.epoch;
        self.stats.hits += 1;
        Some(rect)
    }
    /// Reserves a `w` x `h` rectangle for `key`. None when the glyph is too large or all
    /// space is pinned by the current epoch (the caller draws it as a vector instead).
    pub fn insert(&mut self, key: GlyphKey, w: u32, h: u32) -> Option<AtlasRect> {
        if let Some(r) = self.get(&key) {
            return Some(r);
        }
        if w == 0 || h == 0 || w > self.w || h > self.h {
            return None;
        }
        let class = shelf_height(h).min(self.h);
        let fits = |s: &Shelf| s.h >= h && s.h <= class + class / 2 && s.x + w <= self.w;
        let index = if let Some(i) = self.shelves.iter().position(fits) {
            i
        } else if self.next_y + class <= self.h {
            self.shelves.push(Shelf {
                y: self.next_y,
                h: class,
                x: 0,
                used: self.epoch,
                keys: vec![],
            });
            self.next_y += class;
            self.shelves.len() - 1
        } else if let Some(i) = self
            .shelves
            .iter()
            .enumerate()
            .filter(|(_, s)| s.used < self.epoch && s.h >= h)
            .min_by_key(|(_, s)| (s.used, s.h))
            .map(|(i, _)| i)
        {
            let shelf = &mut self.shelves[i];
            for k in shelf.keys.drain(..) {
                self.map.remove(&k);
                self.stats.evicted += 1;
            }
            shelf.x = 0;
            i
        } else if self.shelves.iter().all(|s| s.used < self.epoch) {
            // Nothing pinned but no shelf is tall enough: start over with this glyph.
            self.stats.evicted += self.map.len() as u64;
            self.reset();
            return self.insert(key, w, h);
        } else {
            return None;
        };
        let shelf = &mut self.shelves[index];
        let rect = AtlasRect {
            x: shelf.x,
            y: shelf.y,
            w,
            h,
        };
        shelf.x += w;
        shelf.used = self.epoch;
        shelf.keys.push(key);
        self.map.insert(key, (rect, index));
        self.stats.rasterized += 1;
        Some(rect)
    }
    fn reset(&mut self) {
        self.shelves.clear();
        self.map.clear();
        self.next_y = 0;
        self.stats.resets += 1;
    }
    /// Texture contents are gone: every entry must be rasterized again from its outline.
    pub fn on_device_lost(&mut self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alto_vector::PathData;

    fn key(n: u64) -> GlyphKey {
        GlyphKey {
            raster: 0,
            outline_hash: [0; 32],
            outline: n,
            matrix: [48, 0, 0, -48],
            phase: [0, 0],
        }
    }
    fn overlaps(a: AtlasRect, b: AtlasRect) -> bool {
        a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
    }
    fn rect_outline(x0: f64, y0: f64, x1: f64, y1: f64) -> GlyphOutline {
        let l = |x, y| PathCommand::Line { x, y };
        GlyphOutline::new(
            1,
            1000.0,
            PathData {
                commands: vec![
                    PathCommand::Move { x: x0, y: y0 },
                    l(x1, y0),
                    l(x1, y1),
                    l(x0, y1),
                    PathCommand::Close,
                ],
            },
        )
    }

    #[test]
    fn packs_without_overlap_and_reuses_entries() {
        let mut atlas = GlyphAtlas::new(64, 64);
        atlas.begin(1);
        let mut rects = vec![];
        for i in 0..12 {
            let (w, h) = (9 + i % 5, 7 + i % 9);
            let r = atlas.insert(key(i as u64), w, h).unwrap();
            assert!(r.x + r.w <= 64 && r.y + r.h <= 64);
            assert!((r.w, r.h) == (w, h));
            rects.push(r);
        }
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(!overlaps(*a, *b), "{a:?} overlaps {b:?}");
            }
        }
        // A second request is a hit with the same texels, not a new raster.
        let before = atlas.stats().rasterized;
        assert_eq!(atlas.insert(key(3), 12, 10), Some(rects[3]));
        assert_eq!(atlas.get(&key(5)), Some(rects[5]));
        assert_eq!(atlas.stats().rasterized, before);
        assert_eq!(atlas.stats().entries, 12);
        // Bigger than the texture: refused.
        assert_eq!(atlas.insert(key(99), 65, 4), None);
    }

    #[test]
    fn evicts_least_recently_used_shelf_and_never_the_current_epoch() {
        let mut atlas = GlyphAtlas::new(32, 32);
        // Four 8-px shelves, each holding two 16-px glyphs.
        for (epoch, i) in [
            (1, 0),
            (1, 1),
            (2, 2),
            (2, 3),
            (3, 4),
            (3, 5),
            (4, 6),
            (4, 7),
        ] {
            atlas.begin(epoch);
            atlas.insert(key(i), 16, 8).unwrap();
        }
        assert_eq!(atlas.stats().shelves, 4);
        // Touch the oldest shelf so the shelf of epoch 2 becomes the LRU one.
        atlas.begin(5);
        assert!(atlas.get(&key(0)).is_some());
        let r = atlas.insert(key(100), 16, 8).unwrap();
        assert!(atlas.get(&key(2)).is_none() && atlas.get(&key(3)).is_none());
        assert!(atlas.get(&key(0)).is_some() && atlas.get(&key(1)).is_some());
        assert_eq!((r.y, atlas.stats().evicted), (8, 2));
        // Everything is pinned by epoch 5 now except epochs 3 and 4; fill those...
        for i in 200..205 {
            assert!(atlas.insert(key(i), 16, 8).is_some());
        }
        // ...and the next glyph cannot evict anything used in this epoch.
        assert_eq!(atlas.insert(key(300), 16, 8), None);
        assert_eq!(atlas.stats().entries, 8);
        // A later epoch may evict again.
        atlas.begin(6);
        assert!(atlas.insert(key(300), 16, 8).is_some());
    }

    #[test]
    fn tall_glyph_resets_an_unpinned_atlas_and_device_loss_clears() {
        let mut atlas = GlyphAtlas::new(32, 32);
        atlas.begin(1);
        for i in 0..8 {
            atlas.insert(key(i), 16, 8).unwrap();
        }
        // In the same epoch a tall glyph has nowhere to go.
        assert_eq!(atlas.insert(key(50), 8, 24), None);
        atlas.begin(2);
        let r = atlas.insert(key(50), 8, 24).unwrap();
        assert_eq!((r.x, r.y), (0, 0));
        assert_eq!(atlas.stats().entries, 1);
        assert_eq!(atlas.stats().resets, 1);
        atlas.on_device_lost();
        assert!(atlas.is_empty() && atlas.get(&key(50)).is_none());
        assert_eq!(atlas.stats().resets, 2);
    }

    #[test]
    fn rasterizer_matches_exact_area() {
        // A 1000-unit square at 12 px/em shifted by a quarter pixel: 12 x 12 px of ink.
        let o = rect_outline(0.0, 0.0, 1000.0, 1000.0);
        let m = [0.012, 0.0, 0.0, -0.012, 2.25, 14.5];
        let px = rasterize(&o, m, 17, 17);
        let ink: f64 = px.iter().map(|&v| v as f64 / 255.0).sum();
        assert!((ink - 144.0).abs() < 0.2, "ink {ink}");
        // Edge pixels carry the fractional coverage: column 2 is 3/4 covered.
        assert_eq!(px[8 * 17 + 2], 191);
        assert_eq!(px[8 * 17 + 5], 255);
        // A circle (cubic arcs) covers pi r^2 within 1 %.
        let k = 0.5522847498 * 500.0;
        let c = |c1x, c1y, c2x, c2y, x, y| PathCommand::Cubic {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        };
        let circle = GlyphOutline::new(
            2,
            1000.0,
            PathData {
                commands: vec![
                    PathCommand::Move {
                        x: 1000.0,
                        y: 500.0,
                    },
                    c(1000.0, 500.0 + k, 500.0 + k, 1000.0, 500.0, 1000.0),
                    c(500.0 - k, 1000.0, 0.0, 500.0 + k, 0.0, 500.0),
                    c(0.0, 500.0 - k, 500.0 - k, 0.0, 500.0, 0.0),
                    c(500.0 + k, 0.0, 1000.0, 500.0 - k, 1000.0, 500.0),
                    PathCommand::Close,
                ],
            },
        );
        let px = rasterize(&circle, [0.02, 0.0, 0.0, -0.02, 1.0, 21.0], 22, 22);
        let ink: f64 = px.iter().map(|&v| v as f64 / 255.0).sum();
        let exact = std::f64::consts::PI * 100.0;
        assert!(
            (ink / exact - 1.0).abs() < 0.01,
            "circle ink {ink} vs {exact}"
        );
    }

    #[test]
    fn placement_quantizes_size_band_and_subpixel_phase() {
        let o = rect_outline(0.0, -200.0, 600.0, 800.0);
        // 12 px/em, origin at (100.3, 50.9): phases snap to 1/4 px.
        let p = place(&o, [0.012, 0.0, 0.0, -0.012, 100.3, 50.9]).unwrap();
        assert_eq!(p.key.matrix, [48, 0, 0, -48]);
        assert_eq!(p.key.phase, [1, 0]);
        // y 50.9 rounds up to the next pixel with phase 0.
        let q = place(&o, [0.012, 0.0, 0.0, -0.012, 100.3, 50.95]).unwrap();
        assert_eq!((q.key, q.dst), (p.key, p.dst));
        // The bitmap covers the glyph with a pixel of padding.
        let px = rasterize(&o, p.raster, p.w, p.h);
        let ink: f64 = px.iter().map(|&v| v as f64 / 255.0).sum();
        assert!((ink - 7.2 * 12.0).abs() < 0.2, "ink {ink}");
        assert!(
            px[..p.w as usize].iter().all(|&v| v == 0),
            "top padding row"
        );
        // Sizes within the same quarter-pixel band share entries; the next band does not.
        let r = place(&o, [0.01201, 0.0, 0.0, -0.01201, 100.3, 50.9]).unwrap();
        assert_eq!(r.key, p.key);
        let s = place(&o, [0.0125, 0.0, 0.0, -0.0125, 100.3, 50.9]).unwrap();
        assert_ne!(s.key, p.key);
        // Too large for an atlas bitmap.
        assert!(place(&o, [0.2, 0.0, 0.0, -0.2, 0.0, 0.0]).is_none());
        assert!((max_scale([0.0, 2.0, 3.0, 0.0]) - 3.0).abs() < 1e-12);
    }
}
