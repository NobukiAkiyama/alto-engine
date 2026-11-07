//! Colour adjust references for versions 1 to 3.
use super::*;

/// 256² straight RGBA. R is x, G is y, B is `(x * 3 + y * 5) & 255` (every row
/// hits every blue). Alpha is 0, 1, 128, 254 on the first four rows and x after that,
/// so each channel, including those four alpha anchors, appears.
pub(super) fn color_adjust_gradient() -> Vec<u8> {
    let mut pixels = vec![0u8; 256 * 256 * 4];
    for y in 0..256u32 {
        for x in 0..256u32 {
            let i = ((y * 256 + x) * 4) as usize;
            pixels[i] = x as u8;
            pixels[i + 1] = y as u8;
            pixels[i + 2] = ((x * 3 + y * 5) & 255) as u8;
            pixels[i + 3] = match y {
                0 => 0,
                1 => 1,
                2 => 128,
                3 => 254,
                _ => x as u8,
            };
        }
    }
    pixels
}

/// Contrast, saturation, brightness. The cast is `f64 as f32`, the same one
/// `render_effect` uses when a document number becomes the mode 3 uniform.
/// The first 12 entries are the interior cross product. The last 8 are the
/// document-range endpoints.
pub(super) fn color_adjust_grid() -> [(f32, f32, f32); 20] {
    const INTERIOR_CONTRAST: [f32; 2] = [0.5f64 as f32, 1.7f64 as f32];
    const INTERIOR_SATURATION: [f32; 3] = [0.0f64 as f32, 0.35f64 as f32, 1.6f64 as f32];
    const INTERIOR_BRIGHTNESS: [f32; 2] = [(-0.35f64) as f32, 0.45f64 as f32];
    const END_CONTRAST: [f32; 2] = [0.0f64 as f32, 4.0f64 as f32];
    const END_SATURATION: [f32; 2] = [0.0f64 as f32, 4.0f64 as f32];
    const END_BRIGHTNESS: [f32; 2] = [(-1.0f64) as f32, 1.0f64 as f32];
    let mut out = [(0.0f32, 0.0, 0.0); 20];
    let mut i = 0;
    for contrast in INTERIOR_CONTRAST {
        for saturation in INTERIOR_SATURATION {
            for brightness in INTERIOR_BRIGHTNESS {
                out[i] = (contrast, saturation, brightness);
                i += 1;
            }
        }
    }
    for contrast in END_CONTRAST {
        for saturation in END_SATURATION {
            for brightness in END_BRIGHTNESS {
                out[i] = (contrast, saturation, brightness);
                i += 1;
            }
        }
    }
    assert_eq!(i, 20);
    out
}

pub(super) fn color_adjust_case_name(contrast: f32, saturation: f32, brightness: f32) -> String {
    format!("color-adjust c={contrast} s={saturation} b={brightness}")
}

pub(super) const Q14_ONE: i32 = 16384;
pub(super) const Q14_HALF: i32 = 8192;
/// Luma weights in Q14. They sum to [`Q14_ONE`].
pub(super) const Q14_LUMA: [i32; 3] = [3483, 11718, 1183];

/// `(n + d/2) / d` for `d > 0`, with the sign taken so a tie moves away from zero.
pub(super) fn div_q_i64(n: i64, d: i64) -> Option<i64> {
    let half = d / 2;
    if n >= 0 {
        n.checked_add(half)?.checked_div(d)
    } else {
        n.checked_neg()?
            .checked_add(half)?
            .checked_div(d)?
            .checked_neg()
    }
}

/// `luma * (16384 - saturation) + channel * saturation`.
pub(super) fn saturation_mix_sum(luma: i64, channel: i64, saturation: i64) -> Option<i64> {
    let gap = i64::from(Q14_ONE).checked_sub(saturation)?;
    luma.checked_mul(gap)?
        .checked_add(channel.checked_mul(saturation)?)
}

/// Mode 17. `q` is premultiplied Q14 plus alpha. Settings are already Q14.
/// The returned channels are premultiplied Q14; alpha is not part of the result
/// because the stage passes the input alpha through unchanged.
///
/// Straight channels are 0..=16384. `color_adjust_q14` quantizes a setting into
/// −16384..=65536 (the f32 is clamped to [−1, 4] first). Brightness in the
/// document is −1..=1, so its Q14 value is −16384..=16384. The contrast stage
/// is not clamped: `(straight - 8192) * contrast / 16384 + 8192 + brightness`
/// lands in −40960..=57344, and luma stays inside that span. The saturation
/// sum, treating luma and the channel as independent inside the span and
/// saturation inside −16384..=65536, is −5502926848..=5771362304. That fits
/// i64 and does not fit i32. The only clamp is 0..=16384 after the mix, which
/// matches `clamp(rgb, 0, 1)`.
pub(super) fn color_adjust_exact_q14_checked(
    q: [u32; 4],
    brightness: i32,
    contrast: i32,
    saturation: i32,
) -> Option<[u32; 3]> {
    let alpha = i64::from(q[3]);
    let one = i64::from(Q14_ONE);
    let half = i64::from(Q14_HALF);
    let brightness = i64::from(brightness);
    let contrast = i64::from(contrast);
    let saturation = i64::from(saturation);
    let mut straight = [0i64; 3];
    if alpha != 0 {
        for i in 0..3 {
            let s = i64::from(q[i]).checked_shl(14)?.checked_div(alpha)?;
            straight[i] = s.clamp(0, one);
        }
    }
    let mut rgb = [0i64; 3];
    for i in 0..3 {
        rgb[i] = div_q_i64(straight[i].checked_sub(half)?.checked_mul(contrast)?, one)?
            .checked_add(half)?
            .checked_add(brightness)?;
    }
    let mut acc = 0i64;
    for i in 0..3 {
        acc = acc.checked_add(rgb[i].checked_mul(i64::from(Q14_LUMA[i]))?)?;
    }
    let luma = div_q_i64(acc, one)?;
    let mut out = [0u32; 3];
    for i in 0..3 {
        let mixed = div_q_i64(saturation_mix_sum(luma, rgb[i], saturation)?, one)?;
        let premul = div_q_i64(mixed.clamp(0, one).checked_mul(alpha)?, one)?;
        out[i] = u32::try_from(premul).ok()?;
    }
    Some(out)
}

pub(super) fn color_adjust_exact_q14(
    q: [u32; 4],
    brightness: i32,
    contrast: i32,
    saturation: i32,
) -> [u32; 3] {
    color_adjust_exact_q14_checked(q, brightness, contrast, saturation)
        .expect("color-adjust Q14 products fit in i64")
}

/// One texel through the color-adjust passes: mode 0 premultiply, normal blend
/// onto clear, mode 3, f16 between stores, unorm8 resolve.
pub(super) fn color_adjust_texel(
    rgba: [u8; 4],
    brightness: f32,
    contrast: f32,
    saturation: f32,
) -> [u8; 4] {
    let straight = rgba.map(|b| f32::from(b) / 255.0);
    let premul = [
        f16_round(straight[0] * straight[3]),
        f16_round(straight[1] * straight[3]),
        f16_round(straight[2] * straight[3]),
        f16_round(straight[3]),
    ];
    let blended = effects_blend([0.0; 4], premul, 0).map(f16_round);
    let eps = 0.000001f32;
    let alpha = blended[3];
    let scale = alpha.max(eps);
    let mut rgb = [blended[0] / scale, blended[1] / scale, blended[2] / scale];
    for channel in &mut rgb {
        *channel = (*channel - 0.5) * contrast + 0.5 + brightness;
    }
    // `dot` as a left fold of f32 products. Saturation 1 multiplies this by zero.
    let luma = (rgb[0] * 0.2126 + rgb[1] * 0.7152) + rgb[2] * 0.0722;
    for channel in &mut rgb {
        *channel = luma * (1.0 - saturation) + *channel * saturation;
        *channel = channel.clamp(0.0, 1.0) * alpha;
    }
    quantize([
        f16_round(rgb[0]),
        f16_round(rgb[1]),
        f16_round(rgb[2]),
        f16_round(alpha),
    ])
}

/// Straight byte to Q14. `(b * 16384 + 127) / 255`, capped at 1.
pub(super) fn byte_q14(byte: u32) -> u32 {
    (byte * 16384 + 127) / 255
}

/// Premultiplied Q14 of two unorm bytes. Opaque 255 stays at 1.
pub(super) fn premul_byte_q14(color: u32, alpha: u32) -> u32 {
    (color * alpha * 16384 + 32_512) / 65_025
}

/// Render version 3, the integer M5 chain: mode 18 premultiplies the recovered
/// bytes into Q14, mode 19 copies them onto the empty backdrop, mode 17 adjusts
/// with alpha unchanged, and the resolve writes `div_q(q * 255, 16384)` bytes.
/// Every stage stores exact Q14 in `rgba16uint`, so nothing rounds in between.
pub(super) fn color_adjust_texel_v3(
    rgba: [u8; 4],
    brightness: f32,
    contrast: f32,
    saturation: f32,
) -> [u8; 4] {
    let bytes = rgba.map(u32::from);
    let alpha = bytes[3];
    let q = [
        premul_byte_q14(bytes[0], alpha),
        premul_byte_q14(bytes[1], alpha),
        premul_byte_q14(bytes[2], alpha),
        byte_q14(alpha),
    ];
    let rgb = color_adjust_exact_q14(
        q,
        crate::renderer::color_adjust_q14(brightness),
        crate::renderer::color_adjust_q14(contrast),
        crate::renderer::color_adjust_q14(saturation),
    );
    q14::bytes([rgb[0], rgb[1], rgb[2], q[3]].map(i64::from))
}

pub(super) fn color_adjust_image(
    src: &[u8],
    brightness: f32,
    contrast: f32,
    saturation: f32,
) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    for (dst, px) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<4>().0)
    {
        *dst = color_adjust_texel(*px, brightness, contrast, saturation);
    }
    out
}

pub(super) fn color_adjust_image_v3(
    src: &[u8],
    brightness: f32,
    contrast: f32,
    saturation: f32,
) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    for (dst, px) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<4>().0)
    {
        *dst = color_adjust_texel_v3(*px, brightness, contrast, saturation);
    }
    out
}

pub(super) fn color_adjust_tile(
    rgba: [u8; 4],
    brightness: f32,
    contrast: f32,
    saturation: f32,
) -> Vec<u8> {
    let mut solid = vec![0u8; 256 * 256 * 4];
    for chunk in solid.as_chunks_mut::<4>().0 {
        *chunk = rgba;
    }
    color_adjust_image(&solid, brightness, contrast, saturation)
}
