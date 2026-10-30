//! Render version 3 glyph placement and rasterization (implementation order 1
//! of doc/design/2026-10-28-v3-integer-text.md): integers only, from a text
//! bake's Q6 outlines and Q24 / Q30 placements. The v2 functions in
//! `glyph_atlas` (`place`, `rasterize`) are unchanged and never called here.
//!
//! The world transform is the v3 `FixedAffine` (Q32 linear, Q16 translation,
//! PR #41), which postdates the memo's Q16 `WorldQ`; `glyph_map_q` composes it
//! with the placement into the memo's `C` and rounds each component once.

use crate::glyph_atlas::MAX_GLYPH_PX;
use crate::raster_q8::{Command, RasterError, rasterize_glyph};
use alto_core::FixedAffine;
use alto_core::text_bake::{BakeCommand, BakeOutline};

/// Half away from zero, `d > 0`.
fn div_q(n: i128, d: i128) -> Option<i64> {
    if d <= 0 {
        return None;
    }
    let q = (n.unsigned_abs().checked_add(d as u128 / 2)? / d as u128) as i128;
    i64::try_from(if n < 0 { -q } else { q }).ok()
}
fn floor_div(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}
fn ceil_div(a: i64, b: i64) -> i64 {
    -(-a).div_euclid(b)
}

/// `C`: font units to level pixels. `m` is Q24 (level pixels per font unit),
/// `t` is Q30 (level pixels).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyphMapQ {
    pub m: [i64; 4],
    pub t: [i64; 2],
}

/// `C` = level scale ∘ `world` ∘ placement, one rounding per component (memo
/// "字形の写像"). `None` outside `|m| < 2^31`, `|t| < 2^60`, or for a level
/// outside -16..=12.
pub fn glyph_map_q(world: FixedAffine, level: i32, m: [i32; 4], t: [i64; 2]) -> Option<GlyphMapQ> {
    if !(-16..=12).contains(&level) {
        return None;
    }
    // Q32 x Q24 is Q56 and Q32 x Q30 is Q62; dividing by 2^(32 + level) leaves
    // Q24 and Q30 at the level's pixels.
    let d = 1i128 << (32 + level);
    let w = world.m.map(i128::from);
    let (p, pt) = (m.map(i128::from), t.map(i128::from));
    let wt = world.t.map(|v| i128::from(v) << 46);
    let m = [
        div_q(w[0] * p[0] + w[2] * p[1], d)?,
        div_q(w[1] * p[0] + w[3] * p[1], d)?,
        div_q(w[0] * p[2] + w[2] * p[3], d)?,
        div_q(w[1] * p[2] + w[3] * p[3], d)?,
    ];
    let t = [
        div_q(w[0] * pt[0] + w[2] * pt[1] + wt[0], d)?,
        div_q(w[1] * pt[0] + w[3] * pt[1] + wt[1], d)?,
    ];
    (m.iter().all(|v| v.unsigned_abs() < 1 << 31) && t.iter().all(|v| v.unsigned_abs() < 1 << 60))
        .then_some(GlyphMapQ { m, t })
}

/// The atlas band holds glyphs whose em, as the Frobenius norm over sqrt 2, is
/// under `ATLAS_MAX_EM_PX` = 24 (memo "帯の選び方").
pub fn atlas_band(c: &GlyphMapQ, upem: u16) -> bool {
    let frob: u128 = c.m.iter().map(|&v| (v as i128 * v as i128) as u128).sum();
    frob * u128::from(upem) * u128::from(upem) < 2 * 24 * 24 * (1u128 << 48)
}

/// An atlas-band glyph's bitmap: the matrix in 1/4 px/em bands, the position in
/// 1/4 px (integer origin and phase 0..=3), and the bitmap's box in pixels from
/// the origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacementQ {
    pub band: [i64; 4],
    pub origin: [i64; 2],
    pub phase: [i64; 2],
    pub x0: i64,
    pub y0: i64,
    pub w: u32,
    pub h: u32,
}

impl PlacementQ {
    /// The bitmap's top-left level pixel.
    pub fn dst(&self) -> [i64; 2] {
        [self.origin[0] + self.x0, self.origin[1] + self.y0]
    }
}

/// The control-point bounds of an outline, Q6 font units.
fn bounds(outline: &BakeOutline) -> Option<[i64; 4]> {
    let mut b: Option<[i64; 4]> = None;
    for p in points(&outline.commands) {
        let (x, y) = (i64::from(p[0]), i64::from(p[1]));
        b = Some(match b {
            None => [x, y, x, y],
            Some(b) => [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)],
        });
    }
    b
}
fn points(commands: &[BakeCommand]) -> impl Iterator<Item = [i32; 2]> + '_ {
    commands.iter().flat_map(|c| match *c {
        BakeCommand::Move(p) | BakeCommand::Line(p) => vec![p],
        BakeCommand::Quad(a, p) => vec![a, p],
        BakeCommand::Cubic(a, b, p) => vec![a, b, p],
        BakeCommand::Close => vec![],
    })
}

/// The atlas-band point of Q6 `(x, y)`, Q8 from the placement's origin pixel.
fn band_point(p: &PlacementQ, upem: u16, x: i64, y: i64) -> Option<[i64; 2]> {
    let u = i128::from(upem);
    let (x, y) = (i128::from(x), i128::from(y));
    let b = p.band.map(i128::from);
    Some([
        div_q(b[0] * x + b[2] * y + 64 * i128::from(p.phase[0]) * u, u)?,
        div_q(b[1] * x + b[3] * y + 64 * i128::from(p.phase[1]) * u, u)?,
    ])
}

/// The memo's `place_q`: `None` for a degenerate band or a bitmap outside
/// `1..=MAX_GLYPH_PX` (then the glyph is drawn in the direct band).
pub fn place_q(c: &GlyphMapQ, outline: &BakeOutline) -> Option<PlacementQ> {
    let upem = i128::from(outline.upem);
    let band = c.m.map(|v| div_q(i128::from(v) * upem * 4, 1 << 24));
    let band = [band[0]?, band[1]?, band[2]?, band[3]?];
    if i128::from(band[0]) * i128::from(band[3]) == i128::from(band[1]) * i128::from(band[2]) {
        return None;
    }
    let q = [
        div_q(i128::from(c.t[0]), 1 << 28)?,
        div_q(i128::from(c.t[1]), 1 << 28)?,
    ];
    let mut p = PlacementQ {
        band,
        origin: q.map(|v| v.div_euclid(4)),
        phase: q.map(|v| v.rem_euclid(4)),
        x0: 0,
        y0: 0,
        w: 0,
        h: 0,
    };
    let [x0, y0, x1, y1] = bounds(outline)?;
    let corners =
        [[x0, y0], [x1, y0], [x0, y1], [x1, y1]].map(|[x, y]| band_point(&p, outline.upem, x, y));
    let corners = [corners[0]?, corners[1]?, corners[2]?, corners[3]?];
    let min = |a: usize| corners.iter().map(|c| c[a]).min().unwrap();
    let max = |a: usize| corners.iter().map(|c| c[a]).max().unwrap();
    let (bx0, by0) = (floor_div(min(0), 256) - 1, floor_div(min(1), 256) - 1);
    let (bx1, by1) = (ceil_div(max(0), 256) + 1, ceil_div(max(1), 256) + 1);
    let (w, h) = (bx1 - bx0, by1 - by0);
    let limit = i64::from(MAX_GLYPH_PX);
    if !(1..=limit).contains(&w) || !(1..=limit).contains(&h) {
        return None;
    }
    p.x0 = bx0;
    p.y0 = by0;
    p.w = w as u32;
    p.h = h as u32;
    Some(p)
}

fn map_commands(
    commands: &[BakeCommand],
    point: impl Fn(i64, i64) -> Option<[i64; 2]>,
) -> Option<Vec<Command>> {
    let q = |p: [i32; 2]| point(i64::from(p[0]), i64::from(p[1]));
    commands
        .iter()
        .map(|c| {
            Some(match *c {
                BakeCommand::Move(p) => Command::Move(q(p)?),
                BakeCommand::Line(p) => Command::Line(q(p)?),
                BakeCommand::Quad(a, p) => Command::Quad(q(a)?, q(p)?),
                BakeCommand::Cubic(a, b, p) => Command::Cubic(q(a)?, q(b)?, q(p)?),
                BakeCommand::Close => Command::Close,
            })
        })
        .collect()
}

/// The v3 atlas key of an atlas-band glyph: `raster = 1`, the outline record's
/// blake3, and the placement's bands and phase.
pub fn atlas_key(p: &PlacementQ, outline: &BakeOutline) -> crate::glyph_atlas::GlyphKey {
    crate::glyph_atlas::GlyphKey {
        raster: 1,
        outline_hash: alto_core::ContentHash::of(&alto_core::text_bake::outline_record(outline)).0,
        outline: 0,
        matrix: p.band.map(|v| v as i32),
        phase: p.phase.map(|v| v as u8),
    }
}

/// An atlas-band glyph's Q8 points in its bitmap (top-left at the origin).
pub fn atlas_commands(p: &PlacementQ, outline: &BakeOutline) -> Option<Vec<Command>> {
    map_commands(&outline.commands, |x, y| {
        let b = band_point(p, outline.upem, x, y)?;
        Some([b[0] - 256 * p.x0, b[1] - 256 * p.y0])
    })
}

/// A direct-band glyph's Q8 points in level pixels: `C` from Q30 to Q8.
pub fn direct_commands(c: &GlyphMapQ, outline: &BakeOutline) -> Option<Vec<Command>> {
    let m = c.m.map(i128::from);
    let t = c.t.map(i128::from);
    map_commands(&outline.commands, |x, y| {
        let (x, y) = (i128::from(x), i128::from(y));
        Some([
            div_q(m[0] * x + m[2] * y + t[0], 1 << 22)?,
            div_q(m[1] * x + m[3] * y + t[1], 1 << 22)?,
        ])
    })
}

/// An atlas-band glyph's bitmap bytes (`w * h`, row major).
pub fn rasterize_atlas(p: &PlacementQ, outline: &BakeOutline) -> Result<Vec<u8>, RasterError> {
    let commands = atlas_commands(p, outline).ok_or(RasterError::Range)?;
    rasterize_glyph(&commands, [0, 0, i64::from(p.w), i64::from(p.h)])
}

/// A direct-band glyph's window (memo "頂点の Q8" 窓): the outline's control-point
/// box mapped by `C`, rounded outward to pixels, intersected with the level-pixel
/// `roi` `[ox, oy, w, h]`. `None` when they do not meet (nothing to draw).
pub fn direct_window(c: &GlyphMapQ, outline: &BakeOutline, roi: [i64; 4]) -> Option<[i64; 4]> {
    let [x0, y0, x1, y1] = bounds(outline)?;
    let m = c.m.map(i128::from);
    let t = c.t.map(i128::from);
    let corner = |x: i64, y: i64| -> Option<[i64; 2]> {
        let (x, y) = (i128::from(x), i128::from(y));
        Some([
            div_q(m[0] * x + m[2] * y + t[0], 1 << 22)?,
            div_q(m[1] * x + m[3] * y + t[1], 1 << 22)?,
        ])
    };
    let corners = [
        corner(x0, y0)?,
        corner(x1, y0)?,
        corner(x0, y1)?,
        corner(x1, y1)?,
    ];
    let lo = |a: usize| floor_div(corners.iter().map(|c| c[a]).min().unwrap(), 256);
    let hi = |a: usize| ceil_div(corners.iter().map(|c| c[a]).max().unwrap(), 256);
    let [ox, oy, w, h] = roi;
    let (left, top) = (lo(0).max(ox), lo(1).max(oy));
    let (right, bottom) = (hi(0).min(ox + w), hi(1).min(oy + h));
    (right > left && bottom > top).then_some([left, top, right - left, bottom - top])
}

/// A direct-band glyph's bytes over the level-pixel `window` `[ox, oy, w, h]`.
/// The bytes are the same as a larger window's, cut (memo "窓の独立性").
pub fn rasterize_direct(
    c: &GlyphMapQ,
    outline: &BakeOutline,
    window: [i64; 4],
) -> Result<Vec<u8>, RasterError> {
    let commands = direct_commands(c, outline).ok_or(RasterError::Range)?;
    rasterize_glyph(&commands, window)
}

/// One glyph's coverage cropped to a ROI, for the v3 text paint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphCrop {
    /// Top-left in ROI pixels.
    pub dst: [u32; 2],
    pub w: u32,
    pub h: u32,
    /// `w * h` coverage bytes, row major.
    pub bytes: Vec<u8>,
    /// Straight Q14 RGBA.
    pub color: [u16; 4],
}

/// The coverage of every glyph of a bake that reaches the level-pixel `roi`
/// `[ox, oy, w, h]`, in drawing order, each cropped to the ROI (memo
/// "アトラスとスクラッチ" 切り抜き). A bake that does not decode, a glyph whose
/// `C` is out of range, and a glyph refused by the rasterizer draw nothing.
/// Crops with no coverage are left out (painting them changes nothing).
pub fn glyph_bitmaps(bake: &[u8], world: FixedAffine, level: i32, roi: [i64; 4]) -> Vec<GlyphCrop> {
    let Ok(bake) = alto_core::text_bake::TextBake::decode(bake) else {
        return vec![];
    };
    let [ox, oy, w, h] = roi;
    let mut out = vec![];
    for g in &bake.glyphs {
        let outline = &bake.outlines[g.outline as usize];
        let Some(c) = glyph_map_q(world, level, g.m, g.t) else {
            continue;
        };
        // The bitmap and its level-pixel box.
        let (bytes, [bx, by, bw, bh]) = if let Some(p) = atlas_band(&c, outline.upem)
            .then(|| place_q(&c, outline))
            .flatten()
        {
            let Ok(bytes) = rasterize_atlas(&p, outline) else {
                continue;
            };
            let [dx, dy] = p.dst();
            (bytes, [dx, dy, i64::from(p.w), i64::from(p.h)])
        } else {
            let Some(window) = direct_window(&c, outline, roi) else {
                continue;
            };
            let Ok(bytes) = rasterize_direct(&c, outline, window) else {
                continue;
            };
            (bytes, window)
        };
        let (left, top) = (bx.max(ox), by.max(oy));
        let (right, bottom) = ((bx + bw).min(ox + w), (by + bh).min(oy + h));
        if right <= left || bottom <= top {
            continue;
        }
        let (cw, ch) = ((right - left) as usize, (bottom - top) as usize);
        let mut crop = Vec::with_capacity(cw * ch);
        for y in 0..ch {
            let row = (top - by) as usize + y;
            let start = row * bw as usize + (left - bx) as usize;
            crop.extend_from_slice(&bytes[start..start + cw]);
        }
        if crop.iter().all(|&b| b == 0) {
            continue;
        }
        out.push(GlyphCrop {
            dst: [(left - ox) as u32, (top - oy) as u32],
            w: cw as u32,
            h: ch as u32,
            bytes: crop,
            color: g.color,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alto_core::Affine2;

    /// A Q6 rectangle outline in font units, clockwise in y-up font space.
    fn rect(x0: i32, y0: i32, x1: i32, y1: i32, upem: u16) -> BakeOutline {
        let q = |v: i32| v * 64;
        BakeOutline {
            font: 0,
            glyph: 1,
            upem,
            commands: vec![
                BakeCommand::Move([q(x0), q(y0)]),
                BakeCommand::Line([q(x1), q(y0)]),
                BakeCommand::Line([q(x1), q(y1)]),
                BakeCommand::Line([q(x0), q(y1)]),
                BakeCommand::Close,
            ],
        }
    }
    /// The bake's Q24 / Q30 placement of an f64 font-units-to-node matrix.
    fn placement(m: [f64; 6]) -> ([i32; 4], [i64; 2]) {
        let q = |v: f64, bits: i32| (v * 2f64.powi(bits)).round();
        (
            [0, 1, 2, 3].map(|i| q(m[i], 24) as i32),
            [q(m[4], 30) as i64, q(m[5], 30) as i64],
        )
    }
    fn map(m: [f64; 6]) -> GlyphMapQ {
        let (pm, pt) = placement(m);
        glyph_map_q(FixedAffine::IDENTITY, 0, pm, pt).unwrap()
    }

    #[test]
    fn rasterizer_q8_matches_exact_area() {
        // v2's case: a 1000-unit square at 12 px/em shifted by a quarter pixel.
        let o = rect(0, 0, 1000, 1000, 1000);
        let c = map([0.012, 0.0, 0.0, -0.012, 2.25, 14.5]);
        assert!(atlas_band(&c, 1000));
        let p = place_q(&c, &o).unwrap();
        assert_eq!((p.origin, p.phase), ([2, 14], [1, 2]));
        let px = rasterize_atlas(&p, &o).unwrap();
        let ink: u64 = px.iter().map(|&v| u64::from(v)).sum();
        // 144 px of ink; full pixels are 255 and the edges round once each.
        assert!((ink as f64 / 255.0 - 144.0).abs() < 0.2, "ink {ink}");
        let w = p.w as usize;
        let at = |x: i64, y: i64| px[((y - p.dst()[1]) as usize) * w + (x - p.dst()[0]) as usize];
        // Column 2 is 3/4 covered: N = 98304, (98304 * 255 + 65536) >> 17 = 191.
        assert_eq!(at(2, 8), 191);
        assert_eq!(at(5, 8), 255);
        // The same glyph in the direct band gives the same coverage.
        let direct = rasterize_direct(&c, &o, [0, 0, 17, 17]).unwrap();
        assert_eq!(direct[8 * 17 + 2], 191);
        assert_eq!(direct[8 * 17 + 5], 255);
    }

    #[test]
    fn place_q_rounds_negative_positions_away_from_zero() {
        let o = rect(0, 0, 500, 500, 1000);
        for (x, origin, phase) in [(-0.125, -1, 3), (-0.375, -1, 2), (0.125, 0, 1), (0.0, 0, 0)] {
            let p = place_q(&map([0.012, 0.0, 0.0, -0.012, x, 10.0]), &o).unwrap();
            assert_eq!((p.origin[0], p.phase[0]), (origin, phase), "x {x}");
        }
        // A degenerate band and an oversized bitmap have no atlas placement.
        assert!(place_q(&map([0.012, 0.0, 0.024, 0.0, 0.0, 0.0]), &o).is_none());
        assert!(
            place_q(
                &map([0.5, 0.0, 0.0, -0.5, 0.0, 0.0]),
                &rect(0, 0, 1000, 1000, 1000)
            )
            .is_none()
        );
    }

    #[test]
    fn atlas_band_is_the_integer_em_test() {
        // At upem 1024, 24 px/em is m = 24 / 1024, exactly 393216 in Q24: the
        // boundary is strict, so it is direct and one Q24 step less is atlas.
        let diag = |q: i64| GlyphMapQ {
            m: [q, 0, 0, -q],
            t: [0, 0],
        };
        assert!(!atlas_band(&diag(393216), 1024));
        assert!(atlas_band(&diag(393215), 1024));
        // A rotation keeps the em: 20 px/em rotated by 0.3 rad is still atlas.
        let (c, s) = (0.3f64.cos() * 0.02, 0.3f64.sin() * 0.02);
        assert!(atlas_band(&map([c, s, s, -c, 0.0, 0.0]), 1000));
        assert!(!atlas_band(
            &map([c * 1.25, s * 1.25, s * 1.25, -c * 1.25, 0.0, 0.0]),
            1000
        ));
    }

    #[test]
    fn rasterizer_q8_window_crop() {
        // A large rotated direct-band glyph at a fractional, negative position:
        // a big window and its 2x2 split agree on every byte.
        let k = 0.5522847498 * 500.0;
        let q = |v: f64| (v * 64.0).round() as i32;
        let circle = BakeOutline {
            font: 0,
            glyph: 2,
            upem: 1000,
            commands: vec![
                BakeCommand::Move([q(1000.0), q(500.0)]),
                BakeCommand::Cubic(
                    [q(1000.0), q(500.0 + k)],
                    [q(500.0 + k), q(1000.0)],
                    [q(500.0), q(1000.0)],
                ),
                BakeCommand::Cubic(
                    [q(500.0 - k), q(1000.0)],
                    [q(0.0), q(500.0 + k)],
                    [q(0.0), q(500.0)],
                ),
                BakeCommand::Cubic(
                    [q(0.0), q(500.0 - k)],
                    [q(500.0 - k), q(0.0)],
                    [q(500.0), q(0.0)],
                ),
                BakeCommand::Cubic(
                    [q(500.0 + k), q(0.0)],
                    [q(1000.0), q(500.0 - k)],
                    [q(1000.0), q(500.0)],
                ),
                BakeCommand::Close,
            ],
        };
        let (cs, sn) = (0.3f64.cos() * 0.04, 0.3f64.sin() * 0.04);
        let c = map([cs, sn, sn, -cs, -7.375, 30.625]);
        assert!(!atlas_band(&c, 1000));
        let full = rasterize_direct(&c, &circle, [-16, -16, 64, 64]).unwrap();
        assert!(full.contains(&255) && full.iter().any(|&b| b > 0 && b < 255));
        for (wx, wy) in [(-16, -16), (16, -16), (-16, 16), (16, 16)] {
            let part = rasterize_direct(&c, &circle, [wx, wy, 32, 32]).unwrap();
            for y in 0..32 {
                let row = (y + (wy + 16) as usize) * 64 + (wx + 16) as usize;
                assert_eq!(
                    &part[y * 32..y * 32 + 32],
                    &full[row..row + 32],
                    "({wx}, {wy}) row {y}"
                );
            }
        }
    }

    #[test]
    fn glyph_key_separates_raster_versions() {
        let o = rect(0, 0, 1000, 1000, 1000);
        let p = place_q(&map([0.012, 0.0, 0.0, -0.012, 2.25, 14.5]), &o).unwrap();
        let v3 = atlas_key(&p, &o);
        // A v2 key with the same matrix and phase is a different entry.
        let v2 = crate::glyph_atlas::GlyphKey {
            raster: 0,
            outline_hash: [0; 32],
            outline: 0,
            ..v3
        };
        assert_eq!((v3.raster, v3.outline), (1, 0));
        assert_ne!(v3, v2);
        let mut atlas = crate::glyph_atlas::GlyphAtlas::new(256, 256);
        atlas.begin(1);
        let a = atlas.insert(v3, p.w, p.h).unwrap();
        let b = atlas.insert(v2, p.w, p.h).unwrap();
        assert_ne!((a.x, a.y), (b.x, b.y), "two entries");
        // Equal shapes share an entry whatever their font and glyph id; another
        // shape, or the same shape at another upem, does not.
        let same = BakeOutline {
            font: 7,
            glyph: 99,
            ..o.clone()
        };
        assert_eq!(atlas_key(&p, &same), v3);
        let other = rect(0, 0, 1000, 999, 1000);
        assert_ne!(atlas_key(&p, &other).outline_hash, v3.outline_hash);
        let upem = BakeOutline {
            upem: 2048,
            ..o.clone()
        };
        assert_ne!(atlas_key(&p, &upem).outline_hash, v3.outline_hash);
    }

    #[test]
    fn empty_outlines_are_transparent() {
        // A space or a missing outline: no atlas placement, and no ink.
        let empty = BakeOutline {
            font: 0,
            glyph: 4,
            upem: 1000,
            commands: vec![],
        };
        let c = map([0.012, 0.0, 0.0, -0.012, 2.25, 14.5]);
        assert!(place_q(&c, &empty).is_none());
        assert_eq!(rasterize_direct(&c, &empty, [0, 0, 4, 4]), Ok(vec![0; 16]));
    }

    #[test]
    fn text_q8_raster_does_not_overflow() {
        // Extremes of the bake and the transform neither panic nor wrap.
        let max = alto_core::text_bake::MAX_OUTLINE_COORD;
        let o = BakeOutline {
            font: 0,
            glyph: 3,
            upem: 16384,
            commands: vec![
                BakeCommand::Move([-max, -max]),
                BakeCommand::Cubic([max, -max], [-max, max], [max, max]),
                BakeCommand::Quad([max, -max], [-max, max]),
                BakeCommand::Close,
            ],
        };
        let world =
            FixedAffine::from_affine(Affine2([16000.0, 0.0, 0.0, 16000.0, 1e9, -1e9])).unwrap();
        let (mut mapped, mut drawn) = (0, 0);
        for level in [-16, -6, 0, 12] {
            for m in [
                [i32::MAX, i32::MIN, i32::MIN, i32::MAX],
                [1, 0, 0, 1],
                [0, 0, 0, 0],
            ] {
                for t in [[0, 0], [1 << 60, -(1 << 60)]] {
                    let Some(c) = glyph_map_q(world, level, m, t) else {
                        // Only an out-of-range C is refused, never by a panic.
                        continue;
                    };
                    mapped += 1;
                    assert!(c.m.iter().all(|v| v.unsigned_abs() < 1 << 31));
                    assert!(c.t.iter().all(|v| v.unsigned_abs() < 1 << 60));
                    let _ = atlas_band(&c, o.upem);
                    if let Some(p) = place_q(&c, &o) {
                        let bytes = rasterize_atlas(&p, &o).unwrap();
                        assert_eq!(bytes.len(), p.w as usize * p.h as usize);
                    }
                    // Glyph curves are capped, never refused: Ok, or the
                    // glyph's own extent refused (Range) whatever the window.
                    match rasterize_direct(&c, &o, [0, 0, 8, 8]) {
                        Ok(bytes) => {
                            assert_eq!(bytes.len(), 64);
                            drawn += 1;
                        }
                        Err(e) => {
                            assert_eq!(e, RasterError::Range);
                            let other = rasterize_direct(&c, &o, [-1000, 7, 3, 3]);
                            assert_eq!(other, Err(RasterError::Range));
                        }
                    }
                }
            }
        }
        assert!(mapped > 0 && drawn > 0, "mapped {mapped} drawn {drawn}");
        // An identity world and a 1:1 placement compose to the placement.
        let unit = glyph_map_q(FixedAffine::IDENTITY, 0, [1 << 24, 0, 0, 1 << 24], [0, 0]);
        assert_eq!(unit.unwrap().m, [1 << 24, 0, 0, 1 << 24]);
        assert!(glyph_map_q(FixedAffine::IDENTITY, 13, [1, 0, 0, 1], [0, 0]).is_none());
        assert!(glyph_map_q(FixedAffine::IDENTITY, -17, [1, 0, 0, 1], [0, 0]).is_none());
        // A glyph wider than 2^24 Q8 is refused whatever the window.
        let wide = [
            Command::Move([0, 0]),
            Command::Line([(1 << 24) + 1, 0]),
            Command::Line([0, 256]),
            Command::Close,
        ];
        assert_eq!(
            rasterize_glyph(&wide, [0, 0, 4, 4]),
            Err(RasterError::Range)
        );
    }
}
