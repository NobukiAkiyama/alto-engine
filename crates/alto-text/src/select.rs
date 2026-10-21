//! Text selection geometry: hit testing, carets, selection rectangles and caret motion.
//! Positions are UTF-16 offsets; geometry is in node coordinates.
use crate::layout::{ClusterBox, LineBox, TextLayout};
use crate::model::{Utf16Index, WritingMode};
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Caret {
    pub offset: u32,
    pub line: usize,
    /// Segment across the line box, node coordinates.
    pub a: [f64; 2],
    pub b: [f64; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Motion {
    Forward,
    Backward,
    LineStart,
    LineEnd,
    NextLine,
    PreviousLine,
    WordForward,
    WordBackward,
    DocumentStart,
    DocumentEnd,
}

fn axes(l: &TextLayout, p: [f64; 2]) -> (f64, f64) {
    match l.writing_mode {
        WritingMode::HorizontalTb => (p[0], p[1]),
        WritingMode::VerticalRl => (p[1], p[0]),
    }
}
fn point(l: &TextLayout, inline: f64, block: f64) -> [f64; 2] {
    match l.writing_mode {
        WritingMode::HorizontalTb => [inline, block],
        WritingMode::VerticalRl => [block, inline],
    }
}

/// Grapheme boundaries (UTF-16) inside a cluster, for carets inside ligatures.
fn stops(text: &Utf16Index, src: &str, c: &ClusterBox) -> Vec<u32> {
    let (Some(a), Some(b)) = (text.byte(c.start), text.byte(c.end)) else {
        return vec![c.start, c.end];
    };
    let mut v: Vec<u32> = src[a..b]
        .grapheme_indices(true)
        .map(|(i, _)| text.utf16(a + i))
        .collect();
    v.push(c.end);
    v
}
/// Inline position of a UTF-16 offset that lies on a grapheme stop of a cluster.
fn position(c: &ClusterBox, stops: &[u32], offset: u32) -> f64 {
    let n = (stops.len() - 1).max(1) as f64;
    let i = stops.iter().position(|s| *s == offset).unwrap_or(0) as f64;
    let t = i / n;
    if c.rtl {
        c.inline[1] - t * (c.inline[1] - c.inline[0])
    } else {
        c.inline[0] + t * (c.inline[1] - c.inline[0])
    }
}

fn nearest_line(l: &TextLayout, block: f64) -> Option<usize> {
    l.lines
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let d = |x: &LineBox| {
                if block < x.block[0] {
                    x.block[0] - block
                } else if block > x.block[1] {
                    block - x.block[1]
                } else {
                    0.0
                }
            };
            d(a).total_cmp(&d(b))
        })
        .map(|(i, _)| i)
}

fn hit_in_line(l: &TextLayout, src: &str, idx: &Utf16Index, line: usize, inline: f64) -> u32 {
    let lb = &l.lines[line];
    let mut best = (f64::INFINITY, lb.start);
    for &ci in &lb.clusters {
        let c = &l.clusters[ci];
        let s = stops(idx, src, c);
        for &o in &s {
            let d = (position(c, &s, o) - inline).abs();
            if d < best.0 {
                best = (d, o);
            }
        }
    }
    best.1
}

pub fn hit_test(l: &TextLayout, src: &str, p: [f64; 2]) -> u32 {
    let (inline, block) = axes(l, p);
    let idx = Utf16Index::new(src);
    match nearest_line(l, block) {
        Some(line) => hit_in_line(l, src, &idx, line, inline),
        None => 0,
    }
}

/// Caret for an offset. At a soft line break the caret belongs to the following line.
pub fn caret(l: &TextLayout, src: &str, offset: u32) -> Option<Caret> {
    let idx = Utf16Index::new(src);
    let offset = offset.min(l.text_len);
    let line = l
        .lines
        .iter()
        .rposition(|lb| lb.start <= offset && offset <= lb.end)
        .or_else(|| l.lines.iter().rposition(|lb| lb.start <= offset))?;
    let lb = &l.lines[line];
    let mut at = None;
    for &ci in &lb.clusters {
        let c = &l.clusters[ci];
        if c.start <= offset && offset <= c.end {
            let s = stops(&idx, src, c);
            if s.contains(&offset) {
                let pos = position(c, &s, offset);
                // Prefer the cluster that starts at the offset (leading edge).
                if c.start == offset || at.is_none() {
                    at = Some(pos);
                }
            }
        }
    }
    let inline = at.unwrap_or(if lb.rtl { lb.inline[1] } else { lb.inline[0] });
    Some(Caret {
        offset,
        line,
        a: point(l, inline, lb.block[0]),
        b: point(l, inline, lb.block[1]),
    })
}

/// Rectangles [x0, y0, x1, y1] covering the UTF-16 range, merged per line.
pub fn selection_rects(l: &TextLayout, src: &str, start: u32, end: u32) -> Vec<[f64; 4]> {
    let (start, end) = (start.min(end), start.max(end));
    let idx = Utf16Index::new(src);
    let mut out = vec![];
    for lb in &l.lines {
        let mut spans: Vec<[f64; 2]> = vec![];
        for &ci in &lb.clusters {
            let c = &l.clusters[ci];
            if c.end <= start || c.start >= end {
                continue;
            }
            let s = stops(&idx, src, c);
            let a = s
                .iter()
                .copied()
                .filter(|o| *o <= start.max(c.start))
                .max()
                .unwrap_or(c.start);
            let b = s
                .iter()
                .copied()
                .filter(|o| *o >= end.min(c.end))
                .min()
                .unwrap_or(c.end);
            let (p, q) = (position(c, &s, a), position(c, &s, b));
            let span = [p.min(q), p.max(q)];
            match spans.last_mut() {
                Some(last) if (last[1] - span[0]).abs() < 1e-6 => last[1] = span[1],
                _ => spans.push(span),
            }
        }
        for s in spans {
            let a = point(l, s[0], lb.block[0]);
            let b = point(l, s[1], lb.block[1]);
            out.push([
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[0].max(b[0]),
                a[1].max(b[1]),
            ]);
        }
    }
    out
}

pub fn navigate(l: &TextLayout, src: &str, offset: u32, motion: Motion) -> u32 {
    let idx = Utf16Index::new(src);
    let offset = offset.min(idx.len_utf16());
    let byte = idx.byte(offset).unwrap_or(src.len());
    let caret_line = || caret(l, src, offset).map(|c| c.line);
    match motion {
        Motion::Forward => src[byte..]
            .grapheme_indices(true)
            .nth(1)
            .map_or(idx.len_utf16(), |(i, _)| idx.utf16(byte + i)),
        Motion::Backward => src[..byte]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| idx.utf16(i)),
        Motion::WordForward => src[byte..]
            .split_word_bound_indices()
            .find(|(i, w)| *i > 0 || !w.trim().is_empty())
            .map(|(i, w)| if i > 0 { i } else { w.len() })
            .map_or(idx.len_utf16(), |i| idx.utf16(byte + i)),
        Motion::WordBackward => src[..byte]
            .split_word_bound_indices()
            .rev()
            .find(|(_, w)| !w.trim().is_empty())
            .map_or(0, |(i, _)| idx.utf16(i)),
        Motion::LineStart => caret_line().map_or(0, |i| l.lines[i].start),
        Motion::LineEnd => caret_line().map_or(offset, |i| l.lines[i].end),
        Motion::DocumentStart => 0,
        Motion::DocumentEnd => idx.len_utf16(),
        Motion::NextLine | Motion::PreviousLine => {
            let Some(c) = caret(l, src, offset) else {
                return offset;
            };
            let target = if motion == Motion::NextLine {
                c.line + 1
            } else if c.line == 0 {
                return 0;
            } else {
                c.line - 1
            };
            if target >= l.lines.len() {
                return idx.len_utf16();
            }
            let (inline, _) = axes(l, c.a);
            hit_in_line(l, src, &idx, target, inline)
        }
    }
}
