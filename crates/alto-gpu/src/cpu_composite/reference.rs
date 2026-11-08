//! The CPU composite walk: fetching texels, popping groups and quantizing like the shader.
use super::*;

pub(super) fn walk(node: &Node, parent: [f64; 6], ops: &mut Vec<Op>) {
    let (visible, opacity, transform) = match node {
        Node::Raster {
            visible,
            opacity,
            transform,
            ..
        }
        | Node::Group {
            visible,
            opacity,
            transform,
            ..
        } => (*visible, *opacity, *transform),
    };
    if !visible || opacity <= 0.0 {
        return;
    }
    let world = Affine2(parent).concat(Affine2(transform)).0;
    match node {
        Node::Group {
            isolated,
            mode,
            children,
            ..
        } => {
            let isolate = *isolated || opacity < 1.0 || *mode != 0;
            if isolate {
                ops.push(Op::Push);
            }
            for child in children {
                walk(child, world, ops);
            }
            if isolate {
                ops.push(Op::Pop {
                    opacity,
                    mode: *mode,
                });
            }
        }
        Node::Raster {
            image,
            mode,
            extra_tiles,
            ..
        } => {
            let Some(inv) = Affine2(world).inverse() else {
                return;
            };
            let mut tiles = split_tiles(image);
            tiles.extend(extra_tiles.iter().cloned());
            tiles.sort_by_key(|t| (t.tx, t.ty));
            ops.push(Op::Draw {
                inv: inv.0,
                q: None,
                opacity,
                mode: *mode,
                tiles,
            });
        }
    }
}

pub(super) fn walk_v3(node: &Node, parent: alto_core::FixedAffine, ops: &mut Vec<Op>) {
    let (visible, opacity, transform) = match node {
        Node::Raster {
            visible,
            opacity,
            transform,
            ..
        }
        | Node::Group {
            visible,
            opacity,
            transform,
            ..
        } => (*visible, *opacity, *transform),
    };
    if !visible || opacity <= 0.0 {
        return;
    }
    let local =
        alto_core::FixedAffine::from_affine(Affine2(transform)).expect("v3 transform out of range");
    let world = parent
        .concat(local)
        .expect("v3 composed transform out of range");
    match node {
        Node::Group {
            isolated,
            mode,
            children,
            ..
        } => {
            let isolate = *isolated || opacity < 1.0 || *mode != 0;
            if isolate {
                ops.push(Op::Push);
            }
            for child in children {
                walk_v3(child, world, ops);
            }
            if isolate {
                ops.push(Op::Pop {
                    opacity,
                    mode: *mode,
                });
            }
        }
        Node::Raster {
            image,
            mode,
            extra_tiles,
            ..
        } => {
            let Some(q) =
                crate::sample::fixed_inverse(&world).expect("v3 transform has no Q16 inverse")
            else {
                return;
            };
            let mut tiles = split_tiles(image);
            tiles.extend(extra_tiles.iter().cloned());
            tiles.sort_by_key(|t| (t.tx, t.ty));
            ops.push(Op::Draw {
                inv: [0.0; 6],
                q: Some(q),
                opacity,
                mode: *mode,
                tiles,
            });
        }
    }
}

pub(super) fn lookup(tiles: &[Tile], key: (i32, i32)) -> Option<&[u8]> {
    let mut lo = 0usize;
    let mut hi = tiles.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        let tile = &tiles[mid];
        if tile.tx < key.0 || (tile.tx == key.0 && tile.ty < key.1) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let tile = tiles.get(lo)?;
    if tile.tx == key.0 && tile.ty == key.1 {
        Some(&tile.pixels)
    } else {
        None
    }
}

pub(super) fn texel(tiles: &[Tile], q: [f32; 2]) -> [f32; 4] {
    let key = ((q[0] / 256.0).floor() as i32, (q[1] / 256.0).floor() as i32);
    let Some(pixels) = lookup(tiles, key) else {
        return [0.0; 4];
    };
    let local_x = (q[0].floor() as i32 - key.0 * 256).clamp(0, 255);
    let local_y = (q[1].floor() as i32 - key.1 * 256).clamp(0, 255);
    let i = ((local_y as usize * 256) + local_x as usize) * 4;
    [
        f32::from(pixels[i]) / 255.0,
        f32::from(pixels[i + 1]) / 255.0,
        f32::from(pixels[i + 2]) / 255.0,
        f32::from(pixels[i + 3]) / 255.0,
    ]
}

pub(super) fn fetch_f32(tiles: &[Tile], inv: [f32; 6], p: [f32; 2]) -> [f32; 4] {
    let q = [
        inv[0] * p[0] + inv[2] * p[1] + inv[4],
        inv[1] * p[0] + inv[3] * p[1] + inv[5],
    ];
    if axis_unit(inv) {
        return texel(tiles, q);
    }
    let base = [q[0].floor(), q[1].floor()];
    let f = [q[0] - base[0], q[1] - base[1]];
    let a = texel(tiles, base);
    let b = texel(tiles, [base[0] + 1.0, base[1]]);
    let d = texel(tiles, [base[0], base[1] + 1.0]);
    let e = texel(tiles, [base[0] + 1.0, base[1] + 1.0]);
    let mut out = [0.0; 4];
    for i in 0..4 {
        let ab = a[i] * (1.0 - f[0]) + b[i] * f[0];
        let de = d[i] * (1.0 - f[0]) + e[i] * f[0];
        out[i] = ab * (1.0 - f[1]) + de * f[1];
    }
    out
}

pub(super) fn sample_u8(tiles: &[Tile], x: i32, y: i32) -> [u8; 4] {
    let key = (x >> 8, y >> 8);
    let Some(pixels) = lookup(tiles, key) else {
        return [0; 4];
    };
    let local_x = (x - key.0 * 256).clamp(0, 255) as usize;
    let local_y = (y - key.1 * 256).clamp(0, 255) as usize;
    let i = (local_y * 256 + local_x) * 4;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

pub(super) fn fetch_integer(
    tiles: &[Tile],
    inv: &IntegerInverse,
    origin_px: [i32; 2],
    scale: i32,
    x: i32,
    y: i32,
) -> [f32; 4] {
    let Some(px) = pixel_q8(origin_px[0], x, scale) else {
        return [0.0; 4];
    };
    let Some(py) = pixel_q8(origin_px[1], y, scale) else {
        return [0.0; 4];
    };
    let Some(q) = source_q8(inv, px, py) else {
        return [0.0; 4];
    };
    let (ix, fx) = floor_frac_q8(q[0]);
    let (iy, fy) = floor_frac_q8(q[1]);
    let a = sample_u8(tiles, ix, iy);
    let b = sample_u8(tiles, ix + 1, iy);
    let d = sample_u8(tiles, ix, iy + 1);
    let e = sample_u8(tiles, ix + 1, iy + 1);
    let mut out = [0.0; 4];
    for channel in 0..4 {
        let byte = bilinear_byte(a[channel], b[channel], d[channel], e[channel], fx, fy);
        out[channel] = f32::from(byte) / 255.0;
    }
    out
}

pub(super) fn fetch_integer_bytes(
    tiles: &[Tile],
    inv: &IntegerInverse,
    origin_px: [i32; 2],
    scale: i32,
    x: i32,
    y: i32,
) -> [u8; 4] {
    let Some(px) = pixel_q8(origin_px[0], x, scale) else {
        return [0; 4];
    };
    let Some(py) = pixel_q8(origin_px[1], y, scale) else {
        return [0; 4];
    };
    let Some(q) = source_q8(inv, px, py) else {
        return [0; 4];
    };
    let (ix, fx) = floor_frac_q8(q[0]);
    let (iy, fy) = floor_frac_q8(q[1]);
    let a = sample_u8(tiles, ix, iy);
    let b = sample_u8(tiles, ix + 1, iy);
    let d = sample_u8(tiles, ix, iy + 1);
    let e = sample_u8(tiles, ix + 1, iy + 1);
    let mut out = [0; 4];
    for channel in 0..4 {
        let byte = bilinear_byte(a[channel], b[channel], d[channel], e[channel], fx, fy);
        out[channel] = byte;
    }
    out
}

/// Tile placement and which sampling path the document's render version selects.
pub(super) struct Frame {
    pub(super) origin: [f32; 2],
    pub(super) scale: f32,
    pub(super) origin_px: [i32; 2],
    pub(super) scale_i: i32,
    pub(super) version: u32,
}

pub(super) fn fetch(
    tiles: &[Tile],
    inv: [f64; 6],
    p: [f32; 2],
    x: i32,
    y: i32,
    frame: &Frame,
) -> [f32; 4] {
    let inv_f32 = inv.map(|v| v as f32);
    if frame.version < RENDER_VERSION || axis_unit(inv_f32) {
        return fetch_f32(tiles, inv_f32, p);
    }
    match crate::sample::integer_inverse_v2(inv) {
        Some(q) => fetch_integer(tiles, &q, frame.origin_px, frame.scale_i, x, y),
        None => fetch_f32(tiles, inv_f32, p),
    }
}

pub(super) fn pop_src(accum: [f32; 4], opacity: f32) -> [f32; 4] {
    let rgb = if accum[3] > 0.0 {
        [
            accum[0] / accum[3].max(0.000001),
            accum[1] / accum[3].max(0.000001),
            accum[2] / accum[3].max(0.000001),
        ]
    } else {
        [0.0; 3]
    };
    [rgb[0], rgb[1], rgb[2], accum[3] * opacity]
}

pub(super) fn quantize(pixel: [f32; 4]) -> [u8; 4] {
    pixel.map(|v| (v.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8)
}

/// Round-ties-to-even f32→f16. Matches the `rgba16float` stores in `effects.wgsl`.
pub(super) fn f32_to_f16_bits(value: f32) -> u16 {
    let x = value.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let mantissa = x & 0x007f_ffff;
    let exp = (x & 0x7f80_0000) >> 23;
    if exp == 0xff {
        return if mantissa == 0 {
            sign | 0x7c00
        } else {
            let payload = (mantissa >> 13) as u16;
            sign | 0x7c00 | payload | u16::from(payload == 0)
        };
    }
    let e = exp as i32 - 127 + 15;
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = mantissa | 0x0080_0000;
        let shift = (14 - e) as u32;
        let mut half = (m >> shift) as u16;
        let rem_mask = (1u32 << shift) - 1;
        let rem = m & rem_mask;
        let halfway = 1u32 << (shift - 1);
        if rem > halfway || (rem == halfway && (half & 1) == 1) {
            half += 1;
        }
        return sign | half;
    }
    if e >= 31 {
        return sign | 0x7c00;
    }
    let mut half = ((mantissa >> 13) as u16) | ((e as u16) << 10);
    let rem = mantissa & 0x1fff;
    if rem > 0x1000 || (rem == 0x1000 && (half & 1) == 1) {
        half += 1;
    }
    sign | half
}

pub(super) fn f16_to_f32(half: u16) -> f32 {
    let sign = ((half as u32) & 0x8000) << 16;
    let exp = ((half >> 10) & 0x1f) as u32;
    let mant = (half & 0x3ff) as u32;
    let bits = if exp == 0 {
        if mant == 0 {
            sign
        } else {
            let mut m = mant;
            let mut e = 113u32;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            m &= 0x3ff;
            sign | (e << 23) | (m << 13)
        }
    } else if exp == 31 {
        sign | 0x7f80_0000 | (mant << 13)
    } else {
        sign | ((exp + (127 - 15)) << 23) | (mant << 13)
    };
    f32::from_bits(bits)
}

pub(super) fn f16_round(value: f32) -> f32 {
    f16_to_f32(f32_to_f16_bits(value))
}
