//! Text Q14 bakes and their parity through the Renderer.
use super::*;

/// Render version 3 text reference (doc/design/2026-10-28-v3-integer-text.md
/// "着色と重ね", implementation order 3): a text node's premultiplied Q14 over
/// the level-pixel ROI `[ox, oy, w, h]`, from its bake only. Each glyph in bake
/// order: `C` from the world and the placement (a glyph whose `C` is out of
/// range is not drawn), the atlas band (`place_q`, the whole bitmap, cut to the
/// ROI) or the direct band (the ROI as the window), then the normal over its
/// straight Q14 colour (`source_over_q14`, the memo's `F = k * c_a`). A bake
/// that does not decode draws nothing (`TEXT_BAKE_INVALID`). There is no atlas
/// here: the cache state never changes a pixel.
pub(super) fn text_q14(
    bake: &[u8],
    world: alto_core::FixedAffine,
    level: i32,
    roi: [i64; 4],
) -> Vec<[u16; 4]> {
    use crate::glyph_q8::{
        atlas_band, direct_window, glyph_map_q, place_q, rasterize_atlas, rasterize_direct,
    };
    let [ox, oy, w, h] = roi;
    let mut acc = vec![[0u16; 4]; (w * h) as usize];
    let Ok(bake) = alto_core::text_bake::TextBake::decode(bake) else {
        return acc;
    };
    for g in &bake.glyphs {
        let outline = &bake.outlines[g.outline as usize];
        let Some(c) = glyph_map_q(world, level, g.m, g.t) else {
            continue;
        };
        let mut paint = |x: i64, y: i64, b: u8| {
            if b > 0 && (ox..ox + w).contains(&x) && (oy..oy + h).contains(&y) {
                let i = ((y - oy) * w + (x - ox)) as usize;
                acc[i] = crate::raster_q8::source_over_q14(acc[i], g.color, b);
            }
        };
        match atlas_band(&c, outline.upem)
            .then(|| place_q(&c, outline))
            .flatten()
        {
            Some(p) => {
                let Ok(bytes) = rasterize_atlas(&p, outline) else {
                    continue;
                };
                let [dx, dy] = p.dst();
                for (i, &b) in bytes.iter().enumerate() {
                    let (bx, by) = ((i % p.w as usize) as i64, (i / p.w as usize) as i64);
                    paint(dx + bx, dy + by, b);
                }
            }
            None => {
                // The window is the glyph's box within the ROI (the bytes are
                // the same as any larger window's, cut).
                let Some(window) = direct_window(&c, outline, roi) else {
                    continue;
                };
                let Ok(bytes) = rasterize_direct(&c, outline, window) else {
                    continue;
                };
                let [wx, wy, ww, _] = window;
                for (i, &b) in bytes.iter().enumerate() {
                    paint(wx + (i as i64 % ww), wy + (i as i64 / ww), b);
                }
            }
        }
    }
    acc
}

/// A v3 document with the parity fonts and the bake of `text` at `size`.
pub(super) fn bake_parity_text(text: &str, size: f32, color: [f32; 4]) -> Vec<u8> {
    bake_parity_content(
        alto_text::TextContent {
            text: text.into(),
            style: alto_text::TextStyle {
                size,
                color,
                ..Default::default()
            },
            ..Default::default()
        },
        2,
    )
}

pub(super) fn bake_parity_content(
    mut content: alto_text::TextContent,
    font_count: usize,
) -> Vec<u8> {
    use alto_doc::Op;
    let mut e = alto_engine::Engine::new();
    let mut meta = alto_doc::DocumentMeta::srgb_u8(512, 512);
    meta.render_version = alto_core::RENDER_VERSION_EXACT;
    let d = e.create_document(meta).unwrap();
    let mut fonts = vec![];
    let mut ops = vec![];
    for bytes in [
        &include_bytes!("../../../../tests/fonts/AltoTestLatin.ttf")[..],
        &include_bytes!("../../../../tests/fonts/AltoTestJP.otf")[..],
    ] {
        let font = e.import_font(bytes.to_vec(), 0).unwrap();
        fonts.push(font.record.id);
        ops.push(Op::SetAsset {
            id: font.record.id,
            record: Some(font.record),
        });
    }
    e.transact(d, "fonts", ops, 0).unwrap();
    content.style.fonts = fonts[..font_count].to_vec();
    e.bake_text(&e.document(d).unwrap().assets, &content)
        .encode()
}

/// The same ten level-0 scenes as the browser's enforced text-v3 cases.
pub(super) type TextHashCase = (String, Vec<u8>, [f64; 6], (i32, i32));
pub(super) fn text_v3_hash_cases() -> Vec<TextHashCase> {
    let content = |text: &str, size: f32, color: [f32; 4]| alto_text::TextContent {
        text: text.into(),
        style: alto_text::TextStyle {
            size,
            color,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut vertical = content("吾輩は猫である。\n第12回", 40.0, [0.0, 0.0, 0.0, 1.0]);
    vertical.writing_mode = alto_text::WritingMode::VerticalRl;
    vertical.frame = alto_text::TextFrame::Area {
        width: 300.0,
        height: 400.0,
    };
    let seam = bake_parity_content(content("Seam 継ぎ目\nWWW", 48.0, [0.4, 0.1, 0.6, 0.9]), 2);
    let mut cases = vec![
        (
            "text-v3-latin-atlas".into(),
            bake_parity_content(content("Alto 0.25 px/em", 13.5, [0.1, 0.2, 0.9, 1.0]), 1),
            [1.0, 0.0, 0.0, 1.0, 20.25, 60.625],
            (0, 0),
        ),
        (
            "text-v3-latin-direct".into(),
            bake_parity_content(content("Ag&", 61.0, [0.9, 0.1, 0.1, 0.5]), 1),
            [1.0, 0.0, 0.0, 1.0, 12.5, 20.375],
            (0, 0),
        ),
        (
            "text-v3-japanese".into(),
            bake_parity_content(content("吾輩は猫", 30.0, [0.0, 0.0, 0.0, 0.75]), 2),
            [1.0, 0.0, 0.0, 1.0, 10.0, 40.5],
            (0, 0),
        ),
        (
            "text-v3-vertical".into(),
            bake_parity_content(vertical, 2),
            [1.0, 0.0, 0.0, 1.0, -40.25, 8.0],
            (0, 0),
        ),
        (
            "text-v3-rotated".into(),
            bake_parity_content(content("Rotated 21", 26.0, [0.2, 0.6, 0.3, 1.0]), 1),
            [
                0.3f64.cos(),
                0.3f64.sin(),
                -0.3f64.sin(),
                0.3f64.cos(),
                40.0,
                30.0,
            ],
            (0, 0),
        ),
        (
            "text-v3-placeholder".into(),
            bake_parity_content(content("A B", 24.0, [0.0, 0.0, 0.0, 1.0]), 0),
            [1.0, 0.0, 0.0, 1.0, 30.0, 50.0],
            (0, 0),
        ),
    ];
    for (tx, ty) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        cases.push((
            format!("text-v3-seam-{}-{}", tx, ty),
            seam.clone(),
            [1.0, 0.0, 0.0, 1.0, 180.5, 200.25],
            (tx, ty),
        ));
    }
    cases
}

/// The parity texts' bakes: small (atlas band) and large (direct band) Latin,
/// and Japanese, at a document position with fractional placement.
pub(super) fn text_reference_bakes() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "latin 13.5 px",
            bake_parity_text("Alto 0.25 px/em", 13.5, [0.1, 0.2, 0.9, 1.0]),
        ),
        (
            "latin 61 px",
            bake_parity_text("Ag&", 61.0, [0.9, 0.1, 0.1, 0.5]),
        ),
        (
            "japanese 30 px",
            bake_parity_text("吾輩は猫", 30.0, [0.0, 0.0, 0.0, 0.75]),
        ),
    ]
}

pub(super) fn moved(x: f64, y: f64) -> alto_core::FixedAffine {
    alto_core::FixedAffine::from_affine(Affine2([1.0, 0.0, 0.0, 1.0, x, y])).unwrap()
}

#[test]
fn glyph_q14_does_not_overflow() {
    // Every coverage byte with the memo's E values for colour and accumulator,
    // rgb <= a in the accumulator: the result keeps a <= 16384 and rgb <= a.
    let e = [0u16, 1, 4095, 4096, 4097, 8191, 8192, 8193, 16383, 16384];
    for b in 0..=255u8 {
        for &ca in &e {
            for &crgb in &[0u16, 1, 8192, 16383, 16384] {
                for &aa in &e {
                    for &argb in e.iter().filter(|&&v| v <= aa) {
                        let out = crate::raster_q8::source_over_q14(
                            [argb, argb, argb, aa],
                            [crgb, crgb, crgb, ca],
                            b,
                        );
                        assert!(out[3] <= 16384, "a {out:?}");
                        assert!(
                            out[..3].iter().all(|&v| v <= out[3]),
                            "rgb {out:?} ({b}, {ca}, {crgb}, {aa}, {argb})"
                        );
                    }
                }
            }
        }
    }
    // An opaque full-coverage glyph replaces the accumulator exactly.
    let full =
        crate::raster_q8::source_over_q14([100, 200, 300, 400], [16384, 0, 8192, 16384], 255);
    assert_eq!(full, [16384, 0, 8192, 16384]);
}

#[test]
fn text_q14_draws_baked_text_in_both_bands() {
    for (name, bake) in text_reference_bakes() {
        let world = moved(20.25, 60.625);
        let q = text_q14(&bake, world, 0, [0, 0, 256, 256]);
        let ink = q.iter().filter(|p| p[3] > 0).count();
        assert!(ink > 50, "{name}: ink {ink}");
        assert!(
            q.iter()
                .all(|p| p[3] <= 16384 && p[..3].iter().all(|&v| v <= p[3])),
            "{name}"
        );
        // Nothing above or left of the text's origin.
        assert!(q[..256 * 10].iter().all(|p| p[3] == 0), "{name} drew above");
        assert!(
            (0..256).all(|y| q[y * 256..y * 256 + 15].iter().all(|p| p[3] == 0)),
            "{name} drew left"
        );
    }
    // A fully covered pixel of the opaque 13.5 px text is exactly its colour.
    let (_, latin) = &text_reference_bakes()[0];
    let q = text_q14(latin, moved(20.25, 60.625), 0, [0, 0, 256, 256]);
    let colour = [0.1f32, 0.2, 0.9, 1.0].map(alto_engine::text::text_color_q14);
    assert!(q.contains(&colour), "no fully covered pixel has {colour:?}");
    // Glyphs paint in bake order: two overlapping squares, the later on top.
    let square = alto_core::text_bake::BakeOutline {
        font: alto_core::text_bake::PLACEHOLDER_FONT,
        glyph: 0,
        upem: 1000,
        commands: vec![
            alto_core::text_bake::BakeCommand::Move([0, 0]),
            alto_core::text_bake::BakeCommand::Line([64_000, 0]),
            alto_core::text_bake::BakeCommand::Line([64_000, 64_000]),
            alto_core::text_bake::BakeCommand::Line([0, 64_000]),
            alto_core::text_bake::BakeCommand::Close,
        ],
    };
    // 40 px squares (direct band), the second 10 px right of the first.
    let glyph = |x: i64, color: [u16; 4]| alto_core::text_bake::BakeGlyph {
        outline: 0,
        m: [671_089, 0, 0, 671_089],
        t: [x << 30, 0],
        color,
    };
    let (red, blue) = ([16384, 0, 0, 16384], [0, 0, 16384, 16384]);
    let two = alto_core::text_bake::TextBake {
        baker: 1,
        fonts: vec![],
        outlines: vec![square],
        glyphs: vec![glyph(0, red), glyph(10, blue)],
    }
    .encode();
    let q = text_q14(&two, moved(0.0, 0.0), 0, [0, 0, 64, 64]);
    assert_eq!(q[20 * 64 + 5], red);
    assert_eq!(q[20 * 64 + 20], blue, "the later glyph is on top");
    // The bands are what the sizes say: 13.5 px atlas, 61 px direct.
    let bake = alto_core::text_bake::TextBake::decode(&text_reference_bakes()[0].1).unwrap();
    let glyph = &bake.glyphs[0];
    let c = crate::glyph_q8::glyph_map_q(moved(0.0, 0.0), 0, glyph.m, glyph.t).unwrap();
    assert!(crate::glyph_q8::atlas_band(
        &c,
        bake.outlines[glyph.outline as usize].upem
    ));
    let bake = alto_core::text_bake::TextBake::decode(&text_reference_bakes()[1].1).unwrap();
    let glyph = &bake.glyphs[0];
    let c = crate::glyph_q8::glyph_map_q(moved(0.0, 0.0), 0, glyph.m, glyph.t).unwrap();
    assert!(!crate::glyph_q8::atlas_band(
        &c,
        bake.outlines[glyph.outline as usize].upem
    ));
}

#[test]
fn text_q14_window_crop_and_tile_seams() {
    // One large ROI and its four quarters agree on every pixel, in both bands,
    // with the text across the seam and at a negative origin.
    for (name, bake) in text_reference_bakes() {
        let world = moved(-31.375, 70.5);
        let full = text_q14(&bake, world, 0, [-64, 0, 256, 256]);
        assert!(
            full.iter().any(|p| p[3] > 0),
            "{name}: the seam test needs ink"
        );
        // Columns 0..64 of this ROI are x = -64..0: the text crosses the origin.
        let left_of_zero = full
            .chunks(256)
            .any(|row| row[..64].iter().any(|p| p[3] > 0));
        assert!(left_of_zero, "{name}: no ink left of 0");
        for (qx, qy) in [(-64, 0), (64, 0), (-64, 128), (64, 128)] {
            let part = text_q14(&bake, world, 0, [qx, qy, 128, 128]);
            for y in 0..128 {
                let row = (y + qy as usize) * 256 + (qx + 64) as usize;
                assert_eq!(
                    &part[y * 128..y * 128 + 128],
                    &full[row..row + 128],
                    "{name} ({qx}, {qy}) row {y}"
                );
            }
        }
    }
}

#[test]
fn text_q14_direct_glyphs_draw_in_large_rois() {
    // A direct-band glyph in a 2048 x 2048 ROI (4 Mi pixels, past one window's
    // limit) draws through its own box, and matches a small ROI around it.
    let (_, bake) = &text_reference_bakes()[1];
    let world = moved(1500.0, 1600.0);
    let large = text_q14(bake, world, 0, [0, 0, 2048, 2048]);
    let small = text_q14(bake, world, 0, [1400, 1500, 256, 256]);
    assert!(small.iter().any(|p| p[3] > 0));
    for y in 0..256usize {
        let row = (1500 + y) * 2048 + 1400;
        assert_eq!(
            &small[y * 256..y * 256 + 256],
            &large[row..row + 256],
            "row {y}"
        );
    }
}

#[test]
fn text_q14_invalid_bake_and_levels() {
    let (_, bake) = text_reference_bakes().remove(0);
    let world = moved(20.0, 60.0);
    // A bake that does not decode draws nothing.
    for broken in [&bake[..bake.len() - 1], &[0u8; 8][..], &[][..]] {
        assert!(
            text_q14(broken, world, 0, [0, 0, 64, 64])
                .iter()
                .all(|p| *p == [0; 4])
        );
    }
    // Level -1 doubles the text; level 1 halves it. Ink scales accordingly.
    let ink = |level: i32| {
        text_q14(&bake, world, level, [0, 0, 512, 512])
            .iter()
            .map(|p| u64::from(p[3]))
            .sum::<u64>()
    };
    let (half, one, two) = (ink(1), ink(0), ink(-1));
    assert!(two > 3 * one && one > 3 * half, "ink {half} {one} {two}");
    // Outside -16..=12 the world does not map and nothing is drawn.
    assert!(
        text_q14(&bake, world, 13, [0, 0, 8, 8])
            .iter()
            .all(|p| *p == [0; 4])
    );
}

/// A v3 scene of one text node drawn from `bake` at `[x, y]` on a `w x h` canvas.
pub(super) fn text_scene(bake: Option<Vec<u8>>, x: f64, y: f64, canvas: [i32; 2]) -> RenderScene {
    let content = RContent::TextBake(bake.map(|bytes| alto_scene::RTextBake {
        hash: ContentHash::of(&bytes),
        bytes: std::sync::Arc::new(bytes),
    }));
    let mut document = scene(content, RFeatures::default());
    document.canvas.x1 = canvas[0];
    document.canvas.y1 = canvas[1];
    document.nodes[0].transform = [1.0, 0.0, 0.0, 1.0, x, y];
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    document
}

/// The tile bytes of `text_q14` (a lone node over transparent is a Q14 copy).
pub(super) fn text_tile_reference(bake: &[u8], x: f64, y: f64, key: (i32, i32, i32)) -> Vec<u8> {
    let (level, tx, ty) = key;
    let q = text_q14(
        bake,
        moved(x, y),
        level,
        [i64::from(tx) * 256, i64::from(ty) * 256, 256, 256],
    );
    q.iter()
        .flat_map(|p| p.map(|v| ((u32::from(v) * 255 + 8192) >> 14) as u8))
        .collect()
}

/// A bake of `n` overlapping `px`-pixel squares (direct band), each moved 7 px
/// right and 5 px down from the last, with alternating translucent colours.
pub(super) fn overlapping_squares(n: i64, px: i64) -> Vec<u8> {
    use alto_core::text_bake::{BakeCommand, BakeGlyph, BakeOutline, PLACEHOLDER_FONT, TextBake};
    let square = BakeOutline {
        font: PLACEHOLDER_FONT,
        glyph: 0,
        upem: 1000,
        commands: vec![
            BakeCommand::Move([0, 0]),
            BakeCommand::Line([64_000, 0]),
            BakeCommand::Line([64_000, 64_000]),
            BakeCommand::Line([0, 64_000]),
            BakeCommand::Close,
        ],
    };
    // px / 1000 node pixels per font unit, Q24.
    let m = ((px << 24) / 1000) as i32;
    let glyphs = (0..n)
        .map(|i| BakeGlyph {
            outline: 0,
            m: [m, 0, 0, m],
            t: [(7 * i) << 30, (5 * i) << 30],
            color: if i % 2 == 0 {
                [16384, 4096, 0, 9000]
            } else {
                [0, 8192, 16384, 12000]
            },
        })
        .collect();
    TextBake {
        baker: 1,
        fonts: vec![],
        outlines: vec![square],
        glyphs,
    }
    .encode()
}

#[test]
fn v3_text_matches_the_integer_reference() {
    let mut bakes = text_reference_bakes();
    // Over 1024 glyphs (two glyph-q14 chunks), and overlapping 200 px direct
    // glyphs whose crops overflow one scratch (several scratches for a tile).
    let block = format!("{}\n", "AltoAltoAlto".repeat(4)).repeat(34);
    bakes.push((
        "many small",
        bake_parity_text(&block, 6.0, [0.2, 0.3, 0.4, 1.0]),
    ));
    bakes.push(("many large", overlapping_squares(6, 200)));
    // The two stress texts reach the paths they are for.
    let crops = |i: usize| {
        crate::glyph_q8::glyph_bitmaps(&bakes[i].1, moved(20.25, 60.625), 0, [0, 0, 256, 256])
    };
    assert!(
        crops(3).len() > 1024,
        "many small: {} crops",
        crops(3).len()
    );
    let area: u64 = crops(4)
        .iter()
        .map(|c| u64::from(c.w) * u64::from(c.h))
        .sum();
    assert!(area > 256 * 256, "many large: crop area {area}");
    for (name, bake) in &bakes {
        for (x, y, key) in [
            (20.25, 60.625, (0, 0, 0)),
            // Across the seam: the right tile of a text starting left of 256.
            (180.5, 30.0, (0, 1, 0)),
            // Zoomed in (level -1): every glyph twice as large.
            (10.0, 20.0, (-1, 0, 0)),
        ] {
            let document = text_scene(Some(bake.clone()), x, y, [512, 512]);
            assert_eq!(crate::renderer::v3_gap(&document), None);
            let Some(gpu) = v3_vector_test_render_at(&document, key) else {
                return;
            };
            let cpu = text_tile_reference(bake, x, y, key);
            let ink = cpu.iter().skip(3).step_by(4).filter(|&&a| a > 0).count();
            let differ = gpu
                .chunks(4)
                .zip(cpu.chunks(4))
                .filter(|(g, c)| g != c)
                .count();
            assert_eq!(
                differ, 0,
                "{name} at ({x}, {y}) key {key:?}: {differ} pixels differ, ink {ink}"
            );
        }
    }
    // Unbaked and undecodable nodes are transparent.
    for bake in [None, Some(vec![1, 2, 3])] {
        let document = text_scene(bake, 20.0, 60.0, [256, 256]);
        let Some(gpu) = v3_vector_test_render(&document) else {
            return;
        };
        assert!(gpu.iter().all(|&b| b == 0));
    }
}

#[test]
fn v3_text_matches_the_locked_hashes() {
    let locked: std::collections::HashMap<_, _> =
        parse_reference(&std::fs::read_to_string(reference_path()).unwrap())
            .into_iter()
            .map(|(name, blake3, fnv)| (name, (blake3, fnv)))
            .collect();
    for (name, bake, transform, (tx, ty)) in text_v3_hash_cases() {
        let mut document = text_scene(Some(bake), 0.0, 0.0, [512, 512]);
        document.nodes[0].transform = transform;
        let Some(gpu) = v3_vector_test_render_at(&document, (0, tx, ty)) else {
            return;
        };
        let (blake3, fnv) = &locked[&name];
        assert_eq!(ContentHash::of(&gpu).hex(), *blake3, "{name}");
        assert_eq!(format!("{:016x}", fnv64(&gpu)), *fnv, "{name}");
    }
}
