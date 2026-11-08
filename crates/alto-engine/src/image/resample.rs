//! Resampling filters used by resize, rotate and other geometry edits.
use super::*;

pub(super) fn resize_pixel(
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
pub(super) fn premultiplied_q14(p: [u8; 4]) -> [i128; 4] {
    let a = (u64::from(p[3]) * 16384 + 127) / 255;
    let mut out = [0i128; 4];
    out[3] = i128::from(a);
    for c in 0..3 {
        out[c] = i128::from((u64::from(p[c]) * u64::from(p[3]) * 16384 + 32512) / 65025);
    }
    out
}
/// Back to straight RGBA8. Colors stay inside alpha, because a filter can overshoot.
pub(super) fn straight_from_q14(p: [i128; 4]) -> [u8; 4] {
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
pub(super) fn catmull_rom_pixel(
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
pub(super) fn area_pixel(
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
