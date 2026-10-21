//! Glyph outlines shared by the text layout and the renderer. Large glyphs render
//! through `TextRun::paths` (lyon / 4x MSAA); small ones through the size-banded
//! glyph atlas, which rasterizes `GlyphOutline`s directly at the display scale.
use crate::{Color, FillRule, PathCommand, PathData, StyledPath};
use std::sync::Arc;

/// One glyph's outline in font units (y up).
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphOutline {
    /// Stable identity of (font, glyph) for atlas keys.
    pub key: u64,
    pub upem: f64,
    pub data: PathData,
    /// Control-point bounds in font units [x0, y0, x1, y1] (conservative).
    pub bounds: [f64; 4],
}
impl GlyphOutline {
    pub fn new(key: u64, upem: f64, data: PathData) -> Self {
        let mut b = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        let mut add = |x: f64, y: f64| {
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        };
        for c in &data.commands {
            match *c {
                PathCommand::Move { x, y } | PathCommand::Line { x, y } => add(x, y),
                PathCommand::Quad { cx, cy, x, y } => {
                    add(cx, cy);
                    add(x, y);
                }
                PathCommand::Cubic {
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                } => {
                    add(c1x, c1y);
                    add(c2x, c2y);
                    add(x, y);
                }
                PathCommand::Close => {}
            }
        }
        if !b[0].is_finite() {
            b = [0.0; 4];
        }
        Self {
            key,
            upem,
            data,
            bounds: b,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.data.commands.is_empty()
    }
}

/// A placed glyph: font units (y up) to node coordinates (y down).
#[derive(Clone, Debug, PartialEq)]
pub struct TextGlyph {
    pub outline: Arc<GlyphOutline>,
    pub transform: [f64; 6],
    pub color: Color,
}

impl TextGlyph {
    /// This glyph alone as a filled path in node coordinates.
    pub fn to_path(&self) -> StyledPath {
        let t = &self.transform;
        let m = |x: f64, y: f64| (t[0] * x + t[2] * y + t[4], t[1] * x + t[3] * y + t[5]);
        let commands = self
            .outline
            .data
            .commands
            .iter()
            .map(|c| match *c {
                PathCommand::Move { x, y } => {
                    let (x, y) = m(x, y);
                    PathCommand::Move { x, y }
                }
                PathCommand::Line { x, y } => {
                    let (x, y) = m(x, y);
                    PathCommand::Line { x, y }
                }
                PathCommand::Quad { cx, cy, x, y } => {
                    let ((cx, cy), (x, y)) = (m(cx, cy), m(x, y));
                    PathCommand::Quad { cx, cy, x, y }
                }
                PathCommand::Cubic {
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                } => {
                    let ((c1x, c1y), (c2x, c2y), (x, y)) = (m(c1x, c1y), m(c2x, c2y), m(x, y));
                    PathCommand::Cubic {
                        c1x,
                        c1y,
                        c2x,
                        c2y,
                        x,
                        y,
                    }
                }
                PathCommand::Close => PathCommand::Close,
            })
            .collect();
        StyledPath {
            data: PathData { commands },
            fill: Some(self.color),
            stroke: None,
            fill_rule: FillRule::NonZero,
        }
    }
}

/// Glyphs sharing one font size. The renderer picks the vector paths or the atlas
/// per run from the run's em size on the display level being drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    /// Font size in node units.
    pub em: f64,
    /// The run's glyphs as filled paths (grouped by color, chunked to the path limit).
    pub paths: Vec<StyledPath>,
    pub glyphs: Vec<TextGlyph>,
}
