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
    normalized_iter(commands.iter().copied())
}

/// `normalized` over commands read twice from `commands`.
fn normalized_iter(
    commands: impl Iterator<Item = Command> + Clone,
) -> Option<(Vec<Command>, [i64; 2])> {
    let mut min = [i64::MAX; 2];
    let mut found = false;
    for command in commands.clone() {
        let mut visit = |p: &[i64; 2]| {
            found = true;
            for axis in 0..2 {
                min[axis] = min[axis].min(p[axis]);
            }
        };
        match &command {
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
    let mut out = Vec::with_capacity(commands.size_hint().0);
    for command in commands {
        out.push(match command {
            Command::Move(p) => Command::Move(shift(p)?),
            Command::Line(p) => Command::Line(shift(p)?),
            Command::Quad(a, b) => Command::Quad(shift(a)?, shift(b)?),
            Command::Cubic(a, b, c) => Command::Cubic(shift(a)?, shift(b)?, shift(c)?),
            Command::Close => Command::Close,
        });
    }
    Some((out, origin))
}

/// A command list kept compactly: a tag byte per command and its points, about
/// a third of `Vec<Command>` for the line-only stroke outlines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Packed {
    tags: Vec<u8>,
    points: Vec<[i64; 2]>,
}

impl Packed {
    pub fn new(commands: &[Command]) -> Self {
        let mut packed = Self::default();
        for command in commands {
            let (tag, points): (u8, &[[i64; 2]]) = match command {
                Command::Move(p) => (0, std::slice::from_ref(p)),
                Command::Line(p) => (1, std::slice::from_ref(p)),
                Command::Quad(a, b) => (2, &[*a, *b]),
                Command::Cubic(a, b, c) => (3, &[*a, *b, *c]),
                Command::Close => (4, &[]),
            };
            packed.tags.push(tag);
            packed.points.extend_from_slice(points);
        }
        packed.tags.shrink_to_fit();
        packed.points.shrink_to_fit();
        packed
    }

    /// The commands, in order.
    pub fn iter(&self) -> impl Iterator<Item = Command> + Clone + '_ {
        let mut points = self.points.iter().copied();
        self.tags.iter().map(move |tag| {
            let mut next = || points.next().expect("a packed point");
            match tag {
                0 => Command::Move(next()),
                1 => Command::Line(next()),
                2 => Command::Quad(next(), next()),
                3 => Command::Cubic(next(), next(), next()),
                _ => Command::Close,
            }
        })
    }

    /// The heap bytes held.
    pub fn bytes(&self) -> usize {
        self.tags.capacity() + self.points.capacity() * std::mem::size_of::<[i64; 2]>()
    }
}

/// Commands normalized once for rasterizing into many windows (a vector path
/// or stroke outline on every tile).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    commands: Packed,
    origin: [i64; 2],
    /// The smallest and largest normalized point, control points included.
    bounds: Option<[[i64; 2]; 2]>,
    /// What flattening returns for a nonzero window that every point is on or
    /// past one edge of. It is the same for every such window: each curve is
    /// dropped or replaced by its chord before it is subdivided (the curve and
    /// interpolation checks before that do not read the window), and with every
    /// coordinate at most `2^60` no segment's interpolation can overflow. `None`
    /// for larger coordinates, which are flattened as before.
    outside: Option<Result<(), RasterError>>,
    /// The curve limit `outside` was found with, used for every window.
    curve_limit: u32,
}

/// Normalized coordinates up to this keep `NonZero::segment`'s interpolation
/// inside `i128` and `i64` for any window.
const PREPARED_EXTENT: i64 = 1 << 60;

impl Prepared {
    /// `Err` is the error `rasterize` gives for every window when the commands
    /// cannot be normalized.
    pub fn new(commands: &[Command], curve_limit: u32) -> Result<Self, RasterError> {
        let (normal, origin) = normalized(commands).ok_or(RasterError::Range)?;
        let mut bounds: Option<[[i64; 2]; 2]> = None;
        for command in &normal {
            let points: &[[i64; 2]] = match command {
                Command::Move(p) | Command::Line(p) => std::slice::from_ref(p),
                Command::Quad(a, b) => &[*a, *b],
                Command::Cubic(a, b, c) => &[*a, *b, *c],
                Command::Close => &[],
            };
            for p in points {
                let b = bounds.get_or_insert([*p, *p]);
                for axis in 0..2 {
                    b[0][axis] = b[0][axis].min(p[axis]);
                    b[1][axis] = b[1][axis].max(p[axis]);
                }
            }
        }
        // Normalization subtracts the smallest point, so every coordinate is
        // already at least 0; both ends are checked so the bound does not rest
        // on that.
        let [min, max] = bounds.map_or([0, 0], |[min, max]| {
            [min[0].min(min[1]), max[0].max(max[1])]
        });
        let outside = (min >= 0 && max <= PREPARED_EXTENT).then(|| {
            // A one-pixel window right of every point.
            let lx = max.div_euclid(256) + 1;
            let window = Window {
                lx,
                ly: 0,
                width: 1,
                height: 1,
                xl: lx * 256,
                xr: lx * 256 + 256,
                yt: 0,
                yb: 256,
            };
            let mut sink = NonZero::<i64>::new(window, true);
            flatten(
                &normal,
                Curves::Reject(curve_limit),
                window,
                true,
                true,
                &mut sink,
            )
        });
        Ok(Self {
            commands: Packed::new(&normal),
            origin,
            bounds,
            outside,
            curve_limit,
        })
    }

    /// Whether every point is on or past one edge of `window` (`outside`).
    fn is_outside(&self, window: Window) -> bool {
        let Some([min, max]) = self.bounds else {
            return true;
        };
        max[0] <= window.xl || min[0] >= window.xr || max[1] <= window.yt || min[1] >= window.yb
    }

    /// The heap bytes held.
    pub fn bytes(&self) -> usize {
        self.commands.bytes()
    }
}

/// `rasterize_crop` of prepared commands, with the curve limit they were
/// prepared for.
pub fn rasterize_crop_prepared(
    prepared: &Prepared,
    rule: FillRule,
    window: [i64; 4],
) -> Result<Option<Crop>, RasterError> {
    let (bytes, bound) = rasterize_normalized(
        Normal::Prepared(prepared),
        prepared.origin,
        rule,
        window,
        Curves::Reject(prepared.curve_limit),
        true,
        false,
    )?;
    let width = window[2] as usize;
    Ok(bound.and_then(|bound| crop_within(&bytes, width, bound)))
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
    close_open: bool,
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
                if close_open && let (Some(a), Some(b)) = (current, start) {
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
    if close_open && let (Some(a), Some(b)) = (current, start) {
        emit(a, b)?;
    }
    Ok(())
}

/// One flattened line segment, start then end, in Q8 pixels.
pub(crate) type Segment = ([i64; 2], [i64; 2]);

/// Flatten one stroke subpath without implicitly closing its last segment.
/// Curve subdivision is the same integer rule as fill coverage. The bounded
/// output prevents a pathological path from allocating unbounded stroke
/// geometry; `CurveRange` skips that path on every tile.
pub(crate) fn flatten_stroke(commands: &[Command]) -> Result<Vec<Segment>, RasterError> {
    struct Segments(Vec<Segment>);
    impl Sink for Segments {
        fn segment(&mut self, a: [i64; 2], b: [i64; 2]) -> Option<()> {
            if self.0.len() >= 1 << 18 {
                return None;
            }
            self.0.push((a, b));
            Some(())
        }
    }
    let mut sink = Segments(Vec::new());
    let empty = Window {
        lx: 0,
        ly: 0,
        width: 0,
        height: 0,
        xl: 0,
        xr: 0,
        yt: 0,
        yb: 0,
    };
    flatten(
        commands,
        Curves::Reject(CURVE_LIMIT),
        empty,
        false,
        false,
        &mut sink,
    )
    .map_err(|e| {
        if e == RasterError::Range {
            RasterError::CurveRange
        } else {
            e
        }
    })?;
    Ok(sink.0)
}

/// The segments `rasterize` integrates for a fill, in the input's Q8 pixels:
/// the same normalization and subdivision, no window skipping. `None` past
/// `limit` segments, a range error, or the curve limit.
pub(crate) fn fill_segments(commands: &[Command], limit: usize) -> Option<Vec<Segment>> {
    struct Segments(Vec<Segment>, usize);
    impl Sink for Segments {
        fn segment(&mut self, a: [i64; 2], b: [i64; 2]) -> Option<()> {
            if self.0.len() >= self.1 {
                return None;
            }
            self.0.push((a, b));
            Some(())
        }
    }
    let (commands, origin) = normalized(commands)?;
    let empty = Window {
        lx: 0,
        ly: 0,
        width: 0,
        height: 0,
        xl: 0,
        xr: 0,
        yt: 0,
        yb: 0,
    };
    let mut sink = Segments(Vec::new(), limit);
    flatten(
        &commands,
        Curves::Reject(CURVE_LIMIT),
        empty,
        false,
        true,
        &mut sink,
    )
    .ok()?;
    let [ox, oy] = [origin[0].checked_mul(256)?, origin[1].checked_mul(256)?];
    sink.0
        .into_iter()
        .map(|(a, b)| {
            Some((
                [a[0].checked_add(ox)?, a[1].checked_add(oy)?],
                [b[0].checked_add(ox)?, b[1].checked_add(oy)?],
            ))
        })
        .collect()
}

/// A vector layer's premultiplied Q14 pixels at level 0, as the v3 renderer paints
/// them: fill before stroke, every path over the last, with the same coverage and
/// blend. `window` is `[x, y, width, height]` in world pixels. A path whose integer
/// curve budget is exceeded is skipped, as it is on screen.
pub fn vector_pixels_q14(
    paths: &[alto_vector::StyledPath],
    world: FixedAffine,
    window: [i64; 4],
) -> Result<Vec<[u16; 4]>, RasterError> {
    let len = usize::try_from(window[2].checked_mul(window[3]).ok_or(RasterError::Range)?)
        .map_err(|_| RasterError::Range)?;
    let mut out = vec![[0u16; 4]; len];
    for path in paths {
        let commands = map_path(&path.data, world, 0).ok_or(RasterError::Range)?;
        for paint in 0..2 {
            let paint_color = if paint == 0 {
                path.fill
            } else {
                path.stroke
                    .as_ref()
                    .filter(|stroke| stroke_visible(stroke))
                    .map(|stroke| stroke.color)
            };
            let Some(paint_color) = paint_color else {
                continue;
            };
            let color = color_q14(paint_color).ok_or(RasterError::Range)?;
            if color[3] == 0 {
                continue;
            }
            let coverage = if paint == 0 {
                let rule = match path.fill_rule {
                    alto_vector::FillRule::NonZero => FillRule::NonZero,
                    alto_vector::FillRule::EvenOdd => FillRule::EvenOdd,
                };
                rasterize(&commands, rule, window, CURVE_LIMIT)
            } else {
                crate::stroke_q8::rasterize_stroke(
                    &path.data,
                    path.stroke.as_ref().expect("visible stroke"),
                    world,
                    0,
                    window,
                )
            };
            let coverage = match coverage {
                Ok(coverage) => coverage,
                Err(RasterError::CurveRange) => continue,
                Err(RasterError::Range) => return Err(RasterError::Range),
            };
            for (pixel, c) in out.iter_mut().zip(coverage) {
                if c != 0 {
                    *pixel = source_over_q14(*pixel, color, c);
                }
            }
        }
    }
    Ok(out)
}

/// Rasterize a path into the level-pixel window. An error means a defined range
/// limit was exceeded; callers must not silently substitute float rendering.
pub fn rasterize(
    commands: &[Command],
    rule: FillRule,
    window: [i64; 4],
    curve_limit: u32,
) -> Result<Vec<u8>, RasterError> {
    rasterize_with(
        commands,
        rule,
        window,
        Curves::Reject(curve_limit),
        true,
        false,
    )
}

/// A coverage cropped to its nonzero bytes: top-left in window pixels, width,
/// height and the row-major bytes.
pub type Crop = ([u32; 2], u32, u32, Vec<u8>);

/// `rasterize` cropped to the smallest rectangle that holds every nonzero byte
/// (`crop`), `None` when every byte is zero. Only the rectangle the rasterizer
/// wrote is scanned. Painting the crop at its top-left gives the same Q14 result
/// as painting the whole coverage, since a zero byte leaves a pixel unchanged.
pub fn rasterize_crop(
    commands: &[Command],
    rule: FillRule,
    window: [i64; 4],
    curve_limit: u32,
) -> Result<Option<Crop>, RasterError> {
    rasterize_crop_iter(commands.iter().copied(), rule, window, curve_limit)
}

fn rasterize_crop_iter(
    commands: impl Iterator<Item = Command> + Clone,
    rule: FillRule,
    window: [i64; 4],
    curve_limit: u32,
) -> Result<Option<Crop>, RasterError> {
    let (bytes, bound) = rasterize_bounded(
        commands,
        rule,
        window,
        Curves::Reject(curve_limit),
        true,
        false,
    )?;
    let width = window[2] as usize;
    Ok(bound.and_then(|bound| crop_within(&bytes, width, bound)))
}

/// The smallest rectangle of a row-major `w`×`h` coverage that holds every
/// nonzero byte: its top-left, width, height and bytes. `None` when every byte
/// is zero.
pub fn crop(coverage: &[u8], w: u32, h: u32) -> Option<Crop> {
    let (w, h) = (w as usize, h as usize);
    assert_eq!(coverage.len(), w * h, "coverage is w*h bytes");
    crop_within(coverage, w, [0, 0, w, h])
}

/// `crop`, scanning only `bound` (`[x0, y0, x1, y1)`), outside which every
/// byte must be zero.
fn crop_within(coverage: &[u8], w: usize, bound: [usize; 4]) -> Option<Crop> {
    let [bx0, by0, bx1, by1] = bound;
    let (mut x0, mut x1, mut y0, mut y1) = (bx1, bx0, by1, by0);
    for y in by0..by1 {
        let row = &coverage[y * w + bx0..y * w + bx1];
        let Some(first) = row.iter().position(|&v| v != 0) else {
            continue;
        };
        let last = row.iter().rposition(|&v| v != 0).expect("a nonzero byte");
        x0 = x0.min(bx0 + first);
        x1 = x1.max(bx0 + last + 1);
        y0 = y0.min(y);
        y1 = y + 1;
    }
    if y0 >= y1 {
        return None;
    }
    let mut bytes = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        bytes.extend_from_slice(&coverage[y * w + x0..y * w + x1]);
    }
    Some((
        [x0 as u32, y0 as u32],
        (x1 - x0) as u32,
        (y1 - y0) as u32,
        bytes,
    ))
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
        false,
    )
}

/// `skip` drops curves that cannot reach the window (`flatten`); `whole_rows`
/// stores and integrates every nonzero cell, even of a path outside the window,
/// instead of the written ones. Both only exist so tests can compare the
/// shortcuts with the full computation.
fn rasterize_with(
    commands: &[Command],
    rule: FillRule,
    window: [i64; 4],
    curve_limit: Curves,
    skip: bool,
    whole_rows: bool,
) -> Result<Vec<u8>, RasterError> {
    rasterize_bounded(
        commands.iter().copied(),
        rule,
        window,
        curve_limit,
        skip,
        whole_rows,
    )
    .map(|(bytes, _)| bytes)
}

/// `rasterize_with`, and a rectangle `[x0, y0, x1, y1)` of window pixels outside
/// which every byte is zero. It may hold only zeros; `None` means no byte was
/// written, so every byte is zero.
fn rasterize_bounded(
    commands: impl Iterator<Item = Command> + Clone,
    rule: FillRule,
    window: [i64; 4],
    curve_limit: Curves,
    skip: bool,
    whole_rows: bool,
) -> Result<(Vec<u8>, Option<[usize; 4]>), RasterError> {
    let (commands, origin) = normalized_iter(commands).ok_or(RasterError::Range)?;
    rasterize_normalized(
        Normal::Owned(commands),
        origin,
        rule,
        window,
        curve_limit,
        skip,
        whole_rows,
    )
}

/// Normalized commands for `rasterize_normalized`: just made, or prepared once
/// for many windows.
enum Normal<'a> {
    Owned(Vec<Command>),
    Prepared(&'a Prepared),
}

/// `rasterize_bounded` after normalization. With `Normal::Prepared`, a path
/// whose result outside the window is known returns it without flattening and
/// without the bytes (`Prepared::outside`).
fn rasterize_normalized(
    commands: Normal<'_>,
    origin: [i64; 2],
    rule: FillRule,
    window: [i64; 4],
    curve_limit: Curves,
    skip: bool,
    whole_rows: bool,
) -> Result<(Vec<u8>, Option<[usize; 4]>), RasterError> {
    use RasterError::Range;
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
    if let Normal::Prepared(prepared) = &commands
        && len > 0
        && matches!(rule, FillRule::NonZero)
        && skip
        && !whole_rows
        && let Some(result) = prepared.outside
        && prepared.is_outside(window)
    {
        return result.map(|()| (vec![], None));
    }
    let commands = match commands {
        Normal::Owned(commands) => commands,
        Normal::Prepared(prepared) => prepared.commands.iter().collect(),
    };
    let mut bytes = vec![0u8; len];
    if len == 0 {
        return Ok((bytes, None));
    }
    let bound = match rule {
        FillRule::NonZero => {
            // A path whose points (curve control points included, which bound the
            // flattened segments) are all above, below, left or right of the window
            // covers none of it: a closed path's crossings left of the window cancel
            // in every row. It is still flattened, so the curve range and the
            // interpolation range are refused exactly as before, but nothing is
            // stored or integrated (`raster_q8_touched_cells_are_exact`).
            let probe = !whole_rows && outside(&commands, window);
            if probe {
                let mut sink = NonZero::<i64>::new(window, true);
                flatten(&commands, curve_limit, window, skip, true, &mut sink)?;
                return Ok((bytes, None));
            }
            match nonzero::<i64>(&commands, curve_limit, window, skip, whole_rows, &mut bytes) {
                Err(Integrate::Wide) => {
                    bytes.fill(0);
                    match nonzero::<i128>(
                        &commands,
                        curve_limit,
                        window,
                        skip,
                        whole_rows,
                        &mut bytes,
                    ) {
                        Ok(Ok(bound)) => bound,
                        Ok(Err(error)) => return Err(error),
                        Err(_) => return Err(Range),
                    }
                }
                Err(Integrate::Range) => return Err(Range),
                Ok(Err(error)) => return Err(error),
                Ok(Ok(bound)) => bound,
            }
        }
        FillRule::EvenOdd => {
            let mut sink = EvenOdd {
                window,
                crossings: vec![vec![]; height * 16],
            };
            flatten(&commands, curve_limit, window, skip, true, &mut sink)?;
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
            Some([0, 0, width, height])
        }
    };
    Ok((bytes, bound))
}

/// Flatten into a storing `NonZero<A>` and integrate. The outer `Err` is an
/// overflow of `A`'s sums (`Wide` only for `i64`); `Ok(Err)` is flattening's
/// own error, which no sum caused.
fn nonzero<A: Acc>(
    commands: &[Command],
    curve_limit: Curves,
    window: Window,
    skip: bool,
    whole_rows: bool,
    bytes: &mut [u8],
) -> Result<Result<Option<[usize; 4]>, RasterError>, Integrate> {
    let mut sink = NonZero::<A>::new(window, false);
    if let Err(error) = flatten(commands, curve_limit, window, skip, true, &mut sink) {
        if sink.wide {
            return Err(Integrate::Wide);
        }
        return Ok(Err(error));
    }
    sink.integrate(bytes, whole_rows).map(Ok)
}

/// Nonzero: the cell integration's `cover` and `area`, per window cell, and
/// per row the first and last column written (`(usize::MAX, 0)` for none).
/// A `probe` stores nothing: it only meets the same range checks.
///
/// The sums are defined in `i128` (`A = i128`). They are first tried in `i64`:
/// each step's terms are small (a piece's `dy` is at most 256 and its offsets
/// from the cell's left edge at most 256 each), so only a running sum can pass
/// `i64`. Then `wide` is set, the `i64` attempt stops, and the path is
/// integrated again in `i128`, which gives the same bytes and errors as if the
/// first try had not happened (`raster_q8_narrow_sums_match_the_wide_ones`).
struct NonZero<A> {
    window: Window,
    cover: Vec<A>,
    area: Vec<A>,
    touched: Vec<(usize, usize)>,
    probe: bool,
    wide: bool,
}

/// A nonzero accumulator: `i128`, or `i64` for the first try.
trait Acc: Copy + Default {
    /// One piece's cover and area steps, or `None` past this type's range.
    fn step(dir: i8, ux: i64, vx: i64, xbase: i128, va: i64, vb: i64) -> Option<(Self, Self)>;
    fn add(self, other: Self) -> Option<Self>;
    /// `|sum * 512 - area|`, or `None` past this type's range.
    fn coverage(sum: Self, area: Self) -> Option<u128>;
    /// Whether exceeding this type means trying again in `i128`.
    const NARROW: bool;
}

impl Acc for i128 {
    fn step(dir: i8, ux: i64, vx: i64, xbase: i128, va: i64, vb: i64) -> Option<(Self, Self)> {
        let dir = i128::from(dir);
        let dy = i128::from(vb) - i128::from(va);
        Some((
            dir * dy,
            dir * (i128::from(ux) + i128::from(vx) - 2 * xbase) * dy,
        ))
    }
    fn add(self, other: Self) -> Option<Self> {
        self.checked_add(other)
    }
    fn coverage(sum: Self, area: Self) -> Option<u128> {
        Some(sum.checked_mul(512)?.checked_sub(area)?.unsigned_abs())
    }
    const NARROW: bool = false;
}

impl Acc for i64 {
    fn step(dir: i8, ux: i64, vx: i64, xbase: i128, va: i64, vb: i64) -> Option<(Self, Self)> {
        let dir = i64::from(dir);
        let xbase = i64::try_from(xbase).ok()?;
        let dy = vb.checked_sub(va)?;
        let offsets = ux.checked_sub(xbase)?.checked_add(vx.checked_sub(xbase)?)?;
        Some((
            dir.checked_mul(dy)?,
            dir.checked_mul(offsets)?.checked_mul(dy)?,
        ))
    }
    fn add(self, other: Self) -> Option<Self> {
        self.checked_add(other)
    }
    fn coverage(sum: Self, area: Self) -> Option<u128> {
        Some(u128::from(
            sum.checked_mul(512)?.checked_sub(area)?.unsigned_abs(),
        ))
    }
    const NARROW: bool = true;
}

/// Why `NonZero::integrate` stopped.
enum Integrate {
    Range,
    /// An `i64` sum overflowed: integrate again in `i128`.
    Wide,
}

/// Whether every point of `commands` is on or past one edge of the window.
fn outside(commands: &[Command], window: Window) -> bool {
    let (mut min, mut max) = ([i64::MAX; 2], [i64::MIN; 2]);
    for command in commands {
        let points: &[[i64; 2]] = match command {
            Command::Move(p) | Command::Line(p) => std::slice::from_ref(p),
            Command::Quad(a, b) => &[*a, *b],
            Command::Cubic(a, b, c) => &[*a, *b, *c],
            Command::Close => &[],
        };
        for p in points {
            for axis in 0..2 {
                min[axis] = min[axis].min(p[axis]);
                max[axis] = max[axis].max(p[axis]);
            }
        }
    }
    max[0] <= window.xl || min[0] >= window.xr || max[1] <= window.yt || min[1] >= window.yb
}

impl<A: Acc> NonZero<A> {
    fn new(window: Window, probe: bool) -> Self {
        let len = if probe {
            0
        } else {
            window.width * window.height
        };
        Self {
            window,
            cover: vec![A::default(); len],
            area: vec![A::default(); len],
            touched: if probe {
                vec![]
            } else {
                vec![(usize::MAX, 0); window.height]
            },
            probe,
            wide: false,
        }
    }

    /// The coverage bytes from the running sum of each row. Only the cells a
    /// segment wrote are integrated: left of a row's first written cell the sum
    /// and area are 0 (byte 0, as `bytes` starts); right of its last one the area
    /// is 0 and the sum constant, so every byte there is the same. The bytes and
    /// the overflow errors equal integrating the whole row (`whole`), which
    /// `raster_q8_touched_cells_are_exact` checks.
    ///
    /// Returns a rectangle `[x0, y0, x1, y1)` holding every nonzero byte: rows no
    /// segment wrote are 0, and a row is 0 left of its first written cell and,
    /// when its constant byte is 0, right of its last one.
    fn integrate(&self, bytes: &mut [u8], whole: bool) -> Result<Option<[usize; 4]>, Integrate> {
        let fail = if A::NARROW {
            Integrate::Wide
        } else {
            Integrate::Range
        };
        let width = self.window.width;
        let mut bound: Option<[usize; 4]> = None;
        for (row, &(first, last)) in self.touched.iter().enumerate() {
            let (first, last) = if whole {
                (0, width - 1)
            } else if first > last {
                continue;
            } else {
                (first, last)
            };
            let mut sum = A::default();
            for col in first..=last {
                let idx = row * width + col;
                let Some(next) = sum.add(self.cover[idx]) else {
                    return Err(fail);
                };
                sum = next;
                let Some(n) = A::coverage(sum, self.area[idx]) else {
                    return Err(fail);
                };
                bytes[idx] = coverage_byte(n);
            }
            let mut right = last + 1;
            if last + 1 < width {
                let Some(n) = A::coverage(sum, A::default()) else {
                    return Err(fail);
                };
                let byte = coverage_byte(n);
                bytes[row * width + last + 1..(row + 1) * width].fill(byte);
                if byte != 0 {
                    right = width;
                }
            }
            let b = bound.get_or_insert([first, row, right, row + 1]);
            *b = [b[0].min(first), b[1], b[2].max(right), row + 1];
        }
        Ok(bound)
    }
}

impl<A: Acc> Sink for NonZero<A> {
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
            -1i8
        } else {
            1i8
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
                // Outside paths only reach the window's left column, where the
                // area is 0 and the cover (a sum of at most 2^36 i64 steps) cannot
                // overflow, so skipping the store keeps every error.
                if self.probe {
                    continue;
                }
                let idx = cy as usize * width + cx as usize;
                let span = &mut self.touched[cy as usize];
                *span = (span.0.min(cx as usize), span.1.max(cx as usize));
                let xbase = (i128::from(lx) + cx as i128) * 256;
                let stored = A::step(dir, ux, vx, xbase, va, vb).and_then(|(cover, area)| {
                    Some((self.cover[idx].add(cover)?, self.area[idx].add(area)?))
                });
                let Some((cover, area)) = stored else {
                    // `i128` keeps the error; `i64` tries again in `i128`.
                    self.wide |= A::NARROW;
                    return None;
                };
                self.cover[idx] = cover;
                self.area[idx] = area;
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
    #[test]
    fn raster_q8_crop_keeps_every_nonzero_byte() {
        assert_eq!(crop(&[0; 12], 4, 3), None);
        assert_eq!(crop(&[], 0, 0), None);
        let mut seed = 0x9e37_79b9_u32;
        for case in 0..400 {
            let (w, h) = (1 + case % 9, 1 + (case / 9) % 7);
            let mut coverage = vec![0u8; (w * h) as usize];
            for byte in &mut coverage {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *byte = if seed >> 29 == 0 {
                    (seed >> 8) as u8 | 1
                } else {
                    0
                };
            }
            let Some(([x, y], cw, ch, bytes)) = crop(&coverage, w, h) else {
                assert!(coverage.iter().all(|&v| v == 0));
                continue;
            };
            // Painting the crop back over zeros gives the coverage, and every
            // edge of the crop holds a nonzero byte.
            let mut back = vec![0u8; coverage.len()];
            for row in 0..ch {
                for col in 0..cw {
                    back[((y + row) * w + x + col) as usize] = bytes[(row * cw + col) as usize];
                }
            }
            assert_eq!(back, coverage, "case {case}");
            let at = |c: u32, r: u32| bytes[(r * cw + c) as usize] != 0;
            assert!((0..cw).any(|c| at(c, 0)) && (0..cw).any(|c| at(c, ch - 1)));
            assert!((0..ch).any(|r| at(0, r)) && (0..ch).any(|r| at(cw - 1, r)));
        }
    }

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
                        rasterize_with(
                            path,
                            rule,
                            window,
                            Curves::Reject(CURVE_LIMIT),
                            true,
                            false
                        ),
                        rasterize_with(
                            path,
                            rule,
                            window,
                            Curves::Reject(CURVE_LIMIT),
                            false,
                            true
                        ),
                        "{rule:?} {window:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn raster_q8_touched_cells_are_exact() {
        // Deterministic random paths: several subpaths (some left open, so the
        // implicit close matters), lines, quads and cubics, overlapping windings,
        // against windows inside, across and fully left, right, above and below.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |range: i64| -> i64 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % (2 * range as u64 + 1)) as i64 - range
        };
        let mut checked = 0;
        for case in 0..300 {
            let mut path = vec![];
            for sub in 0..1 + case % 4 {
                path.push(Command::Move([next(6000), next(6000)]));
                for _ in 0..1 + (case + sub) % 6 {
                    let kind = next(2).rem_euclid(3);
                    let mut points = [[0i64; 2]; 3];
                    for p in &mut points {
                        *p = [next(6000), next(6000)];
                    }
                    let [a, b, c] = points;
                    path.push(match kind {
                        0 => Command::Line(a),
                        1 => Command::Quad(a, b),
                        _ => Command::Cubic(a, b, c),
                    });
                }
                if sub % 2 == 0 {
                    path.push(Command::Close);
                }
            }
            for window in [
                [0, 0, 16, 16],
                [-24, -24, 48, 48],
                [next(30), next(30), 1 + next(20).abs(), 1 + next(20).abs()],
                [-90, -10, 20, 20],
                [60, -10, 20, 20],
                [-10, -90, 20, 20],
                [-10, 60, 20, 20],
            ] {
                let fast = rasterize_with(
                    &path,
                    FillRule::NonZero,
                    window,
                    Curves::Reject(CURVE_LIMIT),
                    true,
                    false,
                );
                let whole = rasterize_with(
                    &path,
                    FillRule::NonZero,
                    window,
                    Curves::Reject(CURVE_LIMIT),
                    true,
                    true,
                );
                assert_eq!(fast, whole, "case {case} {window:?}");
                checked += usize::from(fast.is_ok_and(|b| b.iter().any(|&v| v != 0)));
            }
        }
        // Most cases draw something, so the comparison is not of empty tiles.
        assert!(checked > 600, "{checked}");
    }

    #[test]
    fn raster_q8_crop_equals_cropping_the_whole_coverage() {
        // The rectangle the rasterizer reports must hold every nonzero byte, for
        // both rules, windows inside, across and outside the path, and errors.
        let mut seed = 0x6a09_e667_f3bc_c909u64;
        let mut next = |range: i64| -> i64 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % (2 * range as u64 + 1)) as i64 - range
        };
        let mut drawn = 0;
        for case in 0..300 {
            let mut path = vec![];
            for sub in 0..1 + case % 3 {
                let scale = [400, 3000, 6000][(case + sub) % 3];
                path.push(Command::Move([next(scale), next(scale)]));
                for _ in 0..1 + (case + sub) % 5 {
                    let mut points = [[0i64; 2]; 3];
                    for p in &mut points {
                        *p = [next(scale), next(scale)];
                    }
                    let [a, b, c] = points;
                    path.push(match next(2).rem_euclid(3) {
                        0 => Command::Line(a),
                        1 => Command::Quad(a, b),
                        _ => Command::Cubic(a, b, c),
                    });
                }
                if sub % 2 == 0 {
                    path.push(Command::Close);
                }
            }
            for rule in [FillRule::NonZero, FillRule::EvenOdd] {
                for window in [
                    [0, 0, 16, 16],
                    [-24, -24, 48, 48],
                    [next(30), next(30), 1 + next(20).abs(), 1 + next(20).abs()],
                    [-90, -10, 20, 20],
                    [60, -10, 20, 20],
                    [-10, 60, 20, 20],
                    [-3, -3, 0, 5],
                ] {
                    let whole = rasterize(&path, rule, window, CURVE_LIMIT);
                    let cropped = rasterize_crop(&path, rule, window, CURVE_LIMIT);
                    let prepared = Prepared::new(&path, CURVE_LIMIT)
                        .and_then(|p| rasterize_crop_prepared(&p, rule, window));
                    assert_eq!(prepared, cropped, "prepared case {case}");
                    let expected = whole.map(|b| crop(&b, window[2] as u32, window[3] as u32));
                    drawn += usize::from(matches!(expected, Ok(Some(_))));
                    assert_eq!(cropped, expected, "case {case} {rule:?} {window:?}");
                }
            }
        }
        assert!(drawn > 900, "{drawn}");
    }

    #[test]
    fn raster_q8_prepared_outside_keeps_every_error() {
        // A prepared path skips flattening for a window it is outside of, and
        // returns the result it found once. Paths that flatten to an error must
        // still give that error, whichever side of the window they are on:
        // a curve with no start point (Range), a curve past the limit
        // (CurveRange), lines only, and coordinates past the shortcut's range.
        let big = 1i64 << 40;
        let huge = 1i64 << 61;
        let paths: Vec<Vec<Command>> = vec![
            vec![Command::Quad([0, 0], [512, 512])],
            vec![Command::Move([0, 0]), Command::Quad([big, 0], [0, big])],
            vec![
                Command::Move([0, 0]),
                Command::Line([2048, 0]),
                Command::Line([0, 2048]),
                Command::Close,
            ],
            vec![
                Command::Move([0, 0]),
                Command::Cubic([3000, -500], [-500, 3000], [2048, 2048]),
                Command::Close,
            ],
            vec![
                Command::Move([0, 0]),
                Command::Line([huge, 0]),
                Command::Line([0, 512]),
            ],
            vec![],
        ];
        let mut outside = 0;
        for (case, path) in paths.iter().enumerate() {
            for window in [
                [0, 0, 16, 16],
                [-40, 0, 20, 20],
                [40, 0, 20, 20],
                [0, -40, 20, 20],
                [0, 40, 20, 20],
                [1 << 40, 0, 8, 8],
                [3, 3, 0, 4],
            ] {
                for rule in [FillRule::NonZero, FillRule::EvenOdd] {
                    let whole = rasterize_crop(path, rule, window, CURVE_LIMIT);
                    let prepared = Prepared::new(path, CURVE_LIMIT);
                    outside += usize::from(prepared.as_ref().is_ok_and(|p| {
                        let [ox, oy, w, h] = window;
                        let lx = ox - p.origin[0];
                        let ly = oy - p.origin[1];
                        p.outside.is_some()
                            && p.is_outside(Window {
                                lx,
                                ly,
                                width: w as usize,
                                height: h as usize,
                                xl: lx * 256,
                                xr: (lx + w) * 256,
                                yt: ly * 256,
                                yb: (ly + h) * 256,
                            })
                    }));
                    let got = prepared.and_then(|p| rasterize_crop_prepared(&p, rule, window));
                    assert_eq!(got, whole, "case {case} {window:?} {rule:?}");
                }
            }
        }
        // The shortcut is taken, including for paths that end in an error.
        assert!(outside > 30, "{outside}");
    }

    #[test]
    fn raster_q8_narrow_sums_match_the_wide_ones() {
        // The i64 first try must give the i128 definition's bytes, bound and
        // error, for ordinary paths and for coordinates near the i64 limit.
        let mut seed = 0xbb67_ae85_84ca_a73bu64;
        let mut next = |range: i64| -> i64 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % (2 * range as u64 + 1)) as i64 - range
        };
        let mut compared = 0;
        for case in 0..240 {
            let scale = [6000i64, 1 << 40, 1 << 61][case % 3];
            let mut path = vec![Command::Move([next(scale), next(scale)])];
            for _ in 0..1 + case % 5 {
                let [a, b] = [[next(scale), next(scale)], [next(scale), next(scale)]];
                path.push(if case % 2 == 0 {
                    Command::Line(a)
                } else {
                    Command::Quad(a, b)
                });
            }
            path.push(Command::Close);
            let Some((normal, origin)) = normalized(&path) else {
                continue;
            };
            for window in [
                [0, 0, 16, 16],
                [-24, -24, 48, 48],
                [next(30), next(30), 9, 7],
            ] {
                let [ox, oy, w, h] = window;
                let (lx, ly) = (ox - origin[0], oy - origin[1]);
                let (Some(xl), Some(xr), Some(yt), Some(yb)) = (
                    lx.checked_mul(256),
                    (lx + w).checked_mul(256),
                    ly.checked_mul(256),
                    (ly + h).checked_mul(256),
                ) else {
                    continue;
                };
                let window = Window {
                    lx,
                    ly,
                    width: w as usize,
                    height: h as usize,
                    xl,
                    xr,
                    yt,
                    yb,
                };
                for whole in [false, true] {
                    let mut narrow = vec![0u8; (w * h) as usize];
                    let mut wide = narrow.clone();
                    let limit = Curves::Reject(CURVE_LIMIT);
                    let n = nonzero::<i64>(&normal, limit, window, true, whole, &mut narrow);
                    let d = nonzero::<i128>(&normal, limit, window, true, whole, &mut wide);
                    let n = match n {
                        Ok(r) => Some(r),
                        Err(Integrate::Wide) => None,
                        Err(Integrate::Range) => panic!("i64 reports Wide, not Range"),
                    };
                    let d = d.unwrap_or(Err(RasterError::Range));
                    if let Some(n) = n {
                        assert_eq!(n, d, "case {case} {lx} {ly}");
                        if n.is_ok() {
                            assert_eq!(narrow, wide, "case {case} {lx} {ly}");
                        }
                        compared += 1;
                    }
                }
            }
        }
        assert!(compared > 1000, "{compared}");
    }

    #[test]
    fn raster_q8_paths_on_the_window_edges_are_exact() {
        // Paths whose extreme point lies exactly on a window edge, from outside and
        // from inside, closed and open, with lines and curves: the outside probe
        // and the touched-cell integration against the whole rows.
        let window = [2, 3, 4, 5]; // pixels: x 2..6, y 3..8 (Q8 512..1536, 768..2048)
        let (xl, xr, yt, yb) = (512i64, 1536i64, 768i64, 2048i64);
        let mut drawn = 0;
        for (x0, x1) in [
            (xl - 300, xl),
            (xl, xl + 300),
            (xr - 300, xr),
            (xr, xr + 300),
            (xl - 300, xr + 300),
        ] {
            for (y0, y1) in [
                (yt - 300, yt),
                (yt, yt + 300),
                (yb - 300, yb),
                (yb, yb + 300),
                (yt - 300, yb + 300),
            ] {
                let rect = vec![
                    Command::Move([x0, y0]),
                    Command::Line([x1, y0]),
                    Command::Line([x1, y1]),
                    Command::Line([x0, y1]),
                    Command::Close,
                ];
                let open = vec![
                    Command::Move([x0, y0]),
                    Command::Quad([x1, y0], [x1, y1]),
                    Command::Cubic([x0, y1], [x1, (y0 + y1) / 2], [x0, y0 + 1]),
                ];
                for path in [rect, open] {
                    let run = |whole| {
                        rasterize_with(
                            &path,
                            FillRule::NonZero,
                            window,
                            Curves::Reject(CURVE_LIMIT),
                            true,
                            whole,
                        )
                    };
                    let fast = run(false);
                    assert_eq!(fast, run(true), "{path:?}");
                    drawn += usize::from(fast.is_ok_and(|b| b.iter().any(|&v| v != 0)));
                }
            }
        }
        assert!(drawn > 10, "{drawn}");
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
