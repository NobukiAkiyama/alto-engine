use alto_core::AssetId;
use alto_text::*;
use rustybuzz::ttf_parser;
use std::sync::Arc;

const JP: &[u8] = include_bytes!("../../../tests/fonts/AltoTestJP.otf");
const LATIN: &[u8] = include_bytes!("../../../tests/fonts/AltoTestLatin.ttf");

struct Fixture {
    set: FontSet,
    jp: AssetId,
    latin: AssetId,
}
fn fixture() -> Fixture {
    let (jp, latin) = (AssetId::new(), AssetId::new());
    let mut set = FontSet::default();
    set.insert(FontData::new(jp, Arc::new(JP.to_vec()), 0).unwrap());
    set.insert(FontData::new(latin, Arc::new(LATIN.to_vec()), 0).unwrap());
    Fixture { set, jp, latin }
}
fn content(text: &str, fonts: Vec<AssetId>, vertical: bool) -> TextContent {
    TextContent {
        text: text.into(),
        style: TextStyle {
            fonts,
            size: 100.0,
            ..Default::default()
        },
        writing_mode: if vertical {
            WritingMode::VerticalRl
        } else {
            WritingMode::HorizontalTb
        },
        ..Default::default()
    }
}
/// Ink bounds of a placed glyph in node coordinates.
fn ink(f: &Fixture, g: &PlacedGlyph) -> [f64; 4] {
    let data = if g.glyph.font == Some(f.jp) {
        JP
    } else {
        LATIN
    };
    let face = ttf_parser::Face::parse(data, 0).unwrap();
    let b = face
        .glyph_bounding_box(ttf_parser::GlyphId(g.glyph.glyph))
        .unwrap();
    let t = g.transform;
    let pts = [
        (b.x_min, b.y_min),
        (b.x_max, b.y_min),
        (b.x_min, b.y_max),
        (b.x_max, b.y_max),
    ]
    .map(|(x, y)| {
        let (x, y) = (x as f64, y as f64);
        (t[0] * x + t[2] * y + t[4], t[1] * x + t[3] * y + t[5])
    });
    [
        pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min),
        pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min),
        pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max),
        pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max),
    ]
}
fn glyphs_of(l: &TextLayout, utf16: u32) -> Vec<&PlacedGlyph> {
    l.glyphs
        .iter()
        .filter(|g| {
            let c = &l.clusters[g.cluster];
            c.start <= utf16 && utf16 < c.end
        })
        .collect()
}
fn line_texts(c: &TextContent, l: &TextLayout) -> Vec<String> {
    let u: Vec<u16> = c.text.encode_utf16().collect();
    l.lines
        .iter()
        .map(|line| String::from_utf16(&u[line.start as usize..line.end as usize]).unwrap())
        .collect()
}

#[test]
fn font_inspection_reports_names_and_vertical_support() {
    let jp = inspect(JP, 0).unwrap();
    assert_eq!(jp.family, "Noto Sans JP");
    assert_eq!(jp.embedding, FontEmbedding::Installable);
    assert!(jp.has_vertical_metrics && jp.has_vertical_alternates);
    assert_eq!(jp.outline, "cff");
    let latin = inspect(LATIN, 0).unwrap();
    assert_eq!(latin.outline, "truetype");
    assert!(!latin.has_vertical_alternates);
    assert!(inspect(b"not a font", 0).is_err());
    assert!(inspect(JP, 1).is_err());
}

#[test]
fn restricted_fstype_is_reported() {
    let mut bytes = LATIN.to_vec();
    let tables = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    let os2 = (0..tables)
        .map(|i| 12 + i * 16)
        .find(|o| &bytes[*o..*o + 4] == b"OS/2")
        .unwrap();
    let offset = u32::from_be_bytes(bytes[os2 + 8..os2 + 12].try_into().unwrap()) as usize;
    bytes[offset + 8..offset + 10].copy_from_slice(&2u16.to_be_bytes());
    let info = inspect(&bytes, 0).unwrap();
    assert_eq!(info.embedding, FontEmbedding::Restricted);
    assert!(info.warnings()[0].contains("埋め込みが禁止"));
}

#[test]
fn horizontal_baseline_and_advances() {
    let f = fixture();
    let c = content("あいう", vec![f.jp], false);
    let l = layout(&c, &f.set);
    assert_eq!(l.clusters.len(), 3);
    for (i, cl) in l.clusters.iter().enumerate() {
        assert!((cl.inline[0] - 100.0 * i as f64).abs() < 1e-6);
        assert!((cl.inline[1] - cl.inline[0] - 100.0).abs() < 1e-6);
    }
    // line box 150, em box 100 centred: baseline = 25 + 88.
    assert!(
        (l.lines[0].baseline - 113.0).abs() < 1e-6,
        "{}",
        l.lines[0].baseline
    );
    assert_eq!(l.lines[0].block, [0.0, 150.0]);
    assert!(l.missing.is_empty());
}

#[test]
fn vertical_upright_glyphs_are_centred_in_the_column() {
    let f = fixture();
    let l = layout(&content("一二", vec![f.jp], true), &f.set);
    assert_eq!(l.lines[0].block, [-150.0, 0.0]);
    for (i, g) in l.glyphs.iter().enumerate() {
        let b = ink(&f, g);
        let cx = (b[0] + b[2]) / 2.0;
        assert!((cx + 75.0).abs() < 3.0, "column centre {cx}");
        assert!(
            b[1] >= 100.0 * i as f64 && b[3] <= 100.0 * (i + 1) as f64,
            "{b:?}"
        );
    }
}

fn vertical_punctuation(
    f: &Fixture,
    text: &str,
    extra: Vec<FontFeature>,
    force_fallback: bool,
) -> [f64; 4] {
    let mut set = f.set.clone();
    if force_fallback {
        let mut jp = set.get(&f.jp).unwrap().clone();
        jp.has_vert = false;
        set.insert(jp);
    }
    let mut c = content(text, vec![f.jp], true);
    c.style.features = extra;
    let l = layout(&c, &set);
    let g = glyphs_of(&l, 1);
    assert_eq!(g.len(), 1);
    let b = ink(f, g[0]);
    // Express relative to the second cell: x from the column's left edge, y from the cell top.
    [b[0] + 150.0, b[1] - 100.0, b[2] + 150.0, b[3] - 100.0]
}
fn off(v: Vec<&str>) -> Vec<FontFeature> {
    v.into_iter()
        .map(|t| FontFeature {
            tag: t.into(),
            value: 0,
        })
        .collect()
}

#[test]
fn vertical_kuten_and_touten_sit_in_the_upper_right() {
    let f = fixture();
    for p in ["あ、", "あ。"] {
        let b = vertical_punctuation(&f, p, vec![], false);
        // Column is 150 wide with a 100px em centred at 75: upper-right quadrant of the em box.
        assert!(b[0] > 75.0 && b[3] < 50.0, "{p}: {b:?}");
        // Without vertical alternates the fallback moves the horizontal form to the same place.
        let fallback = vertical_punctuation(&f, p, off(vec!["vert", "vrt2"]), true);
        for i in 0..4 {
            assert!(
                (fallback[i] - b[i]).abs() < 15.0,
                "{p} fallback {fallback:?} vs {b:?}"
            );
        }
        let horizontal = vertical_punctuation(&f, p, off(vec!["vert", "vrt2"]), false);
        assert!(
            horizontal[0] < 75.0 && horizontal[1] > 50.0,
            "control: {horizontal:?}"
        );
    }
}

#[test]
fn vertical_brackets_and_long_vowel_turn_with_the_column() {
    let f = fixture();
    let v = vertical_punctuation(&f, "あー", vec![], false);
    assert!(
        v[3] - v[1] > 3.0 * (v[2] - v[0]),
        "ー must be a vertical stroke: {v:?}"
    );
    let fallback = vertical_punctuation(&f, "あー", off(vec!["vert", "vrt2"]), true);
    assert!(
        fallback[3] - fallback[1] > 3.0 * (fallback[2] - fallback[0]),
        "rotated fallback {fallback:?}"
    );
    let bracket = vertical_punctuation(&f, "あ「", vec![], false);
    let fallback = vertical_punctuation(&f, "あ「", off(vec!["vert", "vrt2"]), true);
    for i in 0..4 {
        assert!(
            (fallback[i] - bracket[i]).abs() < 15.0,
            "「 fallback {fallback:?} vs {bracket:?}"
        );
    }
    // Vertical 「 sits at the foot of its cell, next to the text it opens.
    assert!(bracket[1] > 50.0 && bracket[2] > 100.0, "{bracket:?}");
}

#[test]
fn small_kana_fallback_matches_vertical_forms() {
    let f = fixture();
    for p in ["あっ", "あゃ", "あィ"] {
        let v = vertical_punctuation(&f, p, vec![], false);
        let fallback = vertical_punctuation(&f, p, off(vec!["vert", "vrt2"]), true);
        for i in 0..4 {
            assert!(
                (fallback[i] - v[i]).abs() < 8.0,
                "{p}: {fallback:?} vs {v:?}"
            );
        }
    }
}

#[test]
fn tate_chu_yoko_composes_digits_in_one_em() {
    let f = fixture();
    let c = content("第12回", vec![f.jp], true);
    let l = layout(&c, &f.set);
    assert_eq!(l.clusters.len(), 3);
    let tcy = &l.clusters[1];
    assert_eq!((tcy.start, tcy.end), (1, 3));
    assert!((tcy.inline[1] - tcy.inline[0] - 100.0).abs() < 1e-6);
    let g = glyphs_of(&l, 1);
    assert_eq!(g.len(), 2);
    let (a, b) = (ink(&f, g[0]), ink(&f, g[1]));
    assert!(a[2] <= b[0] + 1.0, "digits side by side: {a:?} {b:?}");
    let centre = (a[0] + b[2]) / 2.0;
    assert!((centre + 75.0).abs() < 4.0, "group centred: {centre}");
    for r in [a, b] {
        assert!(r[1] > 100.0 && r[3] < 200.0, "inside the cell: {r:?}");
        assert_eq!(g[0].transform[1], 0.0, "upright, not rotated");
    }
    // Three digits exceed auto_tcy=2 and rotate with the column instead.
    let l = layout(&content("第123回", vec![f.jp], true), &f.set);
    assert_eq!(l.clusters.len(), 5);
    let g = glyphs_of(&l, 2);
    assert!(g[0].transform[0] == 0.0 && g[0].transform[1] > 0.0);
    // Explicit ranges compress wider groups to one em.
    let mut c = content("記録2026年", vec![f.jp], true);
    c.tcy = vec![TextRange { start: 2, end: 6 }];
    let l = layout(&c, &f.set);
    let g = glyphs_of(&l, 2);
    assert_eq!(g.len(), 4);
    let first = ink(&f, g[0]);
    let last = ink(&f, g[3]);
    assert!(
        last[2] - first[0] <= 100.0 + 1e-6,
        "compressed to an em: {}",
        last[2] - first[0]
    );
    // Horizontal text ignores tate-chu-yoko.
    let mut c = content("第12回", vec![f.jp], false);
    c.tcy = vec![TextRange { start: 1, end: 3 }];
    assert_eq!(layout(&c, &f.set).clusters.len(), 4);
}

#[test]
fn latin_in_vertical_text_is_rotated_and_centred() {
    let f = fixture();
    let l = layout(&content("縦Ab", vec![f.latin, f.jp], true), &f.set);
    let g = glyphs_of(&l, 1)[0];
    assert_eq!(g.glyph.font, Some(f.latin));
    assert_eq!((g.transform[0], g.transform[3]), (0.0, 0.0));
    let a = ink(&f, g);
    assert!(a[3] - a[1] > 0.0 && (a[0] + a[2]) / 2.0 < 0.0);
    assert!(((a[0] + a[2]) / 2.0 + 75.0).abs() < 30.0, "{a:?}");
    assert!(a[1] >= 100.0 - 1.0, "follows the ideograph: {a:?}");
}

#[test]
fn kinsoku_keeps_punctuation_and_small_kana_off_line_starts() {
    let f = fixture();
    let mut c = content("あいう、えお", vec![f.jp], false);
    c.frame = TextFrame::Area {
        width: 300.0,
        height: 1000.0,
    };
    let l = layout(&c, &f.set);
    let lines = line_texts(&c, &l);
    assert!(lines.iter().all(|t| !t.starts_with('、')), "{lines:?}");
    assert_eq!(lines.concat(), c.text);
    for (text, banned) in [
        ("あいうっえお", 'っ'),
        ("あいうーえお", 'ー'),
        ("あいう」えお", '」'),
    ] {
        c.text = text.into();
        let lines = line_texts(&c, &layout(&c, &f.set));
        assert!(lines.iter().all(|t| !t.starts_with(banned)), "{lines:?}");
    }
    c.text = "あい「うえお".into();
    let lines = line_texts(&c, &layout(&c, &f.set));
    assert!(lines.iter().all(|t| !t.ends_with('「')), "{lines:?}");
    // Vertical area text wraps at the height and moves columns right to left.
    let mut c = content("あいうえおかきくけこ", vec![f.jp], true);
    c.frame = TextFrame::Area {
        width: 600.0,
        height: 400.0,
    };
    let l = layout(&c, &f.set);
    assert_eq!(l.lines.len(), 3);
    assert_eq!(l.lines[0].block, [450.0, 600.0]);
    assert!(l.lines[1].block[1] <= l.lines[0].block[0]);
}

#[test]
fn latin_words_wrap_at_spaces() {
    let f = fixture();
    let mut c = content("alpha beta gamma", vec![f.latin], false);
    c.style.size = 20.0;
    c.frame = TextFrame::Area {
        width: 120.0,
        height: 400.0,
    };
    let lines = line_texts(&c, &layout(&c, &f.set));
    assert_eq!(lines, vec!["alpha beta ", "gamma"]);
}

#[test]
fn fallback_chain_selects_fonts_per_grapheme_and_reports_missing() {
    let f = fixture();
    let l = layout(&content("Aあ😀", vec![f.latin, f.jp], false), &f.set);
    assert_eq!(glyphs_of(&l, 0)[0].glyph.font, Some(f.latin));
    assert_eq!(glyphs_of(&l, 1)[0].glyph.font, Some(f.jp));
    assert_eq!(l.fonts_used, vec![f.latin, f.jp]);
    assert_eq!(l.missing, vec![2]);
    // Without fonts the text is still measurable and drawn as placeholder boxes.
    let l = layout(&content("あい", vec![], false), &f.set);
    assert_eq!(l.glyphs.len(), 2);
    assert!(l.glyphs.iter().all(|g| g.glyph.font.is_none()));
    let paths = glyph_paths(&l, &f.set, &OutlineCache::default());
    assert_eq!(paths.len(), 1);
    alto_vector::validate_paths(&paths).unwrap();
}

#[test]
fn opentype_features_change_shaping() {
    let f = fixture();
    let width = |c: &TextContent| {
        let l = layout(c, &f.set);
        l.lines[0].inline[1] - l.lines[0].inline[0]
    };
    let mut c = content("「あ、い」", vec![f.jp], false);
    let plain = width(&c);
    c.style.features = vec![FontFeature {
        tag: "palt".into(),
        value: 1,
    }];
    assert!(width(&c) < plain - 50.0, "palt proportional widths");
    let mut c = content("12", vec![f.jp], false);
    let half = width(&c);
    c.style.features = vec![FontFeature {
        tag: "fwid".into(),
        value: 1,
    }];
    assert!((width(&c) - 200.0).abs() < 1e-6 && half < 150.0);
    let mut c = content("fi AV", vec![f.latin], false);
    assert_eq!(layout(&c, &f.set).glyphs.len(), 4, "fi ligature");
    let kerned = width(&c);
    c.style.features = off(vec!["liga", "kern"]);
    assert_eq!(layout(&c, &f.set).glyphs.len(), 5);
    assert!(width(&c) > kerned);
    // Span overrides apply features to part of the text.
    let mut c = content("あ、い、", vec![f.jp], false);
    c.spans = vec![TextSpan {
        start: 0,
        end: 2,
        style: StylePatch {
            features: Some(vec![FontFeature {
                tag: "palt".into(),
                value: 1,
            }]),
            color: Some([1.0, 0.0, 0.0, 1.0]),
            ..Default::default()
        },
    }];
    let l = layout(&c, &f.set);
    assert!(l.clusters[1].inline[1] - l.clusters[1].inline[0] < 90.0);
    assert!((l.clusters[3].inline[1] - l.clusters[3].inline[0] - 100.0).abs() < 1e-6);
    assert_eq!(glyphs_of(&l, 0)[0].color, [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(glyphs_of(&l, 2)[0].color, [0.0, 0.0, 0.0, 1.0]);
}

#[test]
fn bidi_runs_are_reordered_visually() {
    let f = fixture();
    let l = layout(&content("ab אבג cd", vec![f.latin], false), &f.set);
    let x = |o: u32| l.clusters.iter().find(|c| c.start == o).unwrap().inline[0];
    assert!(x(0) < x(1));
    assert!(x(3) > x(4) && x(4) > x(5), "Hebrew is right-to-left");
    assert!(x(5) > x(1) && x(7) > x(3), "runs keep their order");
    assert!(l.clusters.iter().find(|c| c.start == 4).unwrap().rtl);
}

#[test]
fn selection_hit_test_caret_and_rectangles() {
    let f = fixture();
    let c = content("あいう\nえお", vec![f.jp], false);
    let l = layout(&c, &f.set);
    assert_eq!(l.lines.len(), 2);
    assert_eq!(hit_test(&l, &c.text, [140.0, 60.0]), 1);
    assert_eq!(hit_test(&l, &c.text, [160.0, 60.0]), 2);
    assert_eq!(hit_test(&l, &c.text, [999.0, 60.0]), 3);
    assert_eq!(hit_test(&l, &c.text, [10.0, 200.0]), 4);
    let k = caret(&l, &c.text, 5).unwrap();
    assert_eq!((k.line, k.a), (1, [100.0, 150.0]));
    let rects = selection_rects(&l, &c.text, 1, 5);
    assert_eq!(
        rects,
        vec![[100.0, 0.0, 300.0, 150.0], [0.0, 150.0, 100.0, 300.0]]
    );
    assert_eq!(navigate(&l, &c.text, 1, Motion::NextLine), 5);
    assert_eq!(navigate(&l, &c.text, 5, Motion::PreviousLine), 1);
    assert_eq!(navigate(&l, &c.text, 2, Motion::LineEnd), 3);
    assert_eq!(navigate(&l, &c.text, 5, Motion::LineStart), 4);
    // UTF-16 offsets around a surrogate pair.
    let c = content("a𠮷b", vec![f.latin, f.jp], false);
    let l = layout(&c, &f.set);
    assert!(l.missing.is_empty());
    assert_eq!(navigate(&l, &c.text, 1, Motion::Forward), 3);
    assert_eq!(navigate(&l, &c.text, 3, Motion::Backward), 1);
    let mid = l.clusters[1].inline;
    assert_eq!(hit_test(&l, &c.text, [mid[1] - 1.0, 50.0]), 3);
    // Vertical: inline axis is y, lines progress right to left.
    let c = content("あいう\nえお", vec![f.jp], true);
    let l = layout(&c, &f.set);
    assert_eq!(hit_test(&l, &c.text, [-75.0, 160.0]), 2);
    assert_eq!(hit_test(&l, &c.text, [-225.0, 10.0]), 4);
    let rects = selection_rects(&l, &c.text, 0, 2);
    assert_eq!(rects, vec![[-150.0, 0.0, 0.0, 200.0]]);
    assert_eq!(navigate(&l, &c.text, 1, Motion::NextLine), 5);
}

#[test]
fn glyph_paths_cover_cff_and_truetype_outlines() {
    let f = fixture();
    let cache = OutlineCache::default();
    let mut c = content("縦組みAg", vec![f.latin, f.jp], true);
    c.spans = vec![TextSpan {
        start: 0,
        end: 1,
        style: StylePatch {
            color: Some([0.0, 0.0, 1.0, 1.0]),
            ..Default::default()
        },
    }];
    let l = layout(&c, &f.set);
    let paths = glyph_paths(&l, &f.set, &cache);
    assert_eq!(paths.len(), 2, "split by colour");
    alto_vector::validate_paths(&paths).unwrap();
    let all: Vec<_> = paths.iter().flat_map(|p| &p.data.commands).collect();
    assert!(
        all.iter()
            .any(|c| matches!(c, alto_vector::PathCommand::Cubic { .. }))
    );
    assert!(
        all.iter()
            .any(|c| matches!(c, alto_vector::PathCommand::Quad { .. }))
    );
    assert!(!cache.is_empty());
    // Long text is chunked below the per-path command limit.
    let c = content(&"吾輩は猫である。".repeat(100), vec![f.jp], false);
    let paths = glyph_paths(&layout(&c, &f.set), &f.set, &cache);
    assert!(paths.len() > 1 && paths.iter().all(|p| p.data.commands.len() <= 4000));
}
