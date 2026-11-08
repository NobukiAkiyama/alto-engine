//! Scenes run through the real Renderer, and their CPU tile references.
use super::*;

/// A masked raster drawn through the renderer for render version 3
/// (`Renderer::read_tile`), so its mask tiles are the ones
/// `Context::mask_entries` selects and uploads. The raster is the opaque
/// gradient (`mask_images` 4) in the display tile `key` at level 0, under a
/// root group; the mask spans several tiles.
pub(super) struct RendererMaskCase {
    pub(super) name: &'static str,
    pub(super) key: [i32; 2],
    pub(super) linked: bool,
    pub(super) transform: [f64; 6],
    pub(super) opacity: f32,
    /// R8 mask tiles by tile key.
    pub(super) mask: Vec<([i32; 2], TileSpec)>,
}

impl RendererMaskCase {
    pub(super) fn region(&self) -> [i32; 3] {
        [self.key[0] * 256, self.key[1] * 256, 1]
    }
    /// The mask's document transform as `apply_props` links it (world = identity).
    pub(super) fn document_transform(&self) -> [f64; 6] {
        if self.linked {
            crate::renderer::multiply(ident(), self.transform)
        } else {
            self.transform
        }
    }
}

/// R8 mask bytes below 255 (so a 255 tile is told apart), different for each `seed`.
pub(super) fn mask_tile_bytes(seed: i32) -> Vec<u8> {
    (0..256 * 256)
        .map(|i: i32| ((i % 256) * 7 + (i / 256) * 13 + seed * 31).rem_euclid(200) as u8)
        .collect()
}

/// A tile of the renderer cases, rebuilt from a seed in Rust and in
/// `tests/e2e/m5-v3-renderer-reference.mjs`.
#[derive(Clone, Copy, Debug)]
pub(super) enum TileSpec {
    /// RGBA, [`raster_tile_bytes`].
    Raster(i32),
    /// RGBA, [`raster_tile_bytes`] with alpha 255.
    Opaque(i32),
    /// R8, [`mask_tile_bytes`].
    Mask(i32),
    /// R8, all 255.
    White,
}

impl TileSpec {
    pub(super) fn bytes(self) -> Vec<u8> {
        match self {
            Self::Raster(seed) => raster_tile_bytes(seed),
            Self::Opaque(seed) => {
                let mut bytes = raster_tile_bytes(seed);
                for px in bytes.as_chunks_mut::<4>().0 {
                    px[3] = 255;
                }
                bytes
            }
            Self::Mask(seed) => mask_tile_bytes(seed),
            Self::White => vec![255; 256 * 256],
        }
    }
    pub(super) fn json(self) -> String {
        match self {
            Self::Raster(seed) => format!("[\"raster\",{seed}]"),
            Self::Opaque(seed) => format!("[\"opaque\",{seed}]"),
            Self::Mask(seed) => format!("[\"mask\",{seed}]"),
            Self::White => "[\"white\",0]".into(),
        }
    }
    /// R8 mask bytes as the atlas holds them (`rgba_pixels`: `[v, v, v, 255]`).
    pub(super) fn mask_rgba(self) -> Vec<u8> {
        self.bytes()
            .iter()
            .flat_map(|v| [*v, *v, *v, 255])
            .collect()
    }
}

pub(super) fn renderer_mask_cases() -> Vec<RendererMaskCase> {
    // At origin 999936 the Q16 inverse (x coefficient 65537) reads texel
    // 1000195 at pixel (255, 0), in tile (3907, 0). The f64 inverse puts the
    // ROI's right edge at 1000191.0000000001, before that tile.
    let far = RendererMaskCase {
        name: "renderer-mask-origin-999936",
        key: [3906, 0],
        linked: false,
        transform: [0.9999900000999989, 0.0, 0.0, 1.0, 11.00180998190018, 0.0],
        opacity: 1.0,
        mask: vec![
            ([3905, 0], TileSpec::Mask(1)),
            ([3906, 0], TileSpec::Mask(2)),
            ([3907, 0], TileSpec::White),
        ],
    };
    // A linked mask shrunk by 0.8 and rotated by 0.3 rad about the center of
    // display tile (5, 3): the tile reads mask tiles (4..=6, 2..=4).
    let (s, c) = 0.3f64.sin_cos();
    let (k, cx, cy) = (0.8, 5.0 * 256.0 + 128.0, 3.0 * 256.0 + 128.0);
    let rotated = RendererMaskCase {
        name: "renderer-mask-rotated-3x3",
        key: [5, 3],
        linked: true,
        transform: [
            k * c,
            k * s,
            -k * s,
            k * c,
            cx - k * (c * cx - s * cy),
            cy - k * (s * cx + c * cy),
        ],
        opacity: 0.5,
        mask: (2..=4)
            .flat_map(|ty| (4..=6).map(move |tx| ([tx, ty], TileSpec::Mask(tx * 3 + ty))))
            .collect(),
    };
    vec![far, rotated]
}

/// A pixel mask (or an effect selection) of a [`RendererLayer`]: R8 tiles by
/// key, with its transform, linked to the node or not.
#[derive(Clone)]
pub(super) struct RendererCaseMask {
    pub(super) linked: bool,
    pub(super) transform: [f64; 6],
    pub(super) tiles: Vec<([i32; 2], TileSpec)>,
}

impl RendererCaseMask {
    /// The mask's document transform under node world `world`, as
    /// `Context::mask_entries` links it.
    pub(super) fn document_fixed(&self, world: [f64; 6]) -> Option<alto_core::FixedAffine> {
        let local = alto_core::FixedAffine::from_affine(Affine2(self.transform))?;
        if self.linked {
            alto_core::FixedAffine::from_affine(Affine2(world))?.concat(local)
        } else {
            Some(local)
        }
    }
    /// The tiles as the atlas holds them.
    pub(super) fn rgba(&self) -> Vec<([i32; 2], Vec<u8>)> {
        self.tiles
            .iter()
            .map(|(k, s)| (*k, s.mask_rgba()))
            .collect()
    }
}

/// One child of the root group of a [`RendererCase`], bottom first.
#[derive(Clone)]
pub(super) enum RendererLayer {
    /// A raster node: RGBA tiles by key under node transform `transform`, with
    /// an optional color-adjust layer effect `(contrast, saturation,
    /// brightness)` and its selection, a pixel mask, opacity and blend.
    Raster {
        tiles: Vec<([i32; 2], TileSpec)>,
        transform: [f64; 6],
        adjust: Option<(f32, f32, f32)>,
        selection: Option<RendererCaseMask>,
        mask: Option<RendererCaseMask>,
        opacity: f32,
        blend: RenderBlend,
    },
    /// A color-adjust adjustment layer at the identity transform.
    Adjust {
        settings: (f32, f32, f32),
        mask: Option<RendererCaseMask>,
        opacity: f32,
        blend: RenderBlend,
    },
}

/// A render version 3 scene composed by the renderer (`Renderer::read_tile`),
/// so every stage uniform, tile selection and the resolve are the host's own
/// (`Context::q14_params`, `lookup_entries`, `resolve_span`). The display tile
/// is `key` at `level` (level 1 is the integer pyramid of four level-0 tiles),
/// and the canvas is `[0, canvas)`. `refused` is the `GpuUnsupported` detail
/// the render must fail with, if any.
pub(super) struct RendererCase {
    pub(super) name: String,
    pub(super) level: i32,
    pub(super) key: [i32; 2],
    pub(super) canvas: [i32; 2],
    pub(super) layers: Vec<RendererLayer>,
    pub(super) refused: Option<&'static str>,
}

impl From<&RendererMaskCase> for RendererCase {
    /// The masked opaque gradient of a [`RendererMaskCase`] at its display tile.
    fn from(case: &RendererMaskCase) -> Self {
        Self {
            name: case.name.into(),
            level: 0,
            key: case.key,
            canvas: [(case.key[0] + 1) * 256, (case.key[1] + 1) * 256],
            layers: vec![RendererLayer::Raster {
                tiles: vec![(case.key, TileSpec::Opaque(0))],
                transform: ident(),
                adjust: None,
                selection: None,
                mask: Some(RendererCaseMask {
                    linked: case.linked,
                    transform: case.transform,
                    tiles: case.mask.clone(),
                }),
                opacity: case.opacity,
                blend: RenderBlend::Normal,
            }],
            refused: None,
        }
    }
}

/// RGBA raster tile bytes, different for each `seed`: the color-adjust
/// gradient (every channel value, alpha 0, 1, 128, 254) moved by `seed` rows
/// and columns, so a texel off by one is seen.
pub(super) fn raster_tile_bytes(seed: i32) -> Vec<u8> {
    let gradient = color_adjust_gradient();
    let mut out = vec![0; 256 * 256 * 4];
    for y in 0..256 {
        for x in 0..256 {
            let from = (((y + seed * 37).rem_euclid(256) * 256 + (x + seed * 11).rem_euclid(256))
                * 4) as usize;
            let to = ((y * 256 + x) * 4) as usize;
            out[to..to + 4].copy_from_slice(&gradient[from..from + 4]);
        }
    }
    out
}

/// The real-renderer cases of the float purge: every v3 M5 mode the host
/// dispatches (18, 17, 19 below opacity 1, 20, 21, 22, 23) at the origin, at
/// negative tile keys, near 10^6 px and next to 2^23 px (the last tile whose
/// Q8 pixel centers fit i32), raster transforms whose source spans several
/// tiles, a canvas edge through the tile, level 1 (the pyramid), and the
/// refusals past the integer range.
pub(super) fn renderer_cases() -> Vec<RendererCase> {
    let vivid = (1.7, 1.6, 0.0);
    let (s, c) = 0.3f64.sin_cos();
    // A rotation by 0.3 rad and a scale by `k` about the center of display tile `key`.
    let rotation = |key: [i32; 2], k: f64| {
        let (cx, cy) = (
            f64::from(key[0]) * 256.0 + 128.0,
            f64::from(key[1]) * 256.0 + 128.0,
        );
        [
            k * c,
            k * s,
            -k * s,
            k * c,
            cx - k * (c * cx - s * cy),
            cy - k * (s * cx + c * cy),
        ]
    };
    // Raster or mask tiles over keys `x` by `y`, seeded by key.
    let rasters = |x: std::ops::RangeInclusive<i32>, y: std::ops::RangeInclusive<i32>| {
        y.flat_map(|ty| {
            x.clone()
                .map(move |tx| ([tx, ty], TileSpec::Raster(tx * 5 + ty)))
        })
        .collect::<Vec<_>>()
    };
    let masks = |x: std::ops::RangeInclusive<i32>, y: std::ops::RangeInclusive<i32>| {
        y.flat_map(|ty| {
            x.clone()
                .map(move |tx| ([tx, ty], TileSpec::Mask(tx * 3 + ty)))
        })
        .collect::<Vec<_>>()
    };
    let raster = |tiles, transform, adjust, opacity, blend| RendererLayer::Raster {
        tiles,
        transform,
        adjust,
        selection: None,
        mask: None,
        opacity,
        blend,
    };
    let case = |name: String, key: [i32; 2], layers: Vec<RendererLayer>| RendererCase {
        name,
        level: 0,
        key,
        canvas: [(key[0] + 1) * 256, (key[1] + 1) * 256],
        layers,
        refused: None,
    };
    let mut out: Vec<RendererCase> = renderer_mask_cases().iter().map(Into::into).collect();
    // Display tiles on the canvas (it starts at 0, so a negative display key is
    // all transparent), with their raster and mask tiles at `src`, moved under
    // the display tile by a translation that is not a whole tile: at the
    // origin, at negative keys, near -10^6 px, and the last tile below 2^23 px
    // on both axes.
    for (key, src, off) in [
        ([0, 0], [0, 0], [0.0, 0.0]),
        ([1, 1], [-3, -2], [37.0, -21.0]),
        ([3906, 5], [-3907, 5], [-11.0, 5.0]),
        ([32767, 32767], [32767, 32767], [0.0, 0.0]),
    ] {
        let tag = format!("{key:?} from {src:?}");
        let shift = [
            f64::from(key[0] - src[0]) * 256.0 + off[0],
            f64::from(key[1] - src[1]) * 256.0 + off[1],
        ];
        let place =
            |t: [f64; 6]| crate::renderer::multiply([1.0, 0.0, 0.0, 1.0, shift[0], shift[1]], t);
        let at = |k: [i32; 2]| vec![(k, TileSpec::Raster(k[0] - k[1]))];
        let opaque = |k: [i32; 2]| {
            raster(
                vec![(k, TileSpec::Opaque(k[0] + k[1]))],
                place(ident()),
                None,
                1.0,
                RenderBlend::Normal,
            )
        };
        // Modes 18, 17 and 19 below opacity 1 (multiply onto the opaque layer).
        out.push(case(
            format!("renderer {tag} color-adjust layer multiply o=0.35"),
            key,
            vec![
                opaque(src),
                raster(
                    at(src),
                    place(ident()),
                    Some(vivid),
                    0.35,
                    RenderBlend::Multiply,
                ),
            ],
        ));
        // Mode 20: an adjustment layer at opacity 0.5 over a translucent layer.
        out.push(case(
            format!("renderer {tag} adjustment o=0.5"),
            key,
            vec![
                raster(at(src), place(ident()), None, 1.0, RenderBlend::Normal),
                RendererLayer::Adjust {
                    settings: vivid,
                    mask: None,
                    opacity: 0.5,
                    blend: RenderBlend::Normal,
                },
            ],
        ));
        // Modes 22, 23 and 21 with rotated masks over several tiles. Next to
        // 2^23 the rotations pivot two tiles in and halve the source, so every
        // source position stays below 2^23 px (otherwise refused, below).
        let k = |dx: i32, dy: i32| [src[0] + dx, src[1] + dy];
        let (pivot, k_mask, k_raster) = if key[0] > 30_000 {
            (k(-2, -2), 2.0, 2.0)
        } else {
            (src, 0.8, 1.25)
        };
        let rotated = RendererCaseMask {
            linked: false,
            transform: place(rotation(pivot, k_mask)),
            tiles: masks(src[0] - 1..=src[0] + 1, src[1] - 1..=src[1] + 1),
        };
        out.push(case(
            format!("renderer {tag} selection, masked screen adjustment o=0.5"),
            key,
            vec![
                RendererLayer::Raster {
                    tiles: at(src),
                    transform: place(ident()),
                    adjust: Some(vivid),
                    selection: Some(rotated.clone()),
                    mask: None,
                    opacity: 1.0,
                    blend: RenderBlend::Normal,
                },
                RendererLayer::Adjust {
                    settings: (0.6, 0.2, 0.1),
                    mask: Some(rotated.clone()),
                    opacity: 0.5,
                    blend: RenderBlend::Screen,
                },
            ],
        ));
        // Mode 18 through a rotated, scaled raster whose source spans several
        // tiles (the Q16 tile selection), then a linked mask on it (mode 21).
        out.push(case(
            format!("renderer {tag} rotated raster, linked mask o=0.5"),
            key,
            vec![
                opaque(src),
                RendererLayer::Raster {
                    tiles: rasters(src[0] - 1..=src[0] + 1, src[1] - 1..=src[1] + 1),
                    transform: place(rotation(pivot, k_raster)),
                    adjust: Some(vivid),
                    selection: None,
                    mask: Some(RendererCaseMask {
                        linked: true,
                        transform: [1.0, 0.0, 0.0, 1.0, 37.0, -21.0],
                        tiles: masks(src[0] - 2..=src[0] + 1, src[1] - 2..=src[1] + 1),
                    }),
                    opacity: 0.5,
                    blend: RenderBlend::Normal,
                },
            ],
        ));
    }
    // A canvas edge through a tile near 10^6 px: the resolve span.
    let key = [3906, 7];
    out.push(RendererCase {
        canvas: [3906 * 256 + 100, 7 * 256 + 211],
        ..case(
            "renderer [3906, 7] canvas edge".into(),
            key,
            vec![raster(
                vec![(key, TileSpec::Raster(9))],
                ident(),
                Some(vivid),
                0.5,
                RenderBlend::Normal,
            )],
        )
    });
    // Level 1, the integer pyramid over four level-0 M5 tiles, with the raster
    // at the origin and at negative keys.
    for (key, src) in [([0, 0], [0, 0]), ([1, 1], [-4, -3])] {
        let shift = [
            f64::from(key[0] * 2 - src[0]) * 256.0,
            f64::from(key[1] * 2 - src[1]) * 256.0,
        ];
        let layers = vec![
            raster(
                rasters(src[0] - 1..=src[0] + 2, src[1] - 1..=src[1] + 2),
                crate::renderer::multiply(
                    [1.0, 0.0, 0.0, 1.0, shift[0], shift[1]],
                    rotation(src, 0.9),
                ),
                Some(vivid),
                0.5,
                RenderBlend::Normal,
            ),
            RendererLayer::Adjust {
                settings: (1.2, 0.5, -0.1),
                mask: None,
                opacity: 0.35,
                blend: RenderBlend::Normal,
            },
        ];
        out.push(RendererCase {
            level: 1,
            canvas: [(key[0] + 1) * 512, (key[1] + 1) * 512],
            ..case(
                format!("renderer {key:?} from {src:?} level 1"),
                key,
                layers,
            )
        });
    }
    // The plain integer tile compositor (no effect or mask, so not M5): an
    // opaque base shifted off the tile grid, a 1.00004x scale (Q16 within 6 of
    // unit: the nearest draw that was an f32 fetch) multiplied at opacity 0.7,
    // and a rotated 4-tap layer screened at 0.5, at the same sources as above.
    for (key, src, off) in [
        ([0, 0], [0, 0], [0.25, 0.0]),
        ([1, 1], [-3, -2], [37.5, -21.25]),
        ([3906, 5], [-3907, 5], [-11.75, 5.5]),
        ([32767, 32767], [32767, 32767], [0.0, 0.0]),
    ] {
        let shift = [
            f64::from(key[0] - src[0]) * 256.0 + off[0],
            f64::from(key[1] - src[1]) * 256.0 + off[1],
        ];
        let place =
            |t: [f64; 6]| crate::renderer::multiply([1.0, 0.0, 0.0, 1.0, shift[0], shift[1]], t);
        let (pivot, k_raster) = if key[0] > 30_000 {
            ([src[0] - 2, src[1] - 2], 2.0)
        } else {
            (src, 1.25)
        };
        let around = |k: [i32; 2]| (k[0] - 1..=k[0] + 1, k[1] - 1..=k[1] + 1);
        let (xs, ys) = around(src);
        let opaques: Vec<_> = ys
            .clone()
            .flat_map(|ty| {
                xs.clone()
                    .map(move |tx| ([tx, ty], TileSpec::Opaque(tx - ty)))
            })
            .collect();
        // A 1.00004x scale about the pivot tile's center.
        let (cx, cy) = (
            f64::from(pivot[0]) * 256.0 + 128.0,
            f64::from(pivot[1]) * 256.0 + 128.0,
        );
        let near_unit = [
            1.00004,
            0.0,
            0.0,
            1.00004,
            cx - 1.00004 * cx,
            cy - 1.00004 * cy,
        ];
        out.push(case(
            format!("plain {key:?} from {src:?} nearest, near-unit multiply, rotated screen"),
            key,
            vec![
                raster(opaques, place(ident()), None, 1.0, RenderBlend::Normal),
                raster(
                    rasters(xs.clone(), ys.clone()),
                    place(near_unit),
                    None,
                    0.7,
                    RenderBlend::Multiply,
                ),
                raster(
                    rasters(xs, ys),
                    place(rotation(pivot, k_raster)),
                    None,
                    0.5,
                    RenderBlend::Screen,
                ),
            ],
        ));
    }
    // The opaque-occluder shortcut: an opaque layer over all the tiles its
    // integer draw reads hides the layer below; one with a missing tile in
    // that range (here, half a pixel past the only tile) does not.
    for (name, tiles) in [
        (
            "covering",
            vec![([4, 4], TileSpec::Opaque(1)), ([5, 4], TileSpec::Opaque(2))],
        ),
        ("half a pixel short", vec![([4, 4], TileSpec::Opaque(1))]),
    ] {
        out.push(case(
            format!("plain [4, 4] occluder {name}"),
            [4, 4],
            vec![
                raster(
                    vec![([4, 4], TileSpec::Raster(3))],
                    ident(),
                    None,
                    1.0,
                    RenderBlend::Normal,
                ),
                raster(
                    tiles,
                    [1.0, 0.0, 0.0, 1.0, -0.5, 0.0],
                    None,
                    1.0,
                    RenderBlend::Normal,
                ),
            ],
        ));
    }
    // The plain resolve at a canvas edge near 10^6 px, and the plain pyramid.
    out.push(RendererCase {
        canvas: [3906 * 256 + 100, 7 * 256 + 211],
        ..case(
            "plain [3906, 7] canvas edge".into(),
            [3906, 7],
            vec![raster(
                vec![([3906, 7], TileSpec::Raster(9))],
                [1.0, 0.0, 0.0, 1.0, 0.5, -0.5],
                None,
                0.6,
                RenderBlend::Normal,
            )],
        )
    });
    out.push(RendererCase {
        level: 1,
        canvas: [512 * 2, 512 * 2],
        ..case(
            "plain [1, 1] from [-4, -3] level 1".into(),
            [1, 1],
            vec![raster(
                rasters(-5..=-2, -4..=-1),
                crate::renderer::multiply(
                    [1.0, 0.0, 0.0, 1.0, 6.0 * 256.0 + 0.5, 5.0 * 256.0],
                    rotation([-4, -3], 0.9),
                ),
                None,
                0.8,
                RenderBlend::Normal,
            )],
        )
    });
    // The browser's plain tile 32767 (Linus 08:10): the color-adjust gradient
    // (the browser imports `colorAdjustGradient`) moved there on the wide
    // fixture's canvas; also compared with JS (`renderer_rust_and_js_references_agree`).
    out.push(RendererCase {
        canvas: [8_389_120, 256],
        ..case(
            "plain [32767, 0] wide fixture, moved gradient".into(),
            [32767, 0],
            vec![RendererLayer::Raster {
                tiles: vec![([0, 0], TileSpec::Raster(0))],
                transform: [1.0, 0.0, 0.0, 1.0, 8_388_352.0, 0.0],
                adjust: None,
                selection: None,
                mask: None,
                opacity: 1.0,
                blend: RenderBlend::Normal,
            }],
        )
    });
    // Plain refusals: the first tile at 2^23 px, and an inverse with no Q16
    // value (a 10^-6 scale inverts to 10^6, past an i32 coefficient).
    out.push(RendererCase {
        refused: Some("v3 draw ROI has no integer tile range"),
        ..case(
            "plain [32768, 0] refused".into(),
            [32768, 0],
            vec![raster(
                vec![([32768, 0], TileSpec::Raster(1))],
                ident(),
                None,
                1.0,
                RenderBlend::Normal,
            )],
        )
    });
    out.push(RendererCase {
        refused: Some("v3 draw transform has no Q16 inverse"),
        ..case(
            "plain [0, 0] tiny scale refused".into(),
            [0, 0],
            vec![raster(
                vec![([0, 0], TileSpec::Raster(1))],
                [1e-6, 0.0, 0.0, 1e-6, 0.0, 0.0],
                None,
                1.0,
                RenderBlend::Normal,
            )],
        )
    });
    // Refused: the first tile at 2^23 px has no Q8 pixel center in i32, for the
    // raster (mode 18) and, with the raster near the origin, for a mask whose
    // corner lands past 2^23 px.
    let key = [32768, 0];
    out.push(RendererCase {
        refused: Some("v3 raster ROI has no integer tile range"),
        ..case(
            "renderer [32768, 0] raster refused".into(),
            key,
            vec![raster(
                vec![(key, TileSpec::Raster(1))],
                ident(),
                Some(vivid),
                1.0,
                RenderBlend::Normal,
            )],
        )
    });
    // A mask whose source passes 2^23 texels on tile (0, 0) from pixel 208 on
    // (DQ3-4, Linus 04:40): the shader drops the pixels whose mask Q8 leaves i32. With the mask
    // tiles ending at texel 2^23 - 1 those pixels read nothing either way, so it
    // draws (byte 0 there, as the CPU). With a mask tile at 2^23 they could read
    // it, so that tile is refused.
    for (name, tiles, refused) in [
        ("mask up to 2^23 draws", masks(32767..=32767, 0..=0), None),
        (
            "mask tile at 2^23 refused",
            masks(32767..=32768, 0..=0),
            Some("v3 mask ROI has no integer tile range"),
        ),
    ] {
        out.push(RendererCase {
            refused,
            ..case(
                format!("renderer [0, 0] {name}"),
                [0, 0],
                vec![RendererLayer::Raster {
                    tiles: vec![([0, 0], TileSpec::Opaque(2))],
                    transform: ident(),
                    adjust: None,
                    selection: None,
                    mask: Some(RendererCaseMask {
                        linked: false,
                        transform: [1.0, 0.0, 0.0, 1.0, -8_388_400.0, 0.0],
                        tiles,
                    }),
                    opacity: 1.0,
                    blend: RenderBlend::Normal,
                }],
            )
        });
    }
    out
}

/// A mask's atlas tiles and its document transform, for the CPU reference.
pub(super) type PreparedMask = (Vec<([i32; 2], Vec<u8>)>, alto_core::FixedAffine);

/// CPU reference for a level-0 tile `key` of a [`RendererCase`], evaluated per
/// pixel as `renderer/tile_renderer/` chains the stages, with the checked Q14 functions
/// of [`q14_program_cpu`]. Every target shares the tile's ROI. Pixels whose
/// center is off the canvas are transparent (the exact doubled test, not
/// `resolve_span`).
pub(super) fn renderer_tile_cpu(case: &RendererCase, key: [i32; 2]) -> Vec<u8> {
    if case.plain() {
        return plain_tile_cpu(case, key);
    }
    let region = [key[0] * 256, key[1] * 256, 1];
    let q = q14::Q;
    let clamp = |v: [i64; 4]| v.map(|c| c.clamp(0, q));
    let opacity = |o: f32| i64::from(crate::composite_opacity_q14(o));
    struct Prepared {
        raster: Vec<([i32; 2], Vec<u8>)>,
        mask: Option<PreparedMask>,
        selection: Option<PreparedMask>,
    }
    let prepared: Vec<Prepared> = case
        .layers
        .iter()
        .map(|layer| match layer {
            RendererLayer::Raster {
                tiles,
                transform,
                selection,
                mask,
                ..
            } => Prepared {
                raster: tiles.iter().map(|(k, spec)| (*k, spec.bytes())).collect(),
                mask: mask
                    .as_ref()
                    .map(|m| (m.rgba(), m.document_fixed(*transform).unwrap())),
                selection: selection
                    .as_ref()
                    .map(|m| (m.rgba(), m.document_fixed(*transform).unwrap())),
            },
            RendererLayer::Adjust { mask, .. } => Prepared {
                raster: vec![],
                mask: mask
                    .as_ref()
                    .map(|m| (m.rgba(), m.document_fixed(ident()).unwrap())),
                selection: None,
            },
        })
        .collect();
    let coverage = |m: &PreparedMask, x: i32, y: i32| {
        let tiles: Vec<([i32; 2], &[u8])> = m.0.iter().map(|(k, b)| (*k, &b[..])).collect();
        i64::from(byte_q14(u32::from(
            tiles_texel_fixed_cpu(&tiles, m.1, region, x, y).expect("v3 refuses the mask")[0],
        )))
    };
    let adjust = |v: [i64; 4], (contrast, saturation, brightness): (f32, f32, f32)| {
        let rgb = color_adjust_exact_q14(
            v.map(|c| c as u32),
            crate::renderer::color_adjust_q14(brightness),
            crate::renderer::color_adjust_q14(contrast),
            crate::renderer::color_adjust_q14(saturation),
        );
        clamp([rgb[0], rgb[1], rgb[2], v[3] as u32].map(i64::from))
    };
    let mut out = vec![0u8; 256 * 256 * 4];
    for y in 0..256 {
        for x in 0..256 {
            let on = |o: i32, i: i32, end: i32| {
                (0..2 * i64::from(end)).contains(&(2 * (i64::from(o) + i64::from(i)) + 1))
            };
            if !on(region[0], x, case.canvas[0]) || !on(region[1], y, case.canvas[1]) {
                continue;
            }
            let mut accum = [0i64; 4];
            for (layer, p) in case.layers.iter().zip(&prepared) {
                match layer {
                    RendererLayer::Raster {
                        transform,
                        adjust: settings,
                        opacity: o,
                        blend,
                        ..
                    } => {
                        let tiles: Vec<([i32; 2], &[u8])> =
                            p.raster.iter().map(|(k, b)| (*k, &b[..])).collect();
                        let bytes = tiles_texel_cpu(&tiles, *transform, region, x, y)
                            .expect("v3 refuses the raster")
                            .map(u32::from);
                        let mut src = [
                            premul_byte_q14(bytes[0], bytes[3]),
                            premul_byte_q14(bytes[1], bytes[3]),
                            premul_byte_q14(bytes[2], bytes[3]),
                            byte_q14(bytes[3]),
                        ]
                        .map(i64::from);
                        if let Some(settings) = settings {
                            let adjusted = adjust(src, *settings);
                            src = match &p.selection {
                                Some(m) => clamp(
                                    mask_select_q14(src, adjusted, coverage(m, x, y)).unwrap(),
                                ),
                                None => adjusted,
                            };
                        }
                        let mut op = opacity(*o);
                        if let Some(m) = &p.mask {
                            src = clamp(mask_layer_q14(src, coverage(m, x, y) * op).unwrap());
                            op = q;
                        }
                        let mode = *blend as u32;
                        accum = clamp(if op == q {
                            q14::blend(
                                accum,
                                q14::straight(src),
                                [src[0], src[1], src[2]],
                                src[3],
                                mode,
                            )
                        } else {
                            q14::pop(accum, src, op, mode)
                        });
                    }
                    RendererLayer::Adjust {
                        settings,
                        opacity: o,
                        blend,
                        ..
                    } => {
                        let adjusted = adjust(accum, *settings);
                        let op = opacity(*o);
                        accum = match &p.mask {
                            Some(m) => clamp(
                                mask_adjust_q14(
                                    accum,
                                    adjusted,
                                    coverage(m, x, y) * op,
                                    *blend as u32,
                                )
                                .unwrap(),
                            ),
                            None if op != q => std::array::from_fn(|c| {
                                div_q_i64(accum[c] * (q - op) + adjusted[c] * op, q).unwrap()
                            }),
                            None => adjusted,
                        };
                    }
                }
            }
            let i = ((y * 256 + x) * 4) as usize;
            out[i..i + 4].copy_from_slice(&q14::bytes(accum));
        }
    }
    out
}

/// CPU reference for a [`RendererCase`]: the level-0 tile, or at level 1 the
/// pyramid's `(a + b + c + d + 2) / 4` over the four level-0 children
/// (`downsample-q14.wgsl`), all of which the cases keep on the canvas.
pub(super) fn renderer_case_cpu(case: &RendererCase) -> Vec<u8> {
    if case.level == 0 {
        return renderer_tile_cpu(case, case.key);
    }
    assert_eq!(case.level, 1, "{}", case.name);
    let children: Vec<Vec<u8>> = (0..4)
        .map(|i| renderer_tile_cpu(case, [case.key[0] * 2 + i % 2, case.key[1] * 2 + i / 2]))
        .collect();
    let mut out = vec![0u8; 256 * 256 * 4];
    for y in 0..256 {
        for x in 0..256 {
            let child = &children[(y / 128) * 2 + x / 128];
            for c in 0..4 {
                let at = |dx: usize, dy: usize| {
                    u32::from(child[(((y % 128) * 2 + dy) * 256 + (x % 128) * 2 + dx) * 4 + c])
                };
                out[(y * 256 + x) * 4 + c] =
                    ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8;
            }
        }
    }
    out
}

impl RendererCase {
    /// A copy with the same canvas and layers.
    pub(super) fn clone_layers(&self) -> Self {
        Self {
            name: self.name.clone(),
            level: self.level,
            key: self.key,
            canvas: self.canvas,
            layers: self.layers.clone(),
            refused: self.refused,
        }
    }
    /// Whether every layer is a raster with no effect or mask: the renderer
    /// draws such a scene with the integer tile compositor, not M5.
    pub(super) fn plain(&self) -> bool {
        self.layers.iter().all(|l| {
            matches!(
                l,
                RendererLayer::Raster {
                    adjust: None,
                    selection: None,
                    mask: None,
                    ..
                }
            )
        })
    }
}

/// CPU reference for a level-0 tile `key` of a plain [`RendererCase`]: the
/// draws of [`composite_integer`], every tile of every layer listed (the
/// renderer's tile selection and opaque-occluder shortcut must not change it).
pub(super) fn plain_tile_cpu(case: &RendererCase, key: [i32; 2]) -> Vec<u8> {
    let ops: Vec<Op> = case
        .layers
        .iter()
        .filter_map(|layer| {
            let RendererLayer::Raster {
                tiles,
                transform,
                opacity,
                blend,
                ..
            } = layer
            else {
                unreachable!("a plain case has rasters only");
            };
            let mut tiles: Vec<Tile> = tiles
                .iter()
                .map(|(k, spec)| Tile {
                    tx: k[0],
                    ty: k[1],
                    pixels: spec.bytes(),
                })
                .collect();
            tiles.sort_by_key(|t| (t.tx, t.ty));
            let fixed = alto_core::FixedAffine::from_affine(Affine2(*transform))?;
            let q = crate::sample::fixed_inverse(&fixed)??;
            Some(Op::Draw {
                inv: [0.0; 6],
                q: Some(q),
                opacity: *opacity,
                mode: *blend as u32,
                tiles,
            })
        })
        .collect();
    let frame = Frame {
        origin: [0.0; 2],
        scale: 1.0,
        origin_px: [key[0] * 256, key[1] * 256],
        scale_i: 1,
        version: 3,
    };
    composite_integer(&ops, case.canvas[0] as u32, case.canvas[1] as u32, &frame).0
}
