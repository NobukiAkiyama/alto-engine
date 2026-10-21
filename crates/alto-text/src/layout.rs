//! Paragraph layout: itemization (style, font fallback, bidi level, vertical orientation,
//! tate-chu-yoko), rustybuzz shaping, UAX #14 line breaking with Japanese kinsoku,
//! bidi reordering, alignment and glyph placement in node coordinates.
use crate::font::{FontData, FontSet};
use crate::model::{Color, TextAlign, TextContent, TextFrame, TextStyle, Utf16Index, WritingMode};
use crate::orient::{Orientation, VerticalFallback, auto_tcy_char, orientation, vertical_fallback};
use alto_core::AssetId;
use rustybuzz::ttf_parser::Tag;
use serde::Serialize;
use std::{collections::HashMap, str::FromStr};
use unicode_segmentation::UnicodeSegmentation;

/// A glyph reference. `font: None` is a placeholder box drawn when no font is available.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct GlyphRef {
    pub font: Option<AssetId>,
    pub glyph: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlacedGlyph {
    pub glyph: GlyphRef,
    /// Font units (y up) to node coordinates (y down).
    pub transform: [f64; 6],
    pub color: Color,
    pub cluster: usize,
    /// Font size in node units (the em the renderer measures on screen).
    pub size: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClusterBox {
    /// UTF-16 range of the source text.
    pub start: u32,
    pub end: u32,
    pub line: usize,
    /// Visual extent along the inline axis (x for horizontal, y for vertical).
    pub inline: [f64; 2],
    pub rtl: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LineBox {
    pub start: u32,
    pub end: u32,
    pub paragraph: usize,
    /// Extent across lines: y top..bottom (horizontal) or x left..right (vertical).
    pub block: [f64; 2],
    /// Baseline y (horizontal) or central line x (vertical).
    pub baseline: f64,
    pub inline: [f64; 2],
    /// Clusters in visual order.
    pub clusters: Vec<usize>,
    pub rtl: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TextLayout {
    pub writing_mode: WritingMode,
    pub lines: Vec<LineBox>,
    pub clusters: Vec<ClusterBox>,
    pub glyphs: Vec<PlacedGlyph>,
    pub bounds: [f64; 4],
    /// UTF-16 offsets of characters that no font in the fallback chain could render.
    pub missing: Vec<u32>,
    /// Fonts that produced glyphs, in first-use order.
    pub fonts_used: Vec<AssetId>,
    pub text_len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Horizontal,
    Upright,
    Sideways,
    Tcy(usize),
}

#[derive(Clone, Debug)]
struct Piece {
    glyph: GlyphRef,
    /// Offset from the cluster's visual start along the shaping direction, node units.
    pen: f64,
    x_off: f64,
    y_off: f64,
    fallback: VerticalFallback,
}

#[derive(Clone, Debug)]
struct Cluster {
    start: usize,
    end: usize,
    advance: f64,
    pieces: Vec<Piece>,
    style: usize,
    level: u8,
    mode: Mode,
    font: Option<AssetId>,
    allow_break: bool,
    space: bool,
    /// Horizontal shaped width for tate-chu-yoko composition.
    tcy_width: f64,
}

struct Itemizer<'a> {
    fonts: &'a FontSet,
    faces: HashMap<AssetId, rustybuzz::Face<'a>>,
    styles: Vec<TextStyle>,
    /// (byte start, byte end, style index) for spans.
    span_bytes: Vec<(usize, usize, usize)>,
    /// (byte start, byte end) of tate-chu-yoko groups.
    tcy: Vec<(usize, usize)>,
    missing: Vec<usize>,
}

impl<'a> Itemizer<'a> {
    fn style_at(&self, byte: usize) -> usize {
        self.span_bytes
            .iter()
            .find(|(s, e, _)| *s <= byte && byte < *e)
            .map_or(0, |s| s.2)
    }
    fn tcy_at(&self, byte: usize) -> Option<usize> {
        self.tcy.iter().position(|(s, e)| *s <= byte && byte < *e)
    }
    fn font_for(&self, style: &TextStyle, g: &str) -> (Option<AssetId>, bool) {
        let needed = |c: char| {
            !matches!(c as u32, 0x200C | 0x200D | 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
                && !c.is_control()
        };
        for id in &style.fonts {
            if let Some(face) = self.faces.get(id)
                && g.chars()
                    .filter(|c| needed(*c))
                    .all(|c| face.glyph_index(c).is_some())
            {
                return (Some(*id), false);
            }
        }
        let first = style
            .fonts
            .iter()
            .find(|id| self.faces.contains_key(id))
            .copied();
        (first, g.chars().any(|c| needed(c) && !c.is_whitespace()))
    }
}

fn features(style: &TextStyle) -> Vec<rustybuzz::Feature> {
    style
        .features
        .iter()
        .filter_map(|f| {
            let b: [u8; 4] = f.tag.as_bytes().try_into().ok()?;
            Some(rustybuzz::Feature::new(Tag::from_bytes(&b), f.value, ..))
        })
        .collect()
}

/// Lays out validated text content. Unavailable fonts fall back to placeholder boxes.
pub fn layout(content: &TextContent, fonts: &FontSet) -> TextLayout {
    let text = content.text.as_str();
    let index = Utf16Index::new(text);
    let vertical = content.writing_mode == WritingMode::VerticalRl;
    let mut it = Itemizer {
        fonts,
        faces: HashMap::new(),
        styles: vec![content.style.clone()],
        span_bytes: vec![],
        tcy: vec![],
        missing: vec![],
    };
    for id in content.font_ids() {
        if let Some(f) = fonts.get(&id)
            && let Some(face) = rustybuzz::Face::from_slice(&f.bytes, f.face_index)
        {
            it.faces.insert(id, face);
        }
    }
    for s in &content.spans {
        if let (Some(a), Some(b)) = (index.byte(s.start), index.byte(s.end)) {
            it.styles.push(content.style.patched(&s.style));
            it.span_bytes.push((a, b, it.styles.len() - 1));
        }
    }
    if vertical {
        for r in &content.tcy {
            if let (Some(a), Some(b)) = (index.byte(r.start), index.byte(r.end)) {
                it.tcy.push((a, b));
            }
        }
        if content.auto_tcy > 0 {
            let chars: Vec<(usize, char)> = text.char_indices().collect();
            let mut i = 0;
            while i < chars.len() {
                if !auto_tcy_char(chars[i].1) {
                    i += 1;
                    continue;
                }
                let mut j = i;
                while j < chars.len() && auto_tcy_char(chars[j].1) {
                    j += 1;
                }
                let (a, b) = (chars[i].0, chars.get(j).map_or(text.len(), |c| c.0));
                if j - i <= content.auto_tcy as usize
                    && !it.tcy.iter().any(|(s, e)| *s < b && a < *e)
                {
                    it.tcy.push((a, b));
                }
                i = j;
            }
            it.tcy.sort();
        }
    }

    let mut out = TextLayout {
        writing_mode: content.writing_mode,
        text_len: index.len_utf16(),
        ..Default::default()
    };
    let avail = match content.frame {
        TextFrame::Point => f64::INFINITY,
        TextFrame::Area { width, height } => {
            if vertical {
                height
            } else {
                width
            }
        }
    };
    // Block cursor: top edge (horizontal) or right edge (vertical) of the next line.
    let mut block = match (content.frame, vertical) {
        (TextFrame::Area { width, .. }, true) => width,
        _ => 0.0,
    };
    let mut para_start = 0usize;
    for (pi, para) in text.split('\n').enumerate() {
        let base = para_start;
        para_start += para.len() + 1;
        let clusters = shape_paragraph(&mut it, base, para, vertical);
        let para_rtl = !vertical
            && unicode_bidi::BidiInfo::new(para, None)
                .paragraphs
                .first()
                .is_some_and(|p| p.level.is_rtl());
        // Greedy line breaking at UAX #14 opportunities.
        let mut lines: Vec<std::ops::Range<usize>> = vec![];
        let mut start = 0;
        while start < clusters.len() {
            let mut width = 0.0;
            let mut end = clusters.len();
            let mut last_break = None;
            for (i, c) in clusters.iter().enumerate().skip(start) {
                if i > start && c.allow_break {
                    last_break = Some(i);
                }
                if i > start && !c.space && width + c.advance > avail + 1e-9 {
                    end = last_break.unwrap_or(i);
                    break;
                }
                width += c.advance;
            }
            lines.push(start..end);
            start = end;
        }
        if lines.is_empty() {
            lines.push(0..0);
        }
        let last_line = lines.len() - 1;
        for (li, range) in lines.into_iter().enumerate() {
            let line_clusters = &clusters[range.clone()];
            let base_style = &it.styles[line_clusters
                .first()
                .map_or_else(|| it.style_at(base), |c| c.style)];
            let mut extent = 0.0f64;
            let (mut asc, mut desc) = (0.0f64, 0.0f64);
            for c in line_clusters {
                let st = &it.styles[c.style];
                extent = extent.max(st.size as f64 * st.line_height as f64);
                let (a, d) = metrics(c.font.and_then(|f| fonts.get(&f)), st.size as f64);
                asc = asc.max(a);
                desc = desc.max(d);
            }
            if line_clusters.is_empty() {
                let (a, d) = metrics(
                    base_style.fonts.iter().find_map(|f| fonts.get(f)),
                    base_style.size as f64,
                );
                asc = a;
                desc = d;
                extent = extent.max(base_style.size as f64 * base_style.line_height as f64);
            }
            let thickness = extent.max(asc + desc);
            let (block_range, baseline) = if vertical {
                let r = [block - thickness, block];
                block -= thickness;
                (r, (r[0] + r[1]) / 2.0)
            } else {
                let r = [block, block + thickness];
                block += thickness;
                (r, r[0] + (thickness - asc - desc) / 2.0 + asc)
            };
            // Visual order: reverse runs from the highest level down to the lowest odd level.
            let trailing = line_clusters.iter().rev().take_while(|c| c.space).count();
            let para_level = u8::from(para_rtl);
            let levels: Vec<u8> = line_clusters
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    if i >= line_clusters.len() - trailing {
                        para_level
                    } else {
                        c.level
                    }
                })
                .collect();
            let mut order: Vec<usize> = (0..line_clusters.len()).collect();
            if let (Some(&max), Some(&min)) = (levels.iter().max(), levels.iter().min()) {
                let lowest_odd = min | 1;
                let mut level = max;
                while level >= lowest_odd && level > 0 {
                    let mut i = 0;
                    while i < order.len() {
                        if levels[order[i]] >= level {
                            let mut j = i;
                            while j < order.len() && levels[order[j]] >= level {
                                j += 1;
                            }
                            order[i..j].reverse();
                            i = j;
                        } else {
                            i += 1;
                        }
                    }
                    level -= 1;
                }
            }
            let measured: f64 = line_clusters[..line_clusters.len() - trailing]
                .iter()
                .map(|c| c.advance)
                .sum();
            let mut align = content.align;
            let last_of_para = li == last_line;
            if align == TextAlign::Justify && (last_of_para || avail.is_infinite()) {
                align = TextAlign::Start;
            }
            if para_rtl {
                align = match align {
                    TextAlign::Start => TextAlign::End,
                    TextAlign::End => TextAlign::Start,
                    a => a,
                };
            }
            let room = if avail.is_finite() { avail } else { 0.0 };
            let mut pen = match align {
                TextAlign::Start | TextAlign::Justify => 0.0,
                TextAlign::Center => (room - measured) / 2.0,
                TextAlign::End => room - measured,
            };
            let gaps = line_clusters.len().saturating_sub(trailing + 1);
            let extra = if align == TextAlign::Justify && gaps > 0 && room > measured {
                (room - measured) / gaps as f64
            } else {
                0.0
            };
            let line_index = out.lines.len();
            let mut visual = vec![];
            let inline_start = pen;
            for (k, &ci) in order.iter().enumerate() {
                let c = &line_clusters[ci];
                let id = out.clusters.len();
                visual.push(id);
                out.clusters.push(ClusterBox {
                    start: index.utf16(base + c.start),
                    end: index.utf16(base + c.end),
                    line: line_index,
                    inline: [pen, pen + c.advance],
                    rtl: c.level % 2 == 1,
                });
                place(&mut out, &it, c, pen, baseline, id, fonts);
                pen += c.advance;
                if k + 1 < order.len() && ci < line_clusters.len() - trailing {
                    pen += extra;
                }
            }
            let (ls, le) = if line_clusters.is_empty() {
                let u = index.utf16(base);
                (u, u)
            } else {
                (
                    index.utf16(base + line_clusters[0].start),
                    index.utf16(base + line_clusters[line_clusters.len() - 1].end),
                )
            };
            out.lines.push(LineBox {
                start: ls,
                end: le,
                paragraph: pi,
                block: block_range,
                baseline,
                inline: [inline_start.min(pen), pen.max(inline_start)],
                clusters: visual,
                rtl: para_rtl,
            });
        }
    }
    out.missing = it.missing.iter().map(|b| index.utf16(*b)).collect();
    out.missing.sort();
    out.missing.dedup();
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for l in &out.lines {
        let r = if vertical {
            [l.block[0], l.inline[0], l.block[1], l.inline[1]]
        } else {
            [l.inline[0], l.block[0], l.inline[1], l.block[1]]
        };
        b = [
            b[0].min(r[0]),
            b[1].min(r[1]),
            b[2].max(r[2]),
            b[3].max(r[3]),
        ];
    }
    if let TextFrame::Area { width, height } = content.frame {
        b = [
            b[0].min(0.0),
            b[1].min(0.0),
            b[2].max(width),
            b[3].max(height),
        ];
    }
    out.bounds = if b[0].is_finite() { b } else { [0.0; 4] };
    out
}

fn metrics(font: Option<&FontData>, size: f64) -> (f64, f64) {
    match font {
        Some(f) => (f.ascent / f.upem * size, f.descent / f.upem * size),
        None => (0.88 * size, 0.12 * size),
    }
}

fn shape_paragraph(it: &mut Itemizer, base: usize, para: &str, vertical: bool) -> Vec<Cluster> {
    if para.is_empty() {
        return vec![];
    }
    let levels: Vec<u8> = if vertical {
        vec![0; para.len()]
    } else {
        unicode_bidi::BidiInfo::new(para, None)
            .levels
            .iter()
            .map(|l| l.number())
            .collect()
    };
    let mut breaks = vec![false; para.len() + 1];
    for (i, _) in unicode_linebreak::linebreaks(para) {
        breaks[i] = true;
    }
    // Japanese kinsoku (JIS X 4051 strict): UAX #14 already covers most closing
    // punctuation; small kana, prolonged sound marks and iteration marks are added here.
    let mut prev: Option<char> = None;
    for (i, c) in para.char_indices() {
        if no_line_start(c) || prev.is_some_and(no_line_end) {
            breaks[i] = false;
        }
        prev = Some(c);
    }
    // Itemize graphemes into runs with identical shaping parameters.
    #[derive(Clone, Copy, PartialEq)]
    struct Key {
        style: usize,
        font: Option<AssetId>,
        level: u8,
        mode: Mode,
    }
    let mut runs: Vec<(usize, usize, Key)> = vec![];
    for (g0, g) in para.grapheme_indices(true) {
        let abs = base + g0;
        let style = it.style_at(abs);
        let (font, missing) = it.font_for(&it.styles[style].clone(), g);
        if missing {
            it.missing.push(abs);
        }
        let c = g.chars().next().unwrap_or(' ');
        let has_vert = font
            .and_then(|f| it.fonts.get(&f))
            .is_some_and(|f| f.has_vert);
        let mode = if !vertical {
            Mode::Horizontal
        } else if let Some(t) = it.tcy_at(abs) {
            Mode::Tcy(t)
        } else {
            match orientation(c) {
                Orientation::Upright | Orientation::TransformedUpright => Mode::Upright,
                Orientation::TransformedRotated if has_vert => Mode::Upright,
                _ => Mode::Sideways,
            }
        };
        let key = Key {
            style,
            font,
            level: levels[g0],
            mode,
        };
        match runs.last_mut() {
            Some(r) if r.2 == key && r.1 == g0 => r.1 = g0 + g.len(),
            _ => runs.push((g0, g0 + g.len(), key)),
        }
    }
    let mut clusters = vec![];
    for (s, e, key) in runs {
        let style = it.styles[key.style].clone();
        let size = style.size as f64;
        let k = style.scale as f64;
        let spacing = style.letter_spacing as f64 * size;
        let run_text = &para[s..e];
        let face = key.font.and_then(|f| it.faces.get(&f));
        let Some(face) = face else {
            // No usable font: one placeholder box per grapheme.
            for (g0, g) in run_text.grapheme_indices(true) {
                let space = g.chars().all(|c| c.is_whitespace() && c != '\u{3000}');
                let adv = if space { size * 0.3 } else { size };
                clusters.push(Cluster {
                    start: s + g0,
                    end: s + g0 + g.len(),
                    advance: adv * if vertical { 1.0 } else { k } + spacing,
                    pieces: if space {
                        vec![]
                    } else {
                        vec![Piece {
                            glyph: GlyphRef {
                                font: None,
                                glyph: 0,
                            },
                            pen: 0.0,
                            x_off: 0.0,
                            y_off: 0.0,
                            fallback: VerticalFallback::None,
                        }]
                    },
                    style: key.style,
                    level: key.level,
                    mode: if matches!(key.mode, Mode::Tcy(_)) {
                        Mode::Upright
                    } else {
                        key.mode
                    },
                    font: None,
                    allow_break: breaks[s + g0],
                    space,
                    tcy_width: 0.0,
                });
            }
            continue;
        };
        let font = key
            .font
            .and_then(|f| it.fonts.get(&f))
            .expect("face implies font");
        let s_units = size / font.upem;
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        for (b, c) in run_text.char_indices() {
            // Tabs shape as spaces; the advance is widened below.
            buffer.add(if c == '\t' { ' ' } else { c }, (s + b) as u32);
        }
        let rtl = key.level % 2 == 1;
        buffer.set_direction(match key.mode {
            Mode::Upright => rustybuzz::Direction::TopToBottom,
            _ if rtl => rustybuzz::Direction::RightToLeft,
            _ => rustybuzz::Direction::LeftToRight,
        });
        if let Some(lang) = style
            .language
            .as_deref()
            .and_then(|l| rustybuzz::Language::from_str(l).ok())
        {
            buffer.set_language(lang);
        }
        buffer.guess_segment_properties();
        let shaped = rustybuzz::shape(face, &features(&style), buffer);
        let infos = shaped.glyph_infos();
        let positions = shaped.glyph_positions();
        let upright = key.mode == Mode::Upright;
        // Pen positions in output (visual) order.
        let mut pen = 0.0;
        let mut pens = Vec::with_capacity(infos.len());
        for p in positions {
            pens.push(pen);
            pen += if upright {
                -p.y_advance as f64 * s_units
            } else {
                p.x_advance as f64 * s_units * k
            };
        }
        let total = pen;
        let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
        for (i, info) in infos.iter().enumerate() {
            groups.entry(info.cluster as usize).or_default().push(i);
        }
        let starts: Vec<usize> = groups.keys().copied().collect();
        let make_piece = |i: usize, origin: f64| {
            let info = &infos[i];
            let p = &positions[i];
            let ch = para[info.cluster as usize..].chars().next().unwrap_or(' ');
            Piece {
                glyph: GlyphRef {
                    font: key.font,
                    glyph: info.glyph_id as u16,
                },
                pen: pens[i] - origin,
                x_off: p.x_offset as f64,
                y_off: p.y_offset as f64,
                fallback: if upright && !font.has_vert {
                    vertical_fallback(ch)
                } else {
                    VerticalFallback::None
                },
            }
        };
        if let Mode::Tcy(_) = key.mode {
            clusters.push(Cluster {
                start: s,
                end: e,
                advance: size + spacing,
                pieces: (0..infos.len()).map(|i| make_piece(i, 0.0)).collect(),
                style: key.style,
                level: 0,
                mode: key.mode,
                font: key.font,
                allow_break: breaks[s],
                space: false,
                tcy_width: total,
            });
            continue;
        }
        for (n, &cs) in starts.iter().enumerate() {
            let ce = starts.get(n + 1).copied().unwrap_or(e);
            let members = &groups[&cs];
            let lo = members
                .iter()
                .map(|&i| pens[i])
                .fold(f64::INFINITY, f64::min);
            let hi = members
                .iter()
                .map(|&i| {
                    pens[i]
                        + if upright {
                            -positions[i].y_advance as f64 * s_units
                        } else {
                            positions[i].x_advance as f64 * s_units * k
                        }
                })
                .fold(f64::NEG_INFINITY, f64::max);
            let text = &para[cs..ce];
            let space = text.chars().all(|c| c.is_whitespace() && c != '\u{3000}');
            let tab = text.contains('\t');
            let missing_glyph = members.iter().any(|&i| infos[i].glyph_id == 0)
                && !text.chars().all(char::is_whitespace);
            if missing_glyph && !it.missing.contains(&(base + cs)) {
                it.missing.push(base + cs);
            }
            clusters.push(Cluster {
                start: cs,
                end: ce,
                advance: if tab { size * 2.0 } else { (hi - lo).max(0.0) } + spacing,
                pieces: members.iter().map(|&i| make_piece(i, lo)).collect(),
                style: key.style,
                level: key.level,
                mode: key.mode,
                font: key.font,
                allow_break: breaks[cs],
                space,
                tcy_width: 0.0,
            });
        }
    }
    // Rebase cluster ranges to the paragraph. Clusters arrive in logical order.
    clusters.sort_by_key(|c| c.start);
    clusters
}

fn place(
    out: &mut TextLayout,
    it: &Itemizer,
    c: &Cluster,
    pen: f64,
    baseline: f64,
    id: usize,
    fonts: &FontSet,
) {
    let style = &it.styles[c.style];
    let size = style.size as f64;
    let k = style.scale as f64;
    let font = c.font.and_then(|f| fonts.get(&f));
    if let Some(f) = c.font
        && !out.fonts_used.contains(&f)
    {
        out.fonts_used.push(f);
    }
    let s = font.map_or(size / 1000.0, |f| size / f.upem);
    let (asc, desc) = font.map_or((880.0, 120.0), |f| (f.ascent, f.descent));
    for p in &c.pieces {
        let transform = match (p.glyph.font, c.mode) {
            // Placeholder boxes are 1000-unit squares sitting on the baseline / in the cell.
            (None, Mode::Horizontal) => [s * k, 0.0, 0.0, -s, pen, baseline + desc * s],
            (None, _) => [s, 0.0, 0.0, -s, baseline - size / 2.0, pen + size],
            (_, Mode::Horizontal) => [
                s * k,
                0.0,
                0.0,
                -s,
                pen + p.pen + p.x_off * s * k,
                baseline - p.y_off * s,
            ],
            (_, Mode::Upright) => {
                let (dx, dy) = match p.fallback {
                    VerticalFallback::Punctuation => (0.6 * size, -0.6 * size),
                    VerticalFallback::SmallKana => (0.1 * size, -0.1 * size),
                    VerticalFallback::None => (0.0, 0.0),
                };
                [
                    s * k,
                    0.0,
                    0.0,
                    -s,
                    baseline + p.x_off * s * k + dx,
                    pen + p.pen - p.y_off * s + dy,
                ]
            }
            (_, Mode::Sideways) => {
                let e = baseline - (asc - desc) * s / 2.0;
                [
                    0.0,
                    s * k,
                    s,
                    0.0,
                    e + p.y_off * s,
                    pen + p.pen + p.x_off * s * k,
                ]
            }
            (_, Mode::Tcy(_)) => {
                let comp = if c.tcy_width > size {
                    size / c.tcy_width
                } else {
                    1.0
                };
                let x0 = baseline - c.tcy_width * comp / 2.0;
                let base_y = pen + size / 2.0 + (asc - desc) * s / 2.0;
                [
                    s * k * comp,
                    0.0,
                    0.0,
                    -s,
                    x0 + (p.pen + p.x_off * s * k) * comp,
                    base_y - p.y_off * s,
                ]
            }
        };
        out.glyphs.push(PlacedGlyph {
            glyph: p.glyph,
            transform,
            color: style.color,
            cluster: id,
            size,
        });
    }
}

/// 行頭禁則: characters that must not start a line.
fn no_line_start(c: char) -> bool {
    matches!(
        c,
        '、' | '。'
            | '，'
            | '．'
            | '・'
            | '：'
            | '；'
            | '？'
            | '！'
            | 'ー'
            | '‐'
            | '゠'
            | '–'
            | '〜'
            | '～'
            | '」'
            | '』'
            | '）'
            | '】'
            | '〉'
            | '》'
            | '〕'
            | '｝'
            | '］'
            | '〙'
            | '〗'
            | '｠'
            | '»'
            | '”'
            | '’'
            | 'ゝ'
            | 'ゞ'
            | 'ヽ'
            | 'ヾ'
            | '々'
            | '〻'
            | '…'
            | '‥'
            | ')'
            | ']'
            | '}'
            | ','
            | '.'
            | ':'
            | ';'
            | '?'
            | '!'
    ) || matches!(vertical_fallback(c), VerticalFallback::SmallKana)
}
/// 行末禁則: characters that must not end a line.
fn no_line_end(c: char) -> bool {
    matches!(
        c,
        '「' | '『'
            | '（'
            | '【'
            | '〈'
            | '《'
            | '〔'
            | '｛'
            | '［'
            | '〘'
            | '〖'
            | '｟'
            | '«'
            | '“'
            | '‘'
            | '('
            | '['
            | '{'
    )
}
