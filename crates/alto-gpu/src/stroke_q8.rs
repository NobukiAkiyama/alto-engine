//! Integer v3 stroke outlines. The centerline is flattened in local Q8, then
//! segment bodies, joins and caps are built in local Q16. Their polygons are
//! mapped through the fixed world matrix to screen Q8 and passed to the same
//! window-independent coverage rasterizer as vector fills.

use alto_core::{FixedAffine, q_from_f64_bits};
use alto_vector::{LineCap, LineJoin, PathCommand, PathData, Stroke};

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
    let bits = local_bits(world, level);
    let width =
        raster_q8::q_from_f32_bits(stroke.width.to_bits(), bits).ok_or(RasterError::Range)?;
    let radius = div(i128::from(width), 2).ok_or(RasterError::Range)?;
    if radius == 0 {
        return Ok(vec![0; len]);
    }
    let local = quantize_path(path, bits).ok_or(RasterError::Range)?;
    let mut outline = Vec::new();
    let threshold = threshold(world, level, radius, bits);
    let miter_limit =
        raster_q8::q_from_f32_bits(stroke.miter_limit.to_bits(), 16).ok_or(RasterError::Range)?;
    let miter_limit =
        div(i128::from(radius) * i128::from(miter_limit), Q16).ok_or(RasterError::Range)?;
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
                &mut outline,
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
            let last = *segments.last().ok_or(RasterError::Range)?;
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
            polygon(&mut outline, &start, world, level, bits)?;
            polygon(&mut outline, &end, world, level, bits)?;
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
                    polygon(&mut outline, &points, world, level, bits)?;
                }
                LineJoin::Bevel => polygon(&mut outline, &[vertex, p1, p2], world, level, bits)?,
                LineJoin::Miter => {
                    if let Some(tip) = miter(vertex, first, second, n1, n2, miter_limit) {
                        polygon(&mut outline, &[p1, tip, p2], world, level, bits)?;
                    } else {
                        polygon(&mut outline, &[vertex, p1, p2], world, level, bits)?;
                    }
                }
            }
        }
    }
    raster_q8::rasterize(&outline, FillRule::NonZero, window, raster_q8::CURVE_LIMIT)
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
        }
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
