//! Window-independent integer path coverage for version 3 vector and text.
//! Coordinates are Q8 pixels. The caller maps document geometry to this grid.

use alto_core::{FixedAffine, q_from_f64_bits};
use alto_vector::{PathCommand, PathData};

#[derive(Clone, Copy, Debug)]
pub enum Command {
    Move([i64; 2]),
    Line([i64; 2]),
    Quad([i64; 2], [i64; 2]),
    Cubic([i64; 2], [i64; 2], [i64; 2]),
    Close,
}

#[derive(Clone, Copy, Debug)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

/// `NMAX`: the most segments one curve is flattened into.
pub const CURVE_LIMIT: u32 = 4096;

/// Why a path has no coverage bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RasterError {
    /// A curve needs more than the subdivision limit (`VECTOR_CURVE_RANGE`).
    /// It depends on the path and level only, never on the window, so callers
    /// skip the path (a mask or clip path covers nothing) on every tile.
    CurveRange,
    /// The window or an intermediate exceeded its integer range.
    Range,
}

/// Exact half-away quantization of a binary32 bit pattern. The only floating
/// operation at the drawing boundary is obtaining the stored value's bits.
pub fn q_from_f32_bits(bits: u32, fractional_bits: u32) -> Option<i64> {
    let exponent = ((bits >> 23) & 0xff) as i32;
    if exponent == 0xff {
        return None;
    }
    let fraction = bits & ((1u32 << 23) - 1);
    let (mantissa, power) = if exponent == 0 {
        (u128::from(fraction), -149)
    } else {
        (u128::from(fraction | (1u32 << 23)), exponent - 150)
    };
    let shift = power.checked_add(i32::try_from(fractional_bits).ok()?)?;
    let magnitude = if shift >= 0 {
        let shift = u32::try_from(shift).ok()?;
        if shift >= 128 || mantissa > (u128::MAX >> shift) {
            return None;
        }
        mantissa << shift
    } else {
        let right = u32::try_from(-shift).ok()?;
        if right >= 25 {
            0
        } else {
            (mantissa + (1u128 << (right - 1))) >> right
        }
    };
    let signed = i128::try_from(magnitude).ok()?;
    i64::try_from(if bits >> 31 == 0 { signed } else { -signed }).ok()
}

/// Return the power-of-two level and exact integer level-pixel origin of an
/// M5 ROI, without using its f32 values in arithmetic.
pub fn roi_grid(x: f32, y: f32, scale: f32) -> Option<[i64; 3]> {
    let bits = scale.to_bits();
    let exponent = ((bits >> 23) & 0xff) as i32;
    if bits >> 31 != 0 || bits & ((1 << 23) - 1) != 0 || !(1..255).contains(&exponent) {
        return None;
    }
    let level = exponent - 127;
    let coordinate = |value: f32| -> Option<i64> {
        let bits = value.to_bits();
        let exponent = ((bits >> 23) & 0xff) as i32;
        if exponent == 0xff {
            return None;
        }
        let mantissa = if exponent == 0 {
            u128::from(bits & ((1 << 23) - 1))
        } else {
            u128::from((bits & ((1 << 23) - 1)) | (1 << 23))
        };
        let power = if exponent == 0 { -149 } else { exponent - 150 };
        let shift = power - level;
        let value = if shift >= 0 {
            let shift = u32::try_from(shift).ok()?;
            if shift >= 128 || mantissa > (u128::MAX >> shift) {
                return None;
            }
            mantissa << shift
        } else {
            let right = u32::try_from(-shift).ok()?;
            if right >= 128 {
                if mantissa != 0 {
                    return None;
                }
                0
            } else {
                if mantissa & ((1u128 << right) - 1) != 0 {
                    return None;
                }
                mantissa >> right
            }
        };
        let signed = i128::try_from(value).ok()?;
        i64::try_from(if bits >> 31 == 0 { signed } else { -signed }).ok()
    };
    Some([coordinate(x)?, coordinate(y)?, i64::from(level)])
}

/// Map a validated vector path through the v3 Q32 world matrix to Q8 pixels
/// at `level`. Every point is rounded once after the exact integer product.
pub fn map_path(path: &PathData, world: FixedAffine, level: i32) -> Option<Vec<Command>> {
    if !(-6..=12).contains(&level) {
        return None;
    }
    let divisor = 1i128 << (40 + level);
    let point = |x: f64, y: f64| -> Option<[i64; 2]> {
        let (x, y) = (
            i128::from(q_from_f64_bits(x.to_bits(), 16)?),
            i128::from(q_from_f64_bits(y.to_bits(), 16)?),
        );
        let axis = |a: i64, b: i64, t: i64| {
            div_q(
                i128::from(a)
                    .checked_mul(x)?
                    .checked_add(i128::from(b).checked_mul(y)?)?
                    .checked_add(i128::from(t) << 32)?,
                divisor,
            )
        };
        Some([
            axis(world.m[0], world.m[2], world.t[0])?,
            axis(world.m[1], world.m[3], world.t[1])?,
        ])
    };
    path.commands
        .iter()
        .map(|command| match *command {
            PathCommand::Move { x, y } => Some(Command::Move(point(x, y)?)),
            PathCommand::Line { x, y } => Some(Command::Line(point(x, y)?)),
            PathCommand::Quad { cx, cy, x, y } => Some(Command::Quad(point(cx, cy)?, point(x, y)?)),
            PathCommand::Cubic {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => Some(Command::Cubic(
                point(c1x, c1y)?,
                point(c2x, c2y)?,
                point(x, y)?,
            )),
            PathCommand::Close => Some(Command::Close),
        })
        .collect()
}

/// The memo's `stroke_visible`: a stroke draws pixels only with a nonzero width
/// and an alpha whose Q14 is nonzero (`0 < a < 2^-15` is invisible), decided
/// from the stored bits. A stroke that cannot be decoded counts as visible.
pub fn stroke_visible(stroke: &alto_vector::Stroke) -> bool {
    stroke.width.to_bits() & 0x7fff_ffff != 0
        && q_from_f32_bits(stroke.color[3].to_bits(), 14) != Some(0)
}

pub fn color_q14(color: [f32; 4]) -> Option<[u16; 4]> {
    let mut out = [0u16; 4];
    for (target, source) in out.iter_mut().zip(color) {
        *target = u16::try_from(q_from_f32_bits(source.to_bits(), 14)?).ok()?;
        if *target > 16384 {
            return None;
        }
    }
    Some(out)
}

/// Independent host reference for the v3 vector paint shader's normal blend.
pub fn source_over_q14(dst: [u16; 4], color: [u16; 4], coverage: u8) -> [u16; 4] {
    let k = (u64::from(coverage) * 16384 + 127) / 255;
    let f = k * u64::from(color[3]);
    let source_alpha = (f + 8192) / 16384;
    let mut out = [0u16; 4];
    for channel in 0..4 {
        let source = if channel == 3 {
            source_alpha
        } else {
            (u64::from(color[channel]) * f + (1 << 27)) >> 28
        };
        out[channel] = ((source * 16384 + u64::from(dst[channel]) * (16384 - source_alpha) + 8192)
            >> 14) as u16;
    }
    out
}

/// A pixel's coverage `e` (131072 is full, larger is clamped) as a byte. The
/// only tie is `e = 65536`, which rounds away from 0 to 128.
fn coverage_byte(e: u128) -> u8 {
    ((e.min(131072) * 255 + 65536) >> 17) as u8
}

fn div_q(n: i128, d: i128) -> Option<i64> {
    if d <= 0 {
        return None;
    }
    let magnitude = n.unsigned_abs();
    let quotient = magnitude
        .checked_add((d as u128) / 2)?
        .checked_div(d as u128)?;
    i64::try_from(if n < 0 {
        -(i128::try_from(quotient).ok()?)
    } else {
        i128::try_from(quotient).ok()?
    })
    .ok()
}

fn difference_squared(p: [i64; 2], c: [i64; 2], q: [i64; 2]) -> Option<u128> {
    let x = i128::from(p[0]) - 2 * i128::from(c[0]) + i128::from(q[0]);
    let y = i128::from(p[1]) - 2 * i128::from(c[1]) + i128::from(q[1]);
    x.unsigned_abs()
        .checked_mul(x.unsigned_abs())?
        .checked_add(y.unsigned_abs().checked_mul(y.unsigned_abs())?)
}

/// How many segments a curve needs past the limit: none (`Reject`, vector
/// fill's `VECTOR_CURVE_RANGE`) or the limit itself (`Clamp`, glyphs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Curves {
    Reject(u32),
    Clamp(u32),
}

fn divisions(d: u128, k: u128, curves: Curves) -> Option<u32> {
    let (limit, clamp) = match curves {
        Curves::Reject(n) => (n, false),
        Curves::Clamp(n) => (n, true),
    };
    let Some(required) = k.checked_mul(k).and_then(|v| v.checked_mul(d)) else {
        return clamp.then_some(limit);
    };
    let fits = |n: u32| {
        let n = u128::from(n);
        65536u128 * n * n * n * n >= required
    };
    if !fits(limit) {
        return clamp.then_some(limit);
    }
    let (mut lo, mut hi) = (1, limit);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if fits(mid) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Some(lo)
}

fn lerp_line(a: [i64; 2], b: [i64; 2], y: i64) -> Option<i64> {
    let dy = i128::from(b[1]) - i128::from(a[1]);
    let n = i128::from(a[0])
        .checked_mul(dy)?
        .checked_add((i128::from(b[0]) - i128::from(a[0])) * (i128::from(y) - i128::from(a[1])))?;
    div_q(n, dy)
}

fn lerp_column(a: [i64; 2], b: [i64; 2], x: i64) -> Option<i64> {
    let dx = (i128::from(b[0]) - i128::from(a[0])).abs();
    let n = i128::from(a[1]).checked_mul(dx)?.checked_add(
        (i128::from(b[1]) - i128::from(a[1]))
            .checked_mul((i128::from(x) - i128::from(a[0])).abs())?,
    )?;
    div_q(n, dx)
}

fn normalized(commands: &[Command]) -> Option<(Vec<Command>, [i64; 2])> {
    let mut min = [i64::MAX; 2];
    let mut found = false;
    for command in commands {
        let mut visit = |p: &[i64; 2]| {
            found = true;
            for axis in 0..2 {
                min[axis] = min[axis].min(p[axis]);
            }
        };
        match command {
            Command::Move(p) | Command::Line(p) => visit(p),
            Command::Quad(a, b) => {
                visit(a);
                visit(b);
            }
            Command::Cubic(a, b, c) => {
                visit(a);
                visit(b);
                visit(c);
            }
            Command::Close => {}
        }
    }
    if !found {
        return Some((vec![], [0; 2]));
    }
    let origin = [min[0].div_euclid(256), min[1].div_euclid(256)];
    let shift = |p: [i64; 2]| -> Option<[i64; 2]> {
        Some([
            i64::try_from(i128::from(p[0]) - i128::from(origin[0]) * 256).ok()?,
            i64::try_from(i128::from(p[1]) - i128::from(origin[1]) * 256).ok()?,
        ])
    };
    let mut out = Vec::with_capacity(commands.len());
    for command in commands {
        out.push(match command {
            Command::Move(p) => Command::Move(shift(*p)?),
            Command::Line(p) => Command::Line(shift(*p)?),
            Command::Quad(a, b) => Command::Quad(shift(*a)?, shift(*b)?),
            Command::Cubic(a, b, c) => Command::Cubic(shift(*a)?, shift(*b)?, shift(*c)?),
            Command::Close => Command::Close,
        });
    }
    Some((out, origin))
}

/// The window in path-relative Q8 units: `[XL, XR) x [YT, YB)`.
#[derive(Clone, Copy)]
struct Window {
    lx: i64,
    ly: i64,
    width: usize,
    height: usize,
    xl: i64,
    xr: i64,
    yt: i64,
    yb: i64,
}

/// Where flattened segments go. Segments are never collected: one path can
/// flatten into `2^24` of them (4096 commands of 4096 segments each).
trait Sink {
    fn segment(&mut self, a: [i64; 2], b: [i64; 2]) -> Option<()>;
}

/// Flatten `commands` into `sink`. Every curve's subdivision count is checked
/// (`CurveRange` does not depend on the window), but with `skip` a curve whose
/// control hull is above, below or right of the window is not subdivided, and
/// one left of it becomes its chord. Neither changes a byte (the memo's
/// "結果を変えない省略"); `raster_q8_skips_are_exact` checks that.
fn flatten(
    commands: &[Command],
    limit: Curves,
    window: Window,
    skip: bool,
    sink: &mut impl Sink,
) -> Result<(), RasterError> {
    use RasterError::{CurveRange, Range};
    let (mut start, mut current): (Option<[i64; 2]>, Option<[i64; 2]>) = (None, None);
    let mut emit = |a: [i64; 2], b: [i64; 2]| -> Result<(), RasterError> {
        if a != b {
            sink.segment(a, b).ok_or(Range)?;
        }
        Ok(())
    };
    // `Some(true)`: replace by the chord; `Some(false)`: drop; `None`: subdivide.
    let skipped = |points: &[[i64; 2]]| -> Option<bool> {
        if !skip {
            return None;
        }
        let (min_x, max_x) = (
            points.iter().map(|p| p[0]).min()?,
            points.iter().map(|p| p[0]).max()?,
        );
        let (min_y, max_y) = (
            points.iter().map(|p| p[1]).min()?,
            points.iter().map(|p| p[1]).max()?,
        );
        if max_y <= window.yt || min_y >= window.yb || min_x >= window.xr {
            Some(false)
        } else if max_x <= window.xl {
            Some(true)
        } else {
            None
        }
    };
    for command in commands {
        match *command {
            Command::Move(p) => {
                if let (Some(a), Some(b)) = (current, start) {
                    emit(a, b)?;
                }
                start = Some(p);
                current = Some(p);
            }
            Command::Line(p) => {
                if let Some(a) = current {
                    emit(a, p)?;
                } else {
                    start = Some(p);
                }
                current = Some(p);
            }
            Command::Quad(c, p) => {
                let a = current.ok_or(Range)?;
                // A second difference past u128 needs more than any limit.
                let d = difference_squared(a, c, p).unwrap_or(u128::MAX);
                let n = divisions(d, 10, limit).ok_or(CurveRange)?;
                if let Some(chord) = skipped(&[a, c, p]) {
                    if chord {
                        emit(a, p)?;
                    }
                    current = Some(p);
                    continue;
                }
                let n128 = i128::from(n);
                for i in 1..=n {
                    let i = i128::from(i);
                    let j = n128 - i;
                    let value = |axis| {
                        div_q(
                            j * j * i128::from(a[axis])
                                + 2 * j * i * i128::from(c[axis])
                                + i * i * i128::from(p[axis]),
                            n128 * n128,
                        )
                    };
                    let next = [value(0).ok_or(Range)?, value(1).ok_or(Range)?];
                    emit(current.ok_or(Range)?, next)?;
                    current = Some(next);
                }
            }
            Command::Cubic(c1, c2, p) => {
                let a = current.ok_or(Range)?;
                let d = difference_squared(a, c1, c2)
                    .unwrap_or(u128::MAX)
                    .max(difference_squared(c1, c2, p).unwrap_or(u128::MAX));
                let n = divisions(d, 30, limit).ok_or(CurveRange)?;
                if let Some(chord) = skipped(&[a, c1, c2, p]) {
                    if chord {
                        emit(a, p)?;
                    }
                    current = Some(p);
                    continue;
                }
                let n128 = i128::from(n);
                for i in 1..=n {
                    let i = i128::from(i);
                    let j = n128 - i;
                    let value = |axis| {
                        div_q(
                            j * j * j * i128::from(a[axis])
                                + 3 * j * j * i * i128::from(c1[axis])
                                + 3 * j * i * i * i128::from(c2[axis])
                                + i * i * i * i128::from(p[axis]),
                            n128 * n128 * n128,
                        )
                    };
                    let next = [value(0).ok_or(Range)?, value(1).ok_or(Range)?];
                    emit(current.ok_or(Range)?, next)?;
                    current = Some(next);
                }
            }
            Command::Close => {
                if let (Some(a), Some(b)) = (current, start) {
                    emit(a, b)?;
                    current = Some(b);
                }
            }
        }
    }
    if let (Some(a), Some(b)) = (current, start) {
        emit(a, b)?;
    }
    Ok(())
}

/// Rasterize a path into the level-pixel window. An error means a defined range
/// limit was exceeded; callers must not silently substitute float rendering.
pub fn rasterize(
    commands: &[Command],
    rule: FillRule,
    window: [i64; 4],
    curve_limit: u32,
) -> Result<Vec<u8>, RasterError> {
    rasterize_with(commands, rule, window, Curves::Reject(curve_limit), true)
}

/// The most segments one glyph curve is flattened into (v2's 1..=64).
pub const GLYPH_CURVE_LIMIT: u32 = 64;
/// Glyph coordinates, from the glyph's own pixel origin, are at most this
/// (about 65536 pixels); a larger glyph is not drawn (memo "前提の検査").
pub const GLYPH_EXTENT: i64 = 1 << 24;

/// A glyph's nonzero coverage (doc/design/2026-10-28-v3-integer-text.md
/// "カバレッジ"): the shared core with curves capped at 64 segments instead of
/// refused. `Range` when the glyph is wider than `GLYPH_EXTENT`, which depends
/// on the glyph only, never on the window.
pub fn rasterize_glyph(commands: &[Command], window: [i64; 4]) -> Result<Vec<u8>, RasterError> {
    let (shifted, _) = normalized(commands).ok_or(RasterError::Range)?;
    let too_wide = shifted.iter().any(|c| {
        let points: &[[i64; 2]] = match c {
            Command::Move(p) | Command::Line(p) => std::slice::from_ref(p),
            Command::Quad(a, b) => &[*a, *b],
            Command::Cubic(a, b, p) => &[*a, *b, *p],
            Command::Close => &[],
        };
        points
            .iter()
            .any(|p| p[0] > GLYPH_EXTENT || p[1] > GLYPH_EXTENT)
    });
    if too_wide {
        return Err(RasterError::Range);
    }
    rasterize_with(
        commands,
        FillRule::NonZero,
        window,
        Curves::Clamp(GLYPH_CURVE_LIMIT),
        true,
    )
}

fn rasterize_with(
    commands: &[Command],
    rule: FillRule,
    window: [i64; 4],
    curve_limit: Curves,
    skip: bool,
) -> Result<Vec<u8>, RasterError> {
    use RasterError::Range;
    let (commands, origin) = normalized(commands).ok_or(Range)?;
    let [ox, oy, width, height] = window;
    let (width, height) = (
        usize::try_from(width).map_err(|_| Range)?,
        usize::try_from(height).map_err(|_| Range)?,
    );
    let len = width.checked_mul(height).ok_or(Range)?;
    if len > 1024 * 1024 {
        return Err(Range);
    }
    let lx = ox.checked_sub(origin[0]).ok_or(Range)?;
    let ly = oy.checked_sub(origin[1]).ok_or(Range)?;
    let edge = |start: i64, span: usize| -> Option<(i64, i64)> {
        let end = start.checked_add(i64::try_from(span).ok()?)?;
        Some((start.checked_mul(256)?, end.checked_mul(256)?))
    };
    let (xl, xr) = edge(lx, width).ok_or(Range)?;
    let (yt, yb) = edge(ly, height).ok_or(Range)?;
    let window = Window {
        lx,
        ly,
        width,
        height,
        xl,
        xr,
        yt,
        yb,
    };
    let mut bytes = vec![0u8; len];
    if len == 0 {
        return Ok(bytes);
    }
    match rule {
        FillRule::NonZero => {
            let mut sink = NonZero {
                window,
                cover: vec![0; len],
                area: vec![0; len],
            };
            flatten(&commands, curve_limit, window, skip, &mut sink)?;
            for row in 0..height {
                let mut sum = 0i128;
                for col in 0..width {
                    let idx = row * width + col;
                    sum = sum.checked_add(sink.cover[idx]).ok_or(Range)?;
                    let n = sum
                        .checked_mul(512)
                        .and_then(|v| v.checked_sub(sink.area[idx]))
                        .ok_or(Range)?;
                    bytes[idx] = coverage_byte(n.unsigned_abs());
                }
            }
        }
        FillRule::EvenOdd => {
            let mut sink = EvenOdd {
                window,
                crossings: vec![vec![]; height * 16],
            };
            flatten(&commands, curve_limit, window, skip, &mut sink)?;
            let mut tally = vec![0u32; len];
            for (index, xs) in sink.crossings.iter_mut().enumerate() {
                let row = index / 16;
                xs.sort_unstable();
                // A skipped curve right of the window drops crossings at or
                // beyond XR; the last open interval then ends at XR.
                if xs.len() % 2 == 1 {
                    xs.push((xr - xl) as u32);
                }
                for &[x0, x1] in xs.as_chunks::<2>().0 {
                    let (left, right) = (i64::from(x0) + xl, i64::from(x1) + xl);
                    if left >= right {
                        continue;
                    }
                    let first = left.div_euclid(256);
                    let last = (right - 1).div_euclid(256);
                    for col in first..=last {
                        let cell_left = col * 256;
                        let overlap = right.min(cell_left + 256) - left.max(cell_left);
                        tally[row * width + (col - lx) as usize] += overlap as u32;
                    }
                }
            }
            for (byte, t) in bytes.iter_mut().zip(tally) {
                *byte = coverage_byte(u128::from(t) * 32);
            }
        }
    }
    Ok(bytes)
}

/// Nonzero: the cell integration's `cover` and `area`, per window cell.
struct NonZero {
    window: Window,
    cover: Vec<i128>,
    area: Vec<i128>,
}

impl Sink for NonZero {
    fn segment(&mut self, mut a: [i64; 2], mut b: [i64; 2]) -> Option<()> {
        let Window {
            lx,
            ly,
            width,
            height,
            xl,
            xr,
            yt,
            yb,
        } = self.window;
        if a[1] == b[1] {
            return Some(());
        }
        let dir = if a[1] > b[1] {
            std::mem::swap(&mut a, &mut b);
            -1i128
        } else {
            1i128
        };
        let first = a[1].div_euclid(256).max(ly);
        let last = b[1].div_euclid(256).min(ly + height as i64);
        for row in first..=last {
            let top = a[1].max(row * 256).max(yt);
            let bottom = b[1].min((row + 1) * 256).min(yb);
            if bottom <= top {
                continue;
            }
            let xa = if top == a[1] {
                a[0]
            } else {
                lerp_line(a, b, top)?
            };
            let xb = if bottom == b[1] {
                b[0]
            } else {
                lerp_line(a, b, bottom)?
            };
            let mut points = vec![[xa, top]];
            if xa != xb {
                let left = xa.min(xb).div_euclid(256).max(lx);
                let right = xa.max(xb).div_euclid(256).min(lx + width as i64);
                for col in left..=right {
                    let x = col * 256;
                    if xa.min(xb) < x && x < xa.max(xb) {
                        points.push([x, lerp_column([xa, top], [xb, bottom], x)?]);
                    }
                }
                points.sort_by_key(|p| if xa < xb { p[0] } else { -p[0] });
            }
            points.push([xb, bottom]);
            for pair in points.windows(2) {
                let ([mut ux, va], [mut vx, vb]) = (pair[0], pair[1]);
                let dy = i128::from(vb) - i128::from(va);
                if dy == 0 || (ux >= xr && vx >= xr) {
                    continue;
                }
                if ux <= xl && vx <= xl {
                    ux = xl;
                    vx = xl;
                }
                let cx =
                    ((i128::from(ux) + i128::from(vx)).div_euclid(512) - i128::from(lx)) as isize;
                let cy =
                    ((i128::from(va) + i128::from(vb)).div_euclid(512) - i128::from(ly)) as isize;
                if cx < 0 || cy < 0 || cx >= width as isize || cy >= height as isize {
                    continue;
                }
                let idx = cy as usize * width + cx as usize;
                let xbase = (i128::from(lx) + cx as i128) * 256;
                self.cover[idx] = self.cover[idx].checked_add(dir * dy)?;
                self.area[idx] = self.area[idx]
                    .checked_add(dir * (i128::from(ux) + i128::from(vx) - 2 * xbase) * dy)?;
            }
        }
        Some(())
    }
}

/// Even-odd: each sub-scanline's crossings, clamped to `[XL, XR]` and stored
/// relative to `XL` (clamping keeps the order and the clipped intervals).
struct EvenOdd {
    window: Window,
    crossings: Vec<Vec<u32>>,
}

impl Sink for EvenOdd {
    fn segment(&mut self, mut a: [i64; 2], mut b: [i64; 2]) -> Option<()> {
        let Window {
            height, xl, xr, yt, ..
        } = self.window;
        if a[1] > b[1] {
            std::mem::swap(&mut a, &mut b);
        }
        // Sub-scanline k is at y = YT + 8 + 16 k; it crosses when a.y <= y < b.y.
        let ceil16 = |v: i64| -> i64 { (v + 15).div_euclid(16) };
        let first = ceil16(a[1] - yt - 8).max(0);
        let last = (ceil16(b[1] - yt - 8) - 1).min(height as i64 * 16 - 1);
        for k in first..=last {
            let y = yt + 8 + 16 * k;
            let x = lerp_line(a, b, y)?.clamp(xl, xr);
            self.crossings[k as usize].push(u32::try_from(x - xl).ok()?);
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alto_core::Affine2;

    fn square(x: i64, y: i64, size: i64) -> Vec<Command> {
        let q = |v| v * 256;
        vec![
            Command::Move([q(x), q(y)]),
            Command::Line([q(x + size), q(y)]),
            Command::Line([q(x + size), q(y + size)]),
            Command::Line([q(x), q(y + size)]),
            Command::Close,
        ]
    }

    #[test]
    fn integer_square_and_window_crop() {
        let path = square(1, 1, 2);
        let full = rasterize(&path, FillRule::NonZero, [0, 0, 4, 4], 4096).unwrap();
        assert_eq!(&full[4..8], &[0, 255, 255, 0]);
        assert_eq!(&full[8..12], &[0, 255, 255, 0]);
        let crop = rasterize(&path, FillRule::NonZero, [1, 1, 2, 2], 4096).unwrap();
        assert_eq!(crop, vec![255; 4]);
    }

    #[test]
    fn even_odd_cancels_identical_subpaths() {
        let mut path = square(0, 0, 2);
        path.extend(square(0, 0, 2));
        assert_eq!(
            rasterize(&path, FillRule::EvenOdd, [0, 0, 2, 2], 4096).unwrap(),
            vec![0; 4]
        );
    }

    #[test]
    fn half_pixel_edges_and_negative_window_are_stable() {
        let path = vec![
            Command::Move([-128, -128]),
            Command::Line([384, -128]),
            Command::Line([384, 384]),
            Command::Line([-128, 384]),
            Command::Close,
        ];
        for rule in [FillRule::NonZero, FillRule::EvenOdd] {
            let full = rasterize(&path, rule, [-1, -1, 4, 4], 4096).unwrap();
            assert_eq!(full[0], 64);
            assert_eq!(full[1], 128);
            assert_eq!(full[5], 255);
            let crop = rasterize(&path, rule, [0, 0, 2, 2], 4096).unwrap();
            assert_eq!(crop, vec![full[5], full[6], full[9], full[10]]);
        }
    }

    #[test]
    fn curves_are_independent_of_the_window() {
        let path = vec![
            Command::Move([-512, -256]),
            Command::Cubic([2048, -512], [-512, 2048], [2048, 1024]),
            Command::Line([-512, 1024]),
            Command::Close,
        ];
        for rule in [FillRule::NonZero, FillRule::EvenOdd] {
            let full = rasterize(&path, rule, [-2, -2, 12, 12], 4096).unwrap();
            let crop = rasterize(&path, rule, [1, 1, 4, 4], 4096).unwrap();
            for y in 0..4 {
                assert_eq!(
                    &crop[y * 4..y * 4 + 4],
                    &full[(y + 3) * 12 + 3..(y + 3) * 12 + 7]
                );
            }
        }
    }

    #[test]
    fn document_points_use_the_q32_world_at_every_level() {
        let path = PathData {
            commands: vec![PathCommand::Move { x: 1.0, y: -2.0 }],
        };
        let world = FixedAffine::from_affine(Affine2([1.0, 0.0, 0.0, 1.0, 0.5, 0.0])).unwrap();
        assert!(matches!(
            map_path(&path, world, 0).unwrap()[0],
            Command::Move([384, -512])
        ));
        assert!(matches!(
            map_path(&path, world, -1).unwrap()[0],
            Command::Move([768, -1024])
        ));
        assert!(matches!(
            map_path(&path, world, 1).unwrap()[0],
            Command::Move([192, -256])
        ));
        assert_eq!(roi_grid(-256.0, 128.0, 0.5), Some([-512, 256, -1]));
        assert_eq!(q_from_f32_bits((2f32.powi(-15)).to_bits(), 14), Some(1));
        assert_eq!(
            q_from_f32_bits(f32::from_bits((2f32.powi(-15)).to_bits() - 1).to_bits(), 14),
            Some(0)
        );
    }

    /// A closed polygon of Q8 points.
    fn polygon(points: &[[i64; 2]]) -> Vec<Command> {
        let mut out = vec![Command::Move(points[0])];
        out.extend(points[1..].iter().map(|&p| Command::Line(p)));
        out.push(Command::Close);
        out
    }
    /// The rectangle with doubled document corners (65 is 32.5), clockwise in
    /// y-down pixels.
    fn rect2(x0: i64, y0: i64, x1: i64, y1: i64) -> Vec<Command> {
        let q = |v: i64| v * 128;
        polygon(&[
            [q(x0), q(y0)],
            [q(x1), q(y0)],
            [q(x1), q(y1)],
            [q(x0), q(y1)],
        ])
    }
    fn reversed(path: &[Command]) -> Vec<Command> {
        // Polygons only: reverse each subpath's point order.
        let mut out = vec![];
        let mut points = vec![];
        let flush = |points: &mut Vec<[i64; 2]>, out: &mut Vec<Command>| {
            if !points.is_empty() {
                points.reverse();
                out.extend(polygon(points));
                points.clear();
            }
        };
        for command in path {
            match *command {
                Command::Move(p) => {
                    flush(&mut points, &mut out);
                    points.push(p);
                }
                Command::Line(p) => points.push(p),
                Command::Close => flush(&mut points, &mut out),
                _ => unreachable!(),
            }
        }
        flush(&mut points, &mut out);
        out
    }
    fn draw(path: &[Command], rule: FillRule) -> Vec<u8> {
        rasterize(path, rule, [0, 0, 256, 256], CURVE_LIMIT).unwrap()
    }
    fn at(bytes: &[u8], x: usize, y: usize) -> u8 {
        bytes[y * 256 + x]
    }

    #[test]
    fn coverage_byte_rounds_the_single_tie_away_from_zero() {
        for (e, byte) in [
            (0, 0),
            (1, 0),
            (65535, 127),
            (65536, 128),
            (65537, 128),
            (131071, 255),
            (131072, 255),
            (131073, 255),
            (262144, 255),
            ((1u128 << 43) - 1, 255),
        ] {
            assert_eq!(coverage_byte(e), byte, "e {e}");
        }
        // Even-odd: e = 32 T.
        for (t, byte) in [
            (0, 0),
            (1, 0),
            (2047, 127),
            (2048, 128),
            (2049, 128),
            (4095, 255),
            (4096, 255),
        ] {
            assert_eq!(coverage_byte(32 * t), byte, "T {t}");
        }
    }

    #[test]
    fn vector_fill_rules() {
        // A pentagram (self-intersecting): nonzero fills the center, even-odd not.
        let star: Vec<[i64; 2]> = (0..5)
            .map(|i| {
                let a = (-90.0f64 + 144.0 * f64::from(i)).to_radians();
                [
                    ((128.0 + 100.0 * a.cos()) * 256.0).round() as i64,
                    ((128.0 + 100.0 * a.sin()) * 256.0).round() as i64,
                ]
            })
            .collect();
        let star = polygon(&star);
        assert_eq!(at(&draw(&star, FillRule::NonZero), 128, 128), 255);
        assert_eq!(at(&draw(&star, FillRule::EvenOdd), 128, 128), 0);
        // Outer clockwise, inner counter-clockwise: a hole under both rules.
        let mut hole = rect2(32, 32, 224, 224);
        hole.extend(reversed(&rect2(96, 96, 160, 160)));
        for rule in [FillRule::NonZero, FillRule::EvenOdd] {
            let bytes = draw(&hole, rule);
            assert_eq!(at(&bytes, 64, 64), 0);
            assert_eq!(at(&bytes, 20, 64), 255);
        }
        // Both clockwise: nonzero has no hole, even-odd cancels the overlap.
        let mut same = rect2(32, 32, 224, 224);
        same.extend(rect2(96, 96, 160, 160));
        assert_eq!(at(&draw(&same, FillRule::NonZero), 64, 64), 255);
        assert_eq!(at(&draw(&same, FillRule::EvenOdd), 64, 64), 0);
        // Even-odd does not read the direction of its segments.
        for path in [&star, &hole, &same] {
            assert_eq!(
                draw(path, FillRule::EvenOdd),
                draw(&reversed(path), FillRule::EvenOdd)
            );
        }
    }

    #[test]
    fn vector_even_odd_shared_edge() {
        // (a) The same square twice, same direction: winding 2 inside.
        let square = rect2(65, 65, 401, 401);
        let mut twice = square.clone();
        twice.extend(square.clone());
        assert!(draw(&twice, FillRule::EvenOdd).iter().all(|&b| b == 0));
        let nonzero = draw(&twice, FillRule::NonZero);
        assert_eq!(at(&nonzero, 100, 100), 255);
        assert_eq!(at(&nonzero, 100, 32), 255);
        assert_eq!(at(&nonzero, 32, 100), 255);
        for (x, y) in [(32, 32), (200, 32), (32, 200), (200, 200)] {
            assert_eq!(at(&nonzero, x, y), 128, "corner ({x}, {y})");
        }
        // (b) A and B share the right, top and bottom edges.
        let mut ab = square.clone();
        ab.extend(rect2(193, 65, 401, 401));
        let even = draw(&ab, FillRule::EvenOdd);
        let nonzero = draw(&ab, FillRule::NonZero);
        for x in 33..=95 {
            assert_eq!(at(&even, x, 100), 255, "col {x}");
            assert_eq!(at(&even, x, 32), 128, "top col {x}");
        }
        for x in [32, 96] {
            assert_eq!(at(&even, x, 100), 128, "col {x}");
            assert_eq!(at(&even, x, 32), 64, "top col {x}");
        }
        for x in 97..=200 {
            assert_eq!(at(&even, x, 100), 0, "col {x}");
            assert_eq!(at(&even, x, 32), 0, "top col {x}");
        }
        for x in 33..=199 {
            assert_eq!(at(&nonzero, x, 100), 255, "col {x}");
        }
        assert_eq!(at(&nonzero, 32, 100), 128);
        assert_eq!(at(&nonzero, 96, 100), 255);
        assert_eq!(at(&nonzero, 200, 100), 255);
        // (c) Two squares side by side: the shared edge leaves no seam.
        let mut pair = rect2(65, 65, 193, 401);
        pair.extend(rect2(193, 65, 321, 401));
        for rule in [FillRule::NonZero, FillRule::EvenOdd] {
            let bytes = draw(&pair, rule);
            for y in 33..=199 {
                assert_eq!(at(&bytes, 96, y), 255, "row {y}");
            }
            assert_eq!(at(&bytes, 96, 32), 128);
            assert_eq!(at(&bytes, 96, 200), 128);
        }
    }

    #[test]
    fn vector_q8_window_crop() {
        // Edges through the windows, a curve split on a window edge, a window
        // one pixel right of a point, and a path wider than 2^24 level pixels.
        let paths = [
            vec![
                Command::Move([-512, -256]),
                Command::Cubic([4096, -512], [-512, 4096], [4096, 2048]),
                Command::Quad([2048, 1024], [-512, 2048]),
                Command::Close,
            ],
            vec![
                Command::Move([255, 0]),
                Command::Quad([256, 15 * 256], [255, 0]),
                Command::Line([1024, 2048]),
                Command::Close,
            ],
            polygon(&[
                [-(1 << 33), 300],
                [1 << 33, 700],
                [1 << 33, 3000],
                [-(1 << 33), 2600],
            ]),
        ];
        for path in &paths {
            for rule in [FillRule::NonZero, FillRule::EvenOdd] {
                let full = rasterize(path, rule, [-4, -4, 24, 24], CURVE_LIMIT).unwrap();
                for (wx, wy) in [(-4, -4), (8, -4), (-4, 8), (8, 8)] {
                    let part = rasterize(path, rule, [wx, wy, 12, 12], CURVE_LIMIT).unwrap();
                    for y in 0..12 {
                        let row = (y + (wy + 4) as usize) * 24 + (wx + 4) as usize;
                        assert_eq!(&part[y * 12..y * 12 + 12], &full[row..row + 12]);
                    }
                }
            }
        }
    }

    #[test]
    fn raster_q8_skips_are_exact() {
        // Curves that leave the window above, below, left and right, and a
        // cubic that goes up and down many times left of the window.
        let mut zigzag = vec![Command::Move([-4096, 0])];
        for k in 0..12 {
            let y = if k % 2 == 0 { 6000 } else { -1000 };
            zigzag.push(Command::Cubic(
                [-9000, y],
                [-200, -y],
                [-3000 - 37 * k, y / 2],
            ));
        }
        zigzag.push(Command::Line([3000, 2600]));
        zigzag.push(Command::Close);
        let paths = [
            vec![
                Command::Move([-2048, -2048]),
                Command::Cubic([4096, -9000], [6000, 9000], [5000, 4000]),
                Command::Quad([-7000, 2000], [-2048, -2048]),
                Command::Close,
            ],
            vec![
                Command::Move([300, 300]),
                Command::Cubic([9000, -5000], [12000, 8000], [700, 2800]),
                Command::Quad([-6000, 9000], [300, 300]),
                Command::Close,
            ],
            zigzag,
        ];
        for path in &paths {
            for rule in [FillRule::NonZero, FillRule::EvenOdd] {
                for window in [
                    [0, 0, 12, 12],
                    [-3, 2, 8, 5],
                    [5, -4, 6, 9],
                    [-30, -30, 70, 70],
                ] {
                    assert_eq!(
                        rasterize_with(path, rule, window, Curves::Reject(CURVE_LIMIT), true),
                        rasterize_with(path, rule, window, Curves::Reject(CURVE_LIMIT), false),
                        "{rule:?} {window:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn curve_range_is_refused_by_the_path_not_the_window() {
        // 65536 n^4 >= K^2 D at n = 4096 is D <= 2^64 / K^2.
        for k in [10u128, 30] {
            let limit = (1u128 << 64) / (k * k);
            assert_eq!(divisions(limit, k, Curves::Reject(CURVE_LIMIT)), Some(4096));
            assert_eq!(divisions(limit + 1, k, Curves::Reject(CURVE_LIMIT)), None);
            // The v3 first-draft limit n = 256 (about 6554 / 2185 pixels).
            let small = (1u128 << 48) / (k * k);
            assert_eq!(divisions(small, k, Curves::Reject(CURVE_LIMIT)), Some(256));
            assert_eq!(
                divisions(small + 1, k, Curves::Reject(CURVE_LIMIT)),
                Some(257)
            );
        }
        // A cubic whose second difference is far past 559_241 level pixels.
        let far = 600_000i64 * 256 * 4;
        let path = vec![
            Command::Move([0, 0]),
            Command::Cubic([far, 0], [-far, far], [256, 256]),
            Command::Close,
        ];
        for window in [[0, 0, 4, 4], [10_000, 10_000, 4, 4]] {
            for rule in [FillRule::NonZero, FillRule::EvenOdd] {
                assert_eq!(
                    rasterize(&path, rule, window, CURVE_LIMIT),
                    Err(RasterError::CurveRange)
                );
            }
        }
    }
}
