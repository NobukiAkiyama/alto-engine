//! Integer v3 stroke outlines. The centerline is flattened in local Q8, then
//! segment bodies, joins and caps are built in local Q16. Their polygons are
//! mapped through the fixed world matrix to screen Q8 and passed to the same
//! window-independent coverage rasterizer as vector fills.

use alto_core::{FixedAffine, q_from_f64_bits};
use alto_vector::{Dash, LineCap, LineJoin, PathCommand, PathData, Stroke};

use crate::raster_q8::{self, Command, FillRule, RasterError};

type Point = [i64; 2];
const Q16: i128 = 1 << 16;
const MAX_OUTLINE_COMMANDS: usize = 1 << 20;

fn div(n: i128, d: i128) -> Option<i64> {
    if d == 0 {
        return None;
    }
    let (numerator, denominator) = (n.unsigned_abs(), d.unsigned_abs());
    let value = numerator
        .checked_add(denominator / 2)?
        .checked_div(denominator)?;
    let value = i128::try_from(value).ok()?;
    i64::try_from(if (n < 0) != (d < 0) { -value } else { value }).ok()
}

fn sqrt(n: u128) -> u128 {
    if n < 2 {
        return n;
    }
    let mut x = 1u128 << (128 - n.leading_zeros()).div_ceil(2);
    loop {
        let next = (x + n / x) >> 1;
        if next >= x {
            return x;
        }
        x = next;
    }
}

fn add(a: Point, b: Point) -> Option<Point> {
    Some([a[0].checked_add(b[0])?, a[1].checked_add(b[1])?])
}
fn sub(a: Point, b: Point) -> Option<Point> {
    Some([a[0].checked_sub(b[0])?, a[1].checked_sub(b[1])?])
}
fn neg(a: Point) -> Option<Point> {
    Some([a[0].checked_neg()?, a[1].checked_neg()?])
}
fn cross(a: Point, b: Point) -> i128 {
    i128::from(a[0]) * i128::from(b[1]) - i128::from(a[1]) * i128::from(b[0])
}
fn dot(a: Point, b: Point) -> i128 {
    i128::from(a[0]) * i128::from(b[0]) + i128::from(a[1]) * i128::from(b[1])
}
fn length(a: Point) -> Option<i64> {
    i64::try_from(sqrt(u128::try_from(dot(a, a)).ok()?)).ok()
}
fn scale_to(a: Point, radius: i64) -> Option<Point> {
    let len = i128::from(length(a)?);
    if len == 0 {
        return None;
    }
    Some([
        div(i128::from(a[0]) * i128::from(radius), len)?,
        div(i128::from(a[1]) * i128::from(radius), len)?,
    ])
}

fn map_point(p: Point, world: FixedAffine, level: i32, bits: u32) -> Option<Point> {
    let divisor = 1i128.checked_shl(u32::try_from(24 + bits as i32 + level).ok()?)?;
    let axis = |a: i64, b: i64, t: i64| {
        div(
            i128::from(a)
                .checked_mul(i128::from(p[0]))?
                .checked_add(i128::from(b).checked_mul(i128::from(p[1]))?)?
                .checked_add(i128::from(t) << (16 + bits))?,
            divisor,
        )
    };
    Some([
        axis(world.m[0], world.m[2], world.t[0])?,
        axis(world.m[1], world.m[3], world.t[1])?,
    ])
}

fn polygon(
    outline: &mut Vec<Command>,
    points: &[Point],
    world: FixedAffine,
    level: i32,
    bits: u32,
) -> Result<(), RasterError> {
    if points.len() < 3 || outline.len().saturating_add(points.len() + 2) > MAX_OUTLINE_COMMANDS {
        return if points.len() < 3 {
            Ok(())
        } else {
            Err(RasterError::CurveRange)
        };
    }
    let mut mapped = points
        .iter()
        .map(|&p| map_point(p, world, level, bits).ok_or(RasterError::Range))
        .collect::<Result<Vec<_>, _>>()?;
    mapped.dedup();
    if mapped.len() < 3 {
        return Ok(());
    }
    let mut area = 0i128;
    for i in 0..mapped.len() {
        area = area
            .checked_add(cross(mapped[i], mapped[(i + 1) % mapped.len()]))
            .ok_or(RasterError::Range)?;
    }
    if area == 0 {
        return Ok(());
    }
    // Every primitive must have the same winding for the nonzero fill to be
    // their union, including after a reflected world transform.
    if area < 0 {
        mapped.reverse();
    }
    outline.push(Command::Move(mapped[0]));
    outline.extend(mapped.into_iter().skip(1).map(Command::Line));
    outline.push(Command::Close);
    Ok(())
}

#[derive(Clone, Copy)]
struct Segment {
    a: Point,
    b: Point,
    tangent: Point,
    normal: Point,
}

fn arc(a: Point, b: Point, radius: i64, threshold: i128, depth: u32, out: &mut Vec<Point>) {
    let ab_cross = cross(a, b).unsigned_abs();
    let ab_dot = dot(a, b);
    if a == b
        || depth == 16
        || (ab_dot > 0 && ab_cross.saturating_mul(threshold as u128) <= ab_dot as u128)
    {
        out.push(b);
        return;
    }
    let Some(sum) = add(a, b) else {
        out.push(b);
        return;
    };
    let Some(mid) = scale_to(sum, radius) else {
        out.push(b);
        return;
    };
    if mid == a || mid == b {
        out.push(b);
        return;
    }
    arc(a, mid, radius, threshold, depth + 1, out);
    arc(mid, b, radius, threshold, depth + 1, out);
}

fn round_wedge(
    center: Point,
    a: Point,
    b: Point,
    via: Option<Point>,
    radius: i64,
    threshold: i128,
) -> Option<Vec<Point>> {
    let mut points = vec![center, add(center, a)?];
    let mut offsets = Vec::new();
    if let Some(via) = via {
        arc(a, via, radius, threshold, 0, &mut offsets);
        arc(via, b, radius, threshold, 0, &mut offsets);
    } else {
        arc(a, b, radius, threshold, 0, &mut offsets);
    }
    for offset in offsets {
        points.push(add(center, offset)?);
    }
    Some(points)
}

fn miter(
    vertex: Point,
    first: Segment,
    second: Segment,
    n1: Point,
    n2: Point,
    limit: i64,
) -> Option<Point> {
    let p1 = add(vertex, n1)?;
    let p2 = add(vertex, n2)?;
    let denominator = cross(first.tangent, second.tangent);
    if denominator == 0 {
        return None;
    }
    let numerator = cross(sub(p2, p1)?, second.tangent);
    let candidate = add(
        p1,
        [
            div(
                i128::from(first.tangent[0]).checked_mul(numerator)?,
                denominator,
            )?,
            div(
                i128::from(first.tangent[1]).checked_mul(numerator)?,
                denominator,
            )?,
        ],
    )?;
    let delta = sub(candidate, vertex)?;
    let distance = i128::from(delta[0])
        .checked_mul(i128::from(delta[0]))?
        .checked_add(i128::from(delta[1]).checked_mul(i128::from(delta[1]))?)?;
    (distance <= i128::from(limit) * i128::from(limit)).then_some(candidate)
}

fn subpaths(commands: &[Command]) -> Vec<(Vec<Command>, bool)> {
    let mut out = Vec::new();
    let mut current = Vec::new();
    let mut start = None;
    for &command in commands {
        match command {
            Command::Move(p) => {
                if !current.is_empty() {
                    out.push((std::mem::take(&mut current), false));
                }
                current.push(Command::Move(p));
                start = Some(p);
            }
            Command::Close => {
                current.push(Command::Close);
                out.push((std::mem::take(&mut current), true));
                if let Some(p) = start {
                    current.push(Command::Move(p));
                }
            }
            _ => current.push(command),
        }
    }
    if !current.is_empty() {
        out.push((current, false));
    }
    out
}

fn local_bits(world: FixedAffine, level: i32) -> u32 {
    let row = |a: i64, b: i64| u128::from(a.unsigned_abs()) + u128::from(b.unsigned_abs());
    let maximum = row(world.m[0], world.m[2]).max(row(world.m[1], world.m[3]));
    let extra = (0..=24)
        .find(|extra| maximum <= 1u128 << u32::try_from(32 + level + extra).unwrap())
        .unwrap_or(24);
    8 + u32::try_from(extra).unwrap()
}

fn quantize_path(path: &PathData, bits: u32) -> Option<Vec<Command>> {
    let point = |x: f64, y: f64| {
        Some([
            q_from_f64_bits(x.to_bits(), bits)?,
            q_from_f64_bits(y.to_bits(), bits)?,
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

fn threshold(world: FixedAffine, level: i32, radius: i64, bits: u32) -> i128 {
    let linear = world.m.iter().map(|v| v.unsigned_abs()).max().unwrap_or(0) as u128;
    let screen = ((radius.unsigned_abs() as u128)
        .saturating_mul(linear)
        .saturating_mul(2)
        >> u32::try_from(32 + bits as i32 + level).unwrap_or(48))
    .max(1);
    i128::try_from(sqrt(screen.saturating_mul(32)))
        .unwrap_or(1 << 16)
        .clamp(8, 1 << 16)
}

/// Coverage bytes for a validated stroke. The fill path is independent and is
/// painted first by the renderer. An over-complex outline is skipped identically
/// for every tile; other integer range failures refuse rendering.
pub(crate) fn rasterize_stroke(
    path: &PathData,
    stroke: &Stroke,
    world: FixedAffine,
    level: i32,
    window: [i64; 4],
) -> Result<Vec<u8>, RasterError> {
    let len = usize::try_from(window[2].checked_mul(window[3]).ok_or(RasterError::Range)?)
        .map_err(|_| RasterError::Range)?;
    match outline(path, stroke, world, level)? {
        None => Ok(vec![0; len]),
        Some(outline) => {
            raster_q8::rasterize(&outline, FillRule::NonZero, window, raster_q8::CURVE_LIMIT)
        }
    }
}

#[derive(Clone, Copy)]
struct Stride {
    radius: i64,
    world: FixedAffine,
    level: i32,
    bits: u32,
    threshold: i128,
    miter_limit: i64,
}

/// Body, caps and joins for one polyline of flattened segments. A dash is one
/// such polyline with `closed = false`, so both of its ends get caps.
fn stroke_polyline(
    outline: &mut Vec<Command>,
    segments: &[Segment],
    closed: bool,
    stroke: &Stroke,
    stride: Stride,
) -> Result<(), RasterError> {
    if segments.is_empty() {
        return Ok(());
    }
    let Stride {
        radius,
        world,
        level,
        bits,
        threshold,
        miter_limit,
    } = stride;
    for (index, segment) in segments.iter().copied().enumerate() {
        let mut a = segment.a;
        let mut b = segment.b;
        if !closed && stroke.cap == LineCap::Square {
            let direction = scale_to(segment.tangent, radius).ok_or(RasterError::Range)?;
            if index == 0 {
                a = sub(a, direction).ok_or(RasterError::Range)?;
            }
            if index + 1 == segments.len() {
                b = add(b, direction).ok_or(RasterError::Range)?;
            }
        }
        polygon(
            outline,
            &[
                add(a, segment.normal).ok_or(RasterError::Range)?,
                sub(a, segment.normal).ok_or(RasterError::Range)?,
                sub(b, segment.normal).ok_or(RasterError::Range)?,
                add(b, segment.normal).ok_or(RasterError::Range)?,
            ],
            world,
            level,
            bits,
        )?;
    }
    if !closed && stroke.cap == LineCap::Round {
        let first = segments[0];
        let last = segments[segments.len() - 1];
        let behind = neg(scale_to(first.tangent, radius).ok_or(RasterError::Range)?)
            .ok_or(RasterError::Range)?;
        let forward = scale_to(last.tangent, radius).ok_or(RasterError::Range)?;
        let start = round_wedge(
            first.a,
            neg(first.normal).ok_or(RasterError::Range)?,
            first.normal,
            Some(behind),
            radius,
            threshold,
        )
        .ok_or(RasterError::Range)?;
        let end = round_wedge(
            last.b,
            last.normal,
            neg(last.normal).ok_or(RasterError::Range)?,
            Some(forward),
            radius,
            threshold,
        )
        .ok_or(RasterError::Range)?;
        polygon(outline, &start, world, level, bits)?;
        polygon(outline, &end, world, level, bits)?;
    }
    let count = segments.len();
    let joins = if closed {
        count
    } else {
        count.saturating_sub(1)
    };
    for index in 0..joins {
        let first = segments[index];
        let second = segments[(index + 1) % count];
        let turn = cross(first.tangent, second.tangent);
        if turn == 0 {
            continue;
        }
        let vertex = first.b;
        let (n1, n2) = if turn > 0 {
            (
                neg(first.normal).ok_or(RasterError::Range)?,
                neg(second.normal).ok_or(RasterError::Range)?,
            )
        } else {
            (first.normal, second.normal)
        };
        let p1 = add(vertex, n1).ok_or(RasterError::Range)?;
        let p2 = add(vertex, n2).ok_or(RasterError::Range)?;
        match stroke.join {
            LineJoin::Round => {
                let points = round_wedge(vertex, n1, n2, None, radius, threshold)
                    .ok_or(RasterError::Range)?;
                polygon(outline, &points, world, level, bits)?;
            }
            LineJoin::Bevel => polygon(outline, &[vertex, p1, p2], world, level, bits)?,
            LineJoin::Miter => {
                if let Some(tip) = miter(vertex, first, second, n1, n2, miter_limit) {
                    polygon(outline, &[vertex, p1, tip, p2], world, level, bits)?;
                } else {
                    polygon(outline, &[vertex, p1, p2], world, level, bits)?;
                }
            }
        }
    }
    Ok(())
}

/// The point `position` along `segment`, whose flattened length is `length`.
/// The ends are used verbatim so pieces never accumulate rounding error.
fn at(segment: Segment, position: i64, length: i64) -> Option<Point> {
    if position <= 0 {
        return Some(segment.a);
    }
    if position >= length {
        return Some(segment.b);
    }
    let delta = sub(segment.b, segment.a)?;
    let step = |value: i64| div(i128::from(value) * i128::from(position), i128::from(length));
    add(segment.a, [step(delta[0])?, step(delta[1])?])
}

/// The pattern lengths in local Q with an even number of elements (an odd
/// pattern is repeated twice) and the phase where the path starts.
/// `None` when quantization leaves no period: the stroke then stays solid.
fn dash_pattern(dash: &Dash, bits: u32) -> Result<Option<(Vec<i64>, i64)>, RasterError> {
    let mut lengths = Vec::with_capacity(dash.pattern.len() * 2);
    let mut period = 0i64;
    for &value in &dash.pattern {
        if !value.is_finite() || value < 0.0 {
            return Err(RasterError::Range);
        }
        let q = raster_q8::q_from_f32_bits(value.to_bits(), bits).ok_or(RasterError::Range)?;
        period = period.checked_add(q).ok_or(RasterError::Range)?;
        lengths.push(q);
    }
    if lengths.len() % 2 == 1 {
        for q in lengths.clone() {
            period = period.checked_add(q).ok_or(RasterError::Range)?;
            lengths.push(q);
        }
    }
    if period <= 0 {
        return Ok(None);
    }
    let offset =
        raster_q8::q_from_f32_bits(dash.offset.to_bits(), bits).ok_or(RasterError::Range)?;
    Ok(Some((lengths, offset.rem_euclid(period))))
}

fn push_dash(dashes: &mut Vec<Vec<Segment>>, dash: Vec<Segment>) -> Result<(), RasterError> {
    if dashes.len() >= alto_vector::MAX_DASHES_PER_PATH {
        return Err(RasterError::CurveRange);
    }
    dashes.push(dash);
    Ok(())
}

/// Split the flattened centerline into dashes. `None` means the solid stroke is
/// drawn instead: either the pattern vanished in quantization, or a closed
/// polyline is covered by one mark and keeps its closed join. Pieces cut from
/// one segment inherit its tangent and normal, so their joints leave no seam.
fn dash_polylines(
    segments: &[Segment],
    closed: bool,
    dash: &Dash,
    bits: u32,
) -> Result<Option<Vec<Vec<Segment>>>, RasterError> {
    let Some((lengths, phase)) = dash_pattern(dash, bits)? else {
        return Ok(None);
    };
    let count = lengths.len();
    let mut index = 0usize;
    let mut offset = phase;
    let mut dots = 0usize;
    while offset >= lengths[index] {
        if offset == 0 && lengths[index] == 0 && index.is_multiple_of(2) {
            dots += 1;
        }
        offset -= lengths[index];
        index += 1;
        if index == count {
            return Ok(None);
        }
    }
    let mut remaining = lengths[index] - offset;
    let mut on = index.is_multiple_of(2);
    let mut dashes: Vec<Vec<Segment>> = Vec::new();
    let mut current: Vec<Segment> = Vec::new();
    for _ in 0..dots {
        let first = segments[0];
        let a = first.a;
        push_dash(
            &mut dashes,
            vec![Segment {
                a,
                b: a,
                tangent: first.tangent,
                normal: first.normal,
            }],
        )?;
    }
    if closed && on && dots == 0 {
        let total = segments.iter().try_fold(0i64, |total, segment| {
            total.checked_add(length(segment.tangent)?)
        });
        if total.is_some_and(|total| total <= remaining) {
            return Ok(None);
        }
    }
    for segment in segments.iter().copied() {
        let len = length(segment.tangent).ok_or(RasterError::Range)?;
        let mut position = 0i64;
        while position < len {
            if remaining == 0 {
                loop {
                    index = (index + 1) % count;
                    on = index.is_multiple_of(2);
                    let value = lengths[index];
                    if value > 0 {
                        remaining = value;
                        break;
                    }
                    if on {
                        let a = at(segment, position, len).ok_or(RasterError::Range)?;
                        push_dash(
                            &mut dashes,
                            vec![Segment {
                                a,
                                b: a,
                                tangent: segment.tangent,
                                normal: segment.normal,
                            }],
                        )?;
                    }
                }
            }
            let take = remaining.min(len - position);
            if on {
                let a = at(segment, position, len).ok_or(RasterError::Range)?;
                let b = at(segment, position + take, len).ok_or(RasterError::Range)?;
                current.push(Segment {
                    a,
                    b,
                    tangent: segment.tangent,
                    normal: segment.normal,
                });
            }
            position += take;
            remaining -= take;
            if remaining == 0 && on && !current.is_empty() {
                push_dash(&mut dashes, std::mem::take(&mut current))?;
            }
        }
    }
    if !current.is_empty() {
        push_dash(&mut dashes, current)?;
    }
    if remaining == 0 && !closed {
        // A zero-length mark exactly at the open end still gets its caps.
        let last = segments[segments.len() - 1];
        loop {
            index = (index + 1) % count;
            if lengths[index] > 0 {
                break;
            }
            if index.is_multiple_of(2) {
                push_dash(
                    &mut dashes,
                    vec![Segment {
                        a: last.b,
                        b: last.b,
                        tangent: last.tangent,
                        normal: last.normal,
                    }],
                )?;
            }
        }
    }
    Ok(Some(dashes))
}

fn stroke_bits(stroke: &Stroke, world: FixedAffine, level: i32) -> u32 {
    let bits = local_bits(world, level);
    if stroke.dash.as_ref().is_some_and(|dash| {
        dash.pattern
            .iter()
            .all(|value| raster_q8::q_from_f32_bits(value.to_bits(), bits) == Some(0))
    }) {
        // Q10 retains at least one element even for the minimum legal period
        // split across an odd pattern of fifteen elements repeated twice.
        bits.max(10)
    } else {
        bits
    }
}

/// The stroke's outline polygons in Q8 level pixels; `None` for a zero radius.
/// It depends on the path, stroke, world and level only, never on the window.
pub(crate) fn outline(
    path: &PathData,
    stroke: &Stroke,
    world: FixedAffine,
    level: i32,
) -> Result<Option<Vec<raster_q8::Command>>, RasterError> {
    let bits = stroke_bits(stroke, world, level);
    let width =
        raster_q8::q_from_f32_bits(stroke.width.to_bits(), bits).ok_or(RasterError::Range)?;
    let radius = div(i128::from(width), 2).ok_or(RasterError::Range)?;
    if radius == 0 {
        return Ok(None);
    }
    let local = quantize_path(path, bits).ok_or(RasterError::Range)?;
    let mut outline = Vec::new();
    let threshold = threshold(world, level, radius, bits);
    let miter_limit =
        raster_q8::q_from_f32_bits(stroke.miter_limit.to_bits(), 16).ok_or(RasterError::Range)?;
    let miter_limit =
        div(i128::from(radius) * i128::from(miter_limit), Q16).ok_or(RasterError::Range)?;
    let mut dash_count = 0;
    for (commands, closed) in subpaths(&local) {
        let flattened = raster_q8::flatten_stroke(&commands)?;
        if flattened.is_empty() {
            continue;
        }
        let mut segments = Vec::with_capacity(flattened.len());
        for (a, b) in flattened {
            let tangent = sub(b, a).ok_or(RasterError::Range)?;
            let normal = scale_to([-tangent[1], tangent[0]], radius).ok_or(RasterError::Range)?;
            segments.push(Segment {
                a,
                b,
                tangent,
                normal,
            });
        }
        let stride = Stride {
            radius,
            world,
            level,
            bits,
            threshold,
            miter_limit,
        };
        let Some(dash) = &stroke.dash else {
            stroke_polyline(&mut outline, &segments, closed, stroke, stride)?;
            continue;
        };
        let dashes = dash_polylines(&segments, closed, dash, bits)?;
        dash_count += dashes.as_ref().map_or(1, Vec::len);
        if dash_count > alto_vector::MAX_DASHES_PER_PATH {
            return Err(RasterError::CurveRange);
        }
        if let Some(dashes) = dashes {
            for dash in dashes {
                stroke_polyline(&mut outline, &dash, false, stroke, stride)?;
            }
        } else {
            stroke_polyline(&mut outline, &segments, closed, stroke, stride)?;
        }
    }
    Ok(Some(outline))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke() -> Stroke {
        Stroke {
            color: [1.0; 4],
            width: 10.0,
            cap: LineCap::Round,
            join: LineJoin::Round,
            miter_limit: 4.0,
            dash: None,
        }
    }

    fn outline_hash(path: &PathData, stroke: &Stroke, world: FixedAffine, level: i32) -> u64 {
        let outline = outline(path, stroke, world, level).unwrap().unwrap();
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for command in outline {
            for byte in format!("{command:?}").bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    fn line(x0: f64, y0: f64, x1: f64, y1: f64) -> PathData {
        PathData {
            commands: vec![
                PathCommand::Move { x: x0, y: y0 },
                PathCommand::Line { x: x1, y: y1 },
            ],
        }
    }

    fn closed_square(size: f64) -> PathData {
        PathData {
            commands: vec![
                PathCommand::Move { x: 0.0, y: 0.0 },
                PathCommand::Line { x: size, y: 0.0 },
                PathCommand::Line { x: size, y: size },
                PathCommand::Line { x: 0.0, y: size },
                PathCommand::Close,
            ],
        }
    }

    /// The local segments, closed flag and `bits` that `outline` would build.
    fn centerline(path: &PathData, stroke: &Stroke, level: i32) -> (Vec<Segment>, bool, u32) {
        let bits = stroke_bits(stroke, FixedAffine::IDENTITY, level);
        let width = raster_q8::q_from_f32_bits(stroke.width.to_bits(), bits).unwrap();
        let radius = div(i128::from(width), 2).unwrap();
        let local = quantize_path(path, bits).unwrap();
        let mut segments = Vec::new();
        let mut closed = false;
        for (commands, is_closed) in subpaths(&local) {
            let flattened = raster_q8::flatten_stroke(&commands).unwrap();
            if flattened.is_empty() {
                continue;
            }
            closed = is_closed;
            for (a, b) in flattened {
                let tangent = sub(b, a).unwrap();
                let normal = scale_to([-tangent[1], tangent[0]], radius).unwrap();
                segments.push(Segment {
                    a,
                    b,
                    tangent,
                    normal,
                });
            }
        }
        (segments, closed, bits)
    }

    fn dash_runs(path: &PathData, stroke: &Stroke) -> Vec<Vec<Segment>> {
        let (segments, closed, bits) = centerline(path, stroke, 0);
        dash_polylines(&segments, closed, stroke.dash.as_ref().unwrap(), bits)
            .unwrap()
            .expect("a dash split")
    }

    fn ranges(dashes: &[Vec<Segment>]) -> Vec<(i64, i64)> {
        dashes
            .iter()
            .map(|dash| (dash[0].a[0], dash[dash.len() - 1].b[0]))
            .collect()
    }

    // Pinned before `stroke_polyline` was extracted: a dash-free outline must
    // keep every command of every existing stroke case.
    #[test]
    fn no_dash_outline_is_byte_identical() {
        let line = PathData {
            commands: vec![
                PathCommand::Move { x: 10.5, y: 20.25 },
                PathCommand::Line { x: 90.0, y: 40.5 },
            ],
        };
        let curve = PathData {
            commands: vec![
                PathCommand::Move { x: 0.5, y: 2.0 },
                PathCommand::Quad {
                    cx: 2.0,
                    cy: 0.5,
                    x: 3.5,
                    y: 2.0,
                },
            ],
        };
        let closed = PathData {
            commands: vec![
                PathCommand::Move { x: 10.0, y: 10.0 },
                PathCommand::Line { x: 60.0, y: 10.0 },
                PathCommand::Line { x: 30.0, y: 50.0 },
                PathCommand::Close,
            ],
        };
        assert_eq!(
            outline_hash(&line, &stroke(), FixedAffine::IDENTITY, 0),
            3_869_907_168_425_145_889
        );
        let mut square = stroke();
        square.cap = LineCap::Square;
        assert_eq!(
            outline_hash(&line, &square, FixedAffine::IDENTITY, 0),
            16_665_492_800_968_731_103
        );
        let mut bevel = stroke();
        bevel.join = LineJoin::Bevel;
        assert_eq!(
            outline_hash(&closed, &bevel, FixedAffine::IDENTITY, 0),
            8_801_906_577_633_656_014
        );
        let world = FixedAffine {
            m: [1 << 30, 1 << 28, -(1 << 28), 1 << 30],
            t: [5 << 16, -(7 << 16)],
        };
        assert_eq!(
            outline_hash(&curve, &stroke(), world, 1),
            4_898_033_175_938_251_625
        );
    }

    #[test]
    fn dash_splits_a_line_at_the_pattern() {
        let path = line(0.0, 16.0, 50.0, 16.0);
        let mut style = stroke();
        style.cap = LineCap::Butt;
        style.dash = Some(Dash {
            pattern: vec![10.0, 5.0],
            offset: 0.0,
        });
        let dashes = dash_runs(&path, &style);
        assert!(dashes.iter().all(|dash| dash.len() == 1));
        assert_eq!(
            ranges(&dashes),
            vec![(0, 2560), (3840, 6400), (7680, 10240), (11520, 12800)]
        );
    }

    #[test]
    fn dash_offset_shifts_the_phase() {
        let path = line(0.0, 16.0, 40.0, 16.0);
        let mut style = stroke();
        style.cap = LineCap::Butt;
        style.dash = Some(Dash {
            pattern: vec![10.0, 5.0],
            offset: 3.0,
        });
        assert_eq!(ranges(&dash_runs(&path, &style))[0], (0, 1792));
        style.dash.as_mut().unwrap().offset = -3.0;
        assert_eq!(ranges(&dash_runs(&path, &style))[0], (768, 3328));
    }

    #[test]
    fn odd_pattern_repeats_twice() {
        let path = line(0.0, 16.0, 60.0, 16.0);
        let mut style = stroke();
        style.cap = LineCap::Butt;
        style.dash = Some(Dash {
            pattern: vec![10.0],
            offset: 0.0,
        });
        assert_eq!(
            ranges(&dash_runs(&path, &style)),
            vec![(0, 2560), (5120, 7680), (10240, 12800)]
        );
    }

    #[test]
    fn dash_runs_across_segment_boundaries_as_one_dash() {
        let path = PathData {
            commands: vec![
                PathCommand::Move { x: 0.0, y: 0.0 },
                PathCommand::Line { x: 20.0, y: 0.0 },
                PathCommand::Line { x: 20.0, y: 20.0 },
            ],
        };
        let mut style = stroke();
        style.cap = LineCap::Butt;
        style.dash = Some(Dash {
            pattern: vec![30.0, 10.0],
            offset: 0.0,
        });
        let dashes = dash_runs(&path, &style);
        assert_eq!(dashes.len(), 1);
        assert_eq!(dashes[0].len(), 2);
        assert_eq!(dashes[0][0].a, [0, 0]);
        assert_eq!(dashes[0][0].b, [5120, 0]);
        assert_eq!(dashes[0][1].a, [5120, 0]);
        assert_eq!(dashes[0][1].b, [5120, 2560]);
    }

    #[test]
    fn dash_pieces_from_one_segment_leave_no_seam() {
        let path = PathData {
            commands: vec![
                PathCommand::Move { x: 0.0, y: 0.0 },
                PathCommand::Line { x: 20.0, y: 0.0 },
                PathCommand::Line { x: 20.0, y: 20.0 },
            ],
        };
        let mut style = stroke();
        style.cap = LineCap::Butt;
        style.join = LineJoin::Miter;
        style.width = 8.0;
        style.dash = Some(Dash {
            pattern: vec![30.0, 10.0],
            offset: 0.0,
        });
        let coverage =
            rasterize_stroke(&path, &style, FixedAffine::IDENTITY, 0, [0, 0, 48, 48]).unwrap();
        assert_eq!(coverage[20], 255);
        assert_eq!(coverage[5 * 48 + 20], 255);
        assert_eq!(coverage[5 * 48 + 10], 0);
    }

    #[test]
    fn miter_covers_bevel_and_tip_and_respects_limit() {
        let path = PathData {
            commands: vec![
                PathCommand::Move { x: 8.0, y: 20.0 },
                PathCommand::Line { x: 20.0, y: 20.0 },
                PathCommand::Line { x: 20.0, y: 32.0 },
            ],
        };
        let mut style = stroke();
        style.cap = LineCap::Butt;
        style.join = LineJoin::Miter;
        style.width = 8.0;
        let render = |s: &Stroke| {
            rasterize_stroke(&path, s, FixedAffine::IDENTITY, 0, [0, 0, 48, 48]).unwrap()
        };
        let miter = render(&style);
        assert_eq!(miter[18 * 48 + 21], 255);
        assert_eq!(miter[16 * 48 + 23], 255);
        style.miter_limit = 1.0;
        let bevel = render(&style);
        assert_eq!(bevel[16 * 48 + 23], 0);
        style.join = LineJoin::Bevel;
        assert_eq!(bevel, render(&style));
    }

    #[test]
    fn zero_length_dash_makes_a_dot_with_round_and_square_caps_only() {
        let path = line(10.0, 32.0, 50.0, 32.0);
        let mut style = stroke();
        style.width = 4.0;
        style.dash = Some(Dash {
            pattern: vec![0.0, 8.0],
            offset: 0.0,
        });
        assert_eq!(dash_runs(&path, &style).len(), 6);
        style.cap = LineCap::Butt;
        let coverage =
            rasterize_stroke(&path, &style, FixedAffine::IDENTITY, 0, [0, 0, 64, 64]).unwrap();
        assert!(coverage.iter().all(|&value| value == 0));
        style.cap = LineCap::Round;
        let coverage =
            rasterize_stroke(&path, &style, FixedAffine::IDENTITY, 0, [0, 0, 64, 64]).unwrap();
        assert_eq!(coverage[32 * 64 + 10], 255);
        assert!(coverage[31 * 64 + 8] < 255);
        assert_eq!(coverage[32 * 64 + 13], 0);
        style.cap = LineCap::Square;
        let coverage =
            rasterize_stroke(&path, &style, FixedAffine::IDENTITY, 0, [0, 0, 64, 64]).unwrap();
        assert_eq!(coverage[31 * 64 + 8], 255);
        assert_eq!(coverage[32 * 64 + 13], 0);
    }

    #[test]
    fn closed_path_dashes_over_the_close_segment() {
        let path = closed_square(40.0);
        let mut style = stroke();
        style.cap = LineCap::Butt;
        style.dash = Some(Dash {
            pattern: vec![30.0, 10.0],
            offset: 0.0,
        });
        let dashes = dash_runs(&path, &style);
        assert_eq!(dashes.len(), 4);
        let last = &dashes[dashes.len() - 1];
        // The last mark lies on the `Close` edge from (0, 40) to (0, 0).
        assert_eq!(last[0].a, [0, 10240]);
        assert_eq!(last[last.len() - 1].b, [0, 2560]);
    }

    #[test]
    fn closed_path_covered_by_one_dash_stays_closed() {
        let path = closed_square(10.0);
        let mut style = stroke();
        style.dash = Some(Dash {
            pattern: vec![100.0, 10.0],
            offset: 0.0,
        });
        let (segments, closed, bits) = centerline(&path, &style, 0);
        assert!(closed);
        assert!(
            dash_polylines(&segments, closed, style.dash.as_ref().unwrap(), bits)
                .unwrap()
                .is_none()
        );
        style.dash = None;
        let solid = format!(
            "{:?}",
            outline(&path, &style, FixedAffine::IDENTITY, 0)
                .unwrap()
                .unwrap()
        );
        style.dash = Some(Dash {
            pattern: vec![100.0, 10.0],
            offset: 0.0,
        });
        let dashed = format!(
            "{:?}",
            outline(&path, &style, FixedAffine::IDENTITY, 0)
                .unwrap()
                .unwrap()
        );
        assert_eq!(solid, dashed);
    }

    #[test]
    fn dash_on_a_curve_follows_the_flattened_length() {
        let path = PathData {
            commands: vec![
                PathCommand::Move { x: 0.0, y: 0.0 },
                PathCommand::Quad {
                    cx: 0.0,
                    cy: 100.0,
                    x: 100.0,
                    y: 100.0,
                },
            ],
        };
        let mut style = stroke();
        style.cap = LineCap::Butt;
        style.dash = Some(Dash {
            pattern: vec![10.0, 10.0],
            offset: 0.0,
        });
        let (segments, _, _) = centerline(&path, &style, 0);
        let total: i64 = segments
            .iter()
            .map(|segment| length(segment.tangent).unwrap())
            .sum();
        let dashes = dash_runs(&path, &style);
        assert!(total > 30_000);
        assert_eq!(dashes.len() as i64, (total + 5119) / 5120);
        let on: i64 = dashes
            .iter()
            .flat_map(|dash| dash.iter())
            .map(|fragment| length(sub(fragment.b, fragment.a).unwrap()).unwrap())
            .sum();
        let expected: i64 = (0i64..)
            .map(|k| k * 5120)
            .take_while(|&start| start < total)
            .map(|start| (start + 2560).min(total) - start)
            .sum();
        assert!((on - expected).abs() <= 4 * dashes.len() as i64);
    }

    #[test]
    fn dash_count_over_the_limit_skips_the_stroke_on_every_tile() {
        let path = line(0.0, 16.0, 1000.0, 16.0);
        let mut style = stroke();
        style.cap = LineCap::Butt;
        style.dash = Some(Dash {
            pattern: vec![0.1, 0.1],
            offset: 0.0,
        });
        let wide = rasterize_stroke(&path, &style, FixedAffine::IDENTITY, 0, [0, 0, 64, 32]);
        let narrow = rasterize_stroke(&path, &style, FixedAffine::IDENTITY, 0, [32, 8, 8, 8]);
        assert!(matches!(wide, Err(RasterError::CurveRange)));
        assert!(matches!(narrow, Err(RasterError::CurveRange)));

        // The budget is per path even though each Move restarts the pattern.
        let mut multiple = line(0.0, 16.0, 500.0, 16.0);
        multiple
            .commands
            .extend(line(0.0, 24.0, 500.0, 24.0).commands);
        assert!(matches!(
            rasterize_stroke(&multiple, &style, FixedAffine::IDENTITY, 0, [0, 0, 64, 32]),
            Err(RasterError::CurveRange),
        ));
    }

    #[test]
    fn dash_is_independent_of_the_window() {
        let path = line(5.0, 20.0, 91.0, 20.0);
        let mut style = stroke();
        style.cap = LineCap::Butt;
        style.dash = Some(Dash {
            pattern: vec![7.0, 4.0],
            offset: 0.0,
        });
        let wide =
            rasterize_stroke(&path, &style, FixedAffine::IDENTITY, 0, [0, 0, 96, 40]).unwrap();
        let left =
            rasterize_stroke(&path, &style, FixedAffine::IDENTITY, 0, [0, 0, 48, 40]).unwrap();
        let right =
            rasterize_stroke(&path, &style, FixedAffine::IDENTITY, 0, [48, 0, 48, 40]).unwrap();
        for y in 0..40 {
            assert_eq!(&wide[y * 96..][..48], &left[y * 48..][..48]);
            assert_eq!(&wide[y * 96 + 48..][..48], &right[y * 48..][..48]);
        }
    }

    #[test]
    fn minimum_legal_dash_period_does_not_disappear_at_low_precision() {
        let path = line(0.0, 16.0, 1.0, 16.0);
        let mut style = stroke();
        let dash = Dash {
            pattern: vec![1.0 / 1920.0; 15],
            offset: 0.0,
        };
        dash.validate().unwrap();
        style.dash = Some(dash);
        let (segments, closed, bits) = centerline(&path, &style, 0);
        assert_eq!(local_bits(FixedAffine::IDENTITY, 0), 8);
        assert_eq!(bits, 10);
        assert!(
            !dash_polylines(&segments, closed, style.dash.as_ref().unwrap(), bits)
                .unwrap()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn seam_coverage_is_window_independent() {
        let path = PathData {
            commands: vec![
                PathCommand::Move { x: 250.0, y: 100.0 },
                PathCommand::Line { x: 270.0, y: 100.0 },
            ],
        };
        let wide =
            rasterize_stroke(&path, &stroke(), FixedAffine::IDENTITY, 0, [0, 0, 512, 256]).unwrap();
        let left =
            rasterize_stroke(&path, &stroke(), FixedAffine::IDENTITY, 0, [0, 0, 256, 256]).unwrap();
        let right = rasterize_stroke(
            &path,
            &stroke(),
            FixedAffine::IDENTITY,
            0,
            [256, 0, 256, 256],
        )
        .unwrap();
        for y in 0..256 {
            assert_eq!(&wide[y * 512..][..256], &left[y * 256..][..256]);
            assert_eq!(&wide[y * 512 + 256..][..256], &right[y * 256..][..256]);
        }
        assert_eq!(left[100 * 256 + 255], 255);
        assert_eq!(right[100 * 256], 255);
    }

    #[test]
    fn curve_precision_tracks_the_zoom_level() {
        let path = PathData {
            commands: vec![
                PathCommand::Move { x: 0.5, y: 2.0 },
                PathCommand::Quad {
                    cx: 2.0,
                    cy: 0.5,
                    x: 3.5,
                    y: 2.0,
                },
            ],
        };
        let mut style = stroke();
        style.width = 0.1;
        let coverage =
            rasterize_stroke(&path, &style, FixedAffine::IDENTITY, -6, [0, 0, 256, 256]).unwrap();
        assert!(coverage.contains(&255));
        assert_eq!(coverage[0], 0);
        assert_eq!(local_bits(FixedAffine::IDENTITY, -6), 14);
    }
}
