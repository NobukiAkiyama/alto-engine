//! Integer Q14 stage programs as the tile renderer dispatches them, and their CPU run.
use super::*;

/// The Q28 divisor of the mask factor `F = m * o` (`div28` in `effects-q14.wgsl`).
pub(super) const Q28: i64 = 1 << 28;

/// Mode 21, the layer mask and opacity: `div28(v * F)` on all four channels.
/// `None` when a product or sum leaves i64.
pub(super) fn mask_layer_q14(v: [i64; 4], f: i64) -> Option<[i64; 4]> {
    let mut out = [0; 4];
    for c in 0..4 {
        out[c] = div_q_i64(v[c].checked_mul(f)?, Q28)?;
    }
    Some(out)
}

/// Mode 22, the effect selection: `div_q(in * (16384 - s) + f * s, 16384)`.
/// The v2 mode 12 mix with coverage `k` is the same formula.
pub(super) fn mask_select_q14(input: [i64; 4], filtered: [i64; 4], s: i64) -> Option<[i64; 4]> {
    let mut out = [0; 4];
    for c in 0..4 {
        let n = input[c]
            .checked_mul(q14::Q - s)?
            .checked_add(filtered[c].checked_mul(s)?)?;
        out[c] = div_q_i64(n, q14::Q)?;
    }
    Some(out)
}

/// Mode 23 `g`: the adjusted copy, or for a non-normal blend, the straight
/// product (screen for 2, multiply for every other mode, as v2) premultiplied by
/// the backdrop alpha.
pub(super) fn mask_adjust_blend_q14(
    orig: [i64; 4],
    filt: [i64; 4],
    blend: u32,
) -> Option<[i64; 4]> {
    if blend == 0 {
        return Some(filt);
    }
    let d = q14::straight(orig);
    let t = q14::straight(filt);
    let mut g = [0, 0, 0, orig[3]];
    for c in 0..3 {
        let color = if blend == 2 {
            q14::Q - div_q_i64((q14::Q - d[c]).checked_mul(q14::Q - t[c])?, q14::Q)?
        } else {
            div_q_i64(d[c].checked_mul(t[c])?, q14::Q)?
        };
        g[c] = div_q_i64(color.checked_mul(orig[3])?, q14::Q)?;
    }
    Some(g)
}

/// Mode 23, the adjustment-layer mask, opacity and blend:
/// `div28(orig * (2^28 - F) + g * F)`.
pub(super) fn mask_adjust_q14(
    orig: [i64; 4],
    filt: [i64; 4],
    f: i64,
    blend: u32,
) -> Option<[i64; 4]> {
    let g = mask_adjust_blend_q14(orig, filt, blend)?;
    let mut out = [0; 4];
    for c in 0..4 {
        let n = orig[c]
            .checked_mul(Q28 - f)?
            .checked_add(g[c].checked_mul(f)?)?;
        out[c] = div_q_i64(n, Q28)?;
    }
    Some(out)
}

/// The byte a v3 mask stage reads at pixel (x, y) of a target with the integer
/// ROI `region` (`[x, y, level scale]`), from one mask tile at key (0, 0) whose
/// R channel is the byte. See [`tiles_texel_cpu`].
pub(super) fn mask_byte_cpu(
    image: &[u8],
    transform: [f64; 6],
    region: [i32; 3],
    x: i32,
    y: i32,
) -> Option<u8> {
    mask_tiles_byte_cpu(&[([0, 0], image)], transform, region, x, y)
}

/// The mask byte (R) of [`tiles_texel_cpu`].
pub(super) fn mask_tiles_byte_cpu(
    tiles: &[([i32; 2], &[u8])],
    transform: [f64; 6],
    region: [i32; 3],
    x: i32,
    y: i32,
) -> Option<u8> {
    Some(tiles_texel_cpu(tiles, transform, region, x, y)?[0])
}

/// The RGBA bytes a v3 stage reads from the atlas (mode 18 the raster, modes 21
/// to 23 the mask) at pixel (x, y) of a target with the integer ROI `region`
/// (`[x, y, level scale]`), from the RGBA8 tiles `tiles` by tile key, with
/// document-to-pixel `transform`. Nearest: the texel is the floor of the Q8
/// position. A texel in no listed tile, or a position without an i32 value, is
/// transparent. As the host (`renderer::mask_sampler`), every lookup is Q16; it
/// is evaluated here from `integer_inverse` and `source_q8`, not from the packed
/// bits. `None` when the host refuses the lookup.
pub(super) fn tiles_texel_cpu(
    tiles: &[([i32; 2], &[u8])],
    transform: [f64; 6],
    region: [i32; 3],
    x: i32,
    y: i32,
) -> Option<[u8; 4]> {
    let fixed = alto_core::FixedAffine::from_affine(Affine2(transform))?;
    tiles_texel_fixed_cpu(tiles, fixed, region, x, y)
}

pub(super) fn tiles_texel_fixed_cpu(
    tiles: &[([i32; 2], &[u8])],
    fixed: alto_core::FixedAffine,
    region: [i32; 3],
    x: i32,
    y: i32,
) -> Option<[u8; 4]> {
    let [ox, oy, scale] = region;
    crate::renderer::fixed_mask_sampler(&fixed, [ox, oy, scale])?;
    let inv = crate::sample::fixed_inverse(&fixed)??;
    let (Some(px), Some(py)) = (pixel_q8(ox, x, scale), pixel_q8(oy, y, scale)) else {
        return Some([0; 4]);
    };
    let Some(q) = source_q8(&inv, px, py) else {
        return Some([0; 4]);
    };
    let (tx, ty) = (floor_frac_q8(q[0]).0, floor_frac_q8(q[1]).0);
    let key = [tx >> 8, ty >> 8];
    Some(
        tiles
            .iter()
            .find(|(k, _)| *k == key)
            .map_or([0; 4], |(_, image)| {
                let i = (((ty - key[1] * 256) * 256 + tx - key[0] * 256) * 4) as usize;
                [image[i], image[i + 1], image[i + 2], image[i + 3]]
            }),
    )
}

/// The document transform that puts a 256² tile's pixel `i` under pixel `i` of
/// a target with ROI `region`: mode 18 of a raster drawn as it is.
pub(super) fn placement(region: [i32; 3]) -> [f64; 6] {
    let [x, y, scale] = region.map(f64::from);
    [scale, 0.0, 0.0, scale, x, y]
}

/// One `effects-q14.wgsl` dispatch over 256² targets with the identity ROI.
/// `a`/`b` are input targets, `slot` the atlas layer mode 18 reads, and
/// `settings` the Q14 integers the host bit-casts into the uniform.
#[derive(Clone, Copy)]
pub(super) struct Q14Stage {
    pub(super) mode: u32,
    pub(super) settings: [i32; 4],
    pub(super) a: usize,
    pub(super) b: usize,
    pub(super) out: usize,
    pub(super) slot: u32,
    /// Modes 21 to 23: the mask's document transform (`RMask` after linking).
    pub(super) transform: [f64; 6],
    /// Mode 18: the raster's document transform. `None` places the atlas layer
    /// as it is at the output ROI (`placement`).
    pub(super) raster: Option<[f64; 6]>,
}

/// An M5 graph as `renderer/tile_renderer/` dispatches it for render version 3. Target 0
/// is never written, so it is the empty backdrop.
pub(super) struct Q14Program {
    pub(super) name: String,
    pub(super) targets: usize,
    /// The integer ROI `[x, y, level scale]` of each 256² target; a target past
    /// the end is `[0, 0, 1]`.
    pub(super) regions: Vec<[i32; 3]>,
    pub(super) stages: Vec<Q14Stage>,
    pub(super) result: usize,
}

impl Q14Program {
    pub(super) fn region(&self, target: usize) -> [i32; 3] {
        self.regions.get(target).copied().unwrap_or([0, 0, 1])
    }
}

/// The texel of a 256² target with ROI `input` under pixel (x, y) of a target
/// with ROI `out`: `floor((p - origin) / scale)` at the pixel center
/// `p = out_origin + (i + 1/2) * out_scale`, exact in i64. `None` outside.
pub(super) fn input_texel_cpu(out: [i32; 3], input: [i32; 3], x: i32, y: i32) -> Option<usize> {
    let axis = |o: i32, i: i32, v: i32| {
        let n = 2 * (i64::from(o) - i64::from(i)) + (2 * i64::from(v) + 1) * i64::from(out[2]);
        n.div_euclid(2 * i64::from(input[2]))
    };
    let (tx, ty) = (axis(out[0], input[0], x), axis(out[1], input[1], y));
    ((0..256).contains(&tx) && (0..256).contains(&ty)).then(|| (ty * 256 + tx) as usize)
}

impl Q14Stage {
    /// The atlas lookup transform at output ROI `region`: the raster of mode 18,
    /// the mask of modes 21 to 23, none for the rest.
    pub(super) fn lookup_transform(&self, region: [i32; 3]) -> Option<[f64; 6]> {
        match self.mode {
            18 => Some(self.raster.unwrap_or_else(|| placement(region))),
            21..=23 => Some(self.transform),
            _ => None,
        }
    }
    /// Whether the stage reads input `a` and input `b`, as `renderer/tile_renderer/`
    /// passes them to `q14_params`.
    pub(super) fn reads(&self) -> [bool; 2] {
        match self.mode {
            8 | 17 => [true, false],
            19..=23 => [true, true],
            _ => [false, false],
        }
    }
    /// Mode 18 of a raster with document transform `world`.
    pub(super) fn raster(slot: u32, world: [f64; 6], out: usize) -> Self {
        Self {
            raster: Some(world),
            ..Self::ingress(slot, out)
        }
    }
    pub(super) fn ingress(slot: u32, out: usize) -> Self {
        Self {
            mode: 18,
            settings: [0; 4],
            a: 0,
            b: 0,
            out,
            slot,
            transform: ident(),
            raster: None,
        }
    }
    pub(super) fn layer(dst: usize, src: usize, opacity: f32, mode: u32, out: usize) -> Self {
        Self {
            mode: 19,
            settings: [crate::composite_opacity_q14(opacity), mode as i32, 0, 0],
            a: dst,
            b: src,
            out,
            slot: 0,
            transform: ident(),
            raster: None,
        }
    }
    pub(super) fn color_adjust(
        input: usize,
        (contrast, saturation, brightness): (f32, f32, f32),
        out: usize,
    ) -> Self {
        Self {
            mode: 17,
            settings: [
                crate::renderer::color_adjust_q14(brightness),
                crate::renderer::color_adjust_q14(contrast),
                crate::renderer::color_adjust_q14(saturation),
                0,
            ],
            a: input,
            b: input,
            out,
            slot: 0,
            transform: ident(),
            raster: None,
        }
    }
    pub(super) fn adjustment_mix(
        backdrop: usize,
        adjusted: usize,
        opacity: f32,
        out: usize,
    ) -> Self {
        Self {
            mode: 20,
            settings: [crate::composite_opacity_q14(opacity), 0, 0, 0],
            a: backdrop,
            b: adjusted,
            out,
            slot: 0,
            transform: ident(),
            raster: None,
        }
    }
    /// Mode 21: the layer mask in atlas layer `slot` and the layer opacity.
    pub(super) fn mask(
        input: usize,
        slot: u32,
        opacity: f32,
        transform: [f64; 6],
        out: usize,
    ) -> Self {
        Self {
            mode: 21,
            settings: [crate::composite_opacity_q14(opacity), 1, 0, 0],
            a: input,
            b: input,
            out,
            slot,
            transform,
            raster: None,
        }
    }
    /// Mode 22: `before` where the selection in `slot` is 0, `after` where it is 255.
    pub(super) fn select(before: usize, after: usize, slot: u32, out: usize) -> Self {
        Self {
            mode: 22,
            settings: [0; 4],
            a: before,
            b: after,
            out,
            slot,
            transform: ident(),
            raster: None,
        }
    }
    /// Mode 23: an adjustment layer's mask (or none), opacity and blend.
    pub(super) fn adjust(
        backdrop: usize,
        adjusted: usize,
        mask: Option<u32>,
        opacity: f32,
        blend: u32,
        out: usize,
    ) -> Self {
        Self {
            mode: 23,
            settings: [
                crate::composite_opacity_q14(opacity),
                i32::from(mask.is_some()),
                blend as i32,
                0,
            ],
            a: backdrop,
            b: adjusted,
            out,
            slot: mask.unwrap_or(0),
            transform: ident(),
            raster: None,
        }
    }
}

/// CPU reference for a [`Q14Program`], one stage at a time, with the checked
/// Q14 functions of the integer tile compositor. A stage reads each input at
/// the texel under its output pixel (`input_texel_cpu`). Mode 18 reads the one
/// tile at key (0, 0) in its `slot` through the Q16 lookup of its raster
/// transform (by default the atlas layer as it is, `placement`), and a mask
/// stage its mask the same way.
pub(super) fn q14_program_cpu(images: &[Vec<u8>], program: &Q14Program) -> Vec<u8> {
    let mut t = vec![vec![[0i64; 4]; 256 * 256]; program.targets];
    for stage in &program.stages {
        let region = program.region(stage.out);
        let tiles = [([0, 0], &images[stage.slot as usize][..])];
        let lookup = stage.lookup_transform(region);
        let read = |target: usize, x, y| {
            input_texel_cpu(region, program.region(target), x, y).map_or([0; 4], |j| t[target][j])
        };
        let next: Vec<[i64; 4]> = (0..256 * 256)
            .map(|i: i32| {
                let (x, y) = (i % 256, i / 256);
                let a = read(stage.a, x, y);
                let b = read(stage.b, x, y);
                let w = i64::from(stage.settings[0]);
                match stage.mode {
                    8 => a,
                    18 => {
                        let rgba = tiles_texel_cpu(&tiles, lookup.unwrap(), region, x, y)
                            .expect("v3 refuses a raster without a Q16 lookup");
                        let alpha = u32::from(rgba[3]);
                        let mut q = [0i64; 4];
                        for c in 0..3 {
                            q[c] = i64::from(premul_byte_q14(u32::from(rgba[c]), alpha));
                        }
                        q[3] = i64::from(byte_q14(alpha));
                        q
                    }
                    19 => {
                        let mode = stage.settings[1] as u32;
                        if w == q14::Q {
                            q14::blend(a, q14::straight(b), [b[0], b[1], b[2]], b[3], mode)
                        } else {
                            q14::pop(a, b, w, mode)
                        }
                    }
                    17 => {
                        let q = a.map(|v| v as u32);
                        let rgb = color_adjust_exact_q14(
                            q,
                            stage.settings[0],
                            stage.settings[1],
                            stage.settings[2],
                        );
                        [rgb[0], rgb[1], rgb[2], q[3]].map(i64::from)
                    }
                    20 => std::array::from_fn(|c| {
                        div_q_i64(a[c] * (q14::Q - w) + b[c] * w, q14::Q).unwrap()
                    }),
                    21..=23 => {
                        let m = || {
                            i64::from(byte_q14(u32::from(
                                mask_tiles_byte_cpu(&tiles, lookup.unwrap(), region, x, y)
                                    .expect("v3 refuses a mask without a Q16 lookup"),
                            )))
                        };
                        let f = |has: i32| if has == 0 { q14::Q } else { m() } * w;
                        match stage.mode {
                            21 => mask_layer_q14(a, f(stage.settings[1])),
                            22 => mask_select_q14(a, b, m()),
                            _ => mask_adjust_q14(
                                a,
                                b,
                                f(stage.settings[1]),
                                stage.settings[2] as u32,
                            ),
                        }
                        .expect("mask Q14 terms fit in i64")
                    }
                    mode => panic!("no CPU reference for Q14 mode {mode}"),
                }
                .map(|v| v.clamp(0, q14::Q))
            })
            .collect();
        t[stage.out] = next;
    }
    t[program.result]
        .iter()
        .flat_map(|q| q14::bytes(*q))
        .collect()
}

/// The browser `color-adjust` test as an M5 graph: an adjustment layer over one
/// raster. Raster ingress, the copy onto the empty backdrop, then the Q14 adjust.
pub(super) fn color_adjust_program(settings: (f32, f32, f32)) -> Q14Program {
    Q14Program {
        name: color_adjust_case_name(settings.0, settings.1, settings.2),
        targets: 4,
        regions: vec![],
        stages: vec![
            Q14Stage::ingress(0, 1),
            Q14Stage::layer(0, 1, 1.0, 0, 2),
            Q14Stage::color_adjust(2, settings, 3),
        ],
        result: 3,
    }
}

/// Atlas layers for [`opacity_programs`]: the color-adjust gradient, opaque
/// blue, opaque red and half-alpha red.
pub(super) fn opacity_images() -> Vec<Vec<u8>> {
    vec![
        color_adjust_gradient(),
        [0, 0, 255, 255].repeat(256 * 256),
        [255, 0, 0, 255].repeat(256 * 256),
        [255, 0, 0, 128].repeat(256 * 256),
    ]
}

/// Opacity 0.5 through the integer M5 stages, enforced on every backend: a layer
/// at half opacity (normal and four other modes), a color-adjusted layer at half
/// opacity, and a color-adjust adjustment layer at half opacity.
pub(super) fn opacity_programs() -> Vec<Q14Program> {
    let vivid = (1.7, 1.6, 0.0);
    let mut out = vec![Q14Program {
        name: "m5-opacity-half".into(),
        targets: 5,
        regions: vec![],
        stages: vec![
            Q14Stage::ingress(1, 1),
            Q14Stage::layer(0, 1, 1.0, 0, 2),
            Q14Stage::ingress(2, 3),
            Q14Stage::layer(2, 3, 0.5, 0, 4),
        ],
        result: 4,
    }];
    for (mode, name) in [
        (1, "multiply"),
        (2, "screen"),
        (9, "soft-light"),
        (14, "color"),
    ] {
        out.push(Q14Program {
            name: format!("m5-opacity-half-{name}"),
            targets: 5,
            regions: vec![],
            stages: vec![
                Q14Stage::ingress(0, 1),
                Q14Stage::layer(0, 1, 1.0, 0, 2),
                Q14Stage::ingress(3, 3),
                Q14Stage::layer(2, 3, 0.5, mode, 4),
            ],
            result: 4,
        });
    }
    out.push(Q14Program {
        name: "m5-color-adjust-layer-opacity-half".into(),
        targets: 6,
        regions: vec![],
        stages: vec![
            Q14Stage::ingress(1, 1),
            Q14Stage::layer(0, 1, 1.0, 0, 2),
            Q14Stage::ingress(0, 3),
            Q14Stage::color_adjust(3, vivid, 4),
            Q14Stage::layer(2, 4, 0.5, 0, 5),
        ],
        result: 5,
    });
    out.push(Q14Program {
        name: "m5-adjustment-opacity-half".into(),
        targets: 5,
        regions: vec![],
        stages: vec![
            Q14Stage::ingress(0, 1),
            Q14Stage::layer(0, 1, 1.0, 0, 2),
            Q14Stage::color_adjust(2, vivid, 3),
            Q14Stage::adjustment_mix(2, 3, 0.5, 4),
        ],
        result: 4,
    });
    out
}
