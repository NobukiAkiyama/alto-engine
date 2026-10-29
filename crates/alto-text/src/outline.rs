//! Placed glyphs -> f64 vector paths for the shared lyon / 4x MSAA renderer, and
//! per-size glyph runs whose outlines the renderer's glyph atlas rasterizes directly.
use crate::font::{FontData, FontSet};
use crate::layout::{GlyphRef, TextLayout};
use alto_core::AssetId;
use alto_vector::{FillRule, GlyphOutline, PathCommand, PathData, StyledPath, TextGlyph, TextRun};
use rustybuzz::ttf_parser::{self, GlyphId, OutlineBuilder};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

/// Commands kept below the per-path limit of the vector schema.
const CHUNK: usize = 4000;
const LIMIT: f64 = 1_048_576.0;

struct Collect {
    cmds: Vec<PathCommand>,
    open: bool,
}
impl OutlineBuilder for Collect {
    fn move_to(&mut self, x: f32, y: f32) {
        self.open = true;
        let (x, y) = (x as f64, y as f64);
        self.cmds.push(PathCommand::Move { x, y });
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = (x as f64, y as f64);
        self.cmds.push(PathCommand::Line { x, y });
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.cmds.push(PathCommand::Quad {
            cx: x1 as f64,
            cy: y1 as f64,
            x: x as f64,
            y: y as f64,
        });
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.cmds.push(PathCommand::Cubic {
            c1x: x1 as f64,
            c1y: y1 as f64,
            c2x: x2 as f64,
            c2y: y2 as f64,
            x: x as f64,
            y: y as f64,
        });
    }
    fn close(&mut self) {
        if self.open {
            self.open = false;
            self.cmds.push(PathCommand::Close);
        }
    }
}

type Outlines = HashMap<(AssetId, u16), Arc<GlyphOutline>>;

fn key(font: Option<AssetId>, glyph: u16) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (font, glyph).hash(&mut h);
    h.finish()
}

/// Glyph outlines in font units, shared across layouts.
#[derive(Default)]
pub struct OutlineCache {
    map: Mutex<Outlines>,
    placeholder: std::sync::OnceLock<Arc<GlyphOutline>>,
}
impl OutlineCache {
    pub fn len(&self) -> usize {
        self.map.lock().map_or(0, |m| m.len())
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn outline<'f>(
        &self,
        fonts: &'f FontSet,
        faces: &mut HashMap<AssetId, Option<ttf_parser::Face<'f>>>,
        g: GlyphRef,
    ) -> Arc<GlyphOutline> {
        let Some(font) = g.font else {
            // Placeholder: a 1000-unit box with a hole (opposite winding for non-zero).
            return self
                .placeholder
                .get_or_init(|| {
                    let (a, b, c, d) = (80.0, 920.0, 0.0, 1000.0);
                    let (i0, i1, j0, j1) = (160.0, 840.0, 80.0, 920.0);
                    let l = |x, y| PathCommand::Line { x, y };
                    let cmds = vec![
                        PathCommand::Move { x: a, y: c },
                        l(b, c),
                        l(b, d),
                        l(a, d),
                        PathCommand::Close,
                        PathCommand::Move { x: i0, y: j0 },
                        l(i0, j1),
                        l(i1, j1),
                        l(i1, j0),
                        PathCommand::Close,
                    ];
                    Arc::new(GlyphOutline::new(
                        key(None, 0),
                        1000.0,
                        PathData { commands: cmds },
                    ))
                })
                .clone();
        };
        if let Some(hit) = self
            .map
            .lock()
            .ok()
            .and_then(|m| m.get(&(font, g.glyph)).cloned())
        {
            return hit;
        }
        let face = faces.entry(font).or_insert_with(|| {
            fonts
                .get(&font)
                .and_then(|f| ttf_parser::Face::parse(&f.bytes, f.face_index).ok())
        });
        let mut c = Collect {
            cmds: vec![],
            open: false,
        };
        let mut upem = 1000.0;
        if let Some(face) = face {
            upem = face.units_per_em() as f64;
            face.outline_glyph(GlyphId(g.glyph), &mut c);
        }
        if c.open {
            c.cmds.push(PathCommand::Close);
        }
        let raw = Arc::new(GlyphOutline::new(
            key(Some(font), g.glyph),
            upem,
            PathData { commands: c.cmds },
        ));
        if let Ok(mut m) = self.map.lock() {
            if m.len() > 65536 {
                m.clear();
            }
            m.insert((font, g.glyph), raw.clone());
        }
        raw
    }
}

/// One outline command in font units (y up), exactly as the font stores it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RawCommand {
    Move([f32; 2]),
    Line([f32; 2]),
    Quad([f32; 2], [f32; 2]),
    Cubic([f32; 2], [f32; 2], [f32; 2]),
    Close,
}

struct Raw {
    cmds: Vec<RawCommand>,
    open: bool,
}
impl OutlineBuilder for Raw {
    fn move_to(&mut self, x: f32, y: f32) {
        self.open = true;
        self.cmds.push(RawCommand::Move([x, y]));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.cmds.push(RawCommand::Line([x, y]));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.cmds.push(RawCommand::Quad([x1, y1], [x, y]));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.cmds
            .push(RawCommand::Cubic([x1, y1], [x2, y2], [x, y]));
    }
    fn close(&mut self) {
        if self.open {
            self.open = false;
            self.cmds.push(RawCommand::Close);
        }
    }
}

/// A glyph's units per em and outline, read from the font's own bytes and face
/// (not `OutlineCache`, whose key holds neither). `None` when the face does not
/// parse. Subpaths are closed as `glyph_paths` closes them.
pub fn raw_outline(font: &FontData, glyph: u16) -> Option<(u16, Vec<RawCommand>)> {
    let face = ttf_parser::Face::parse(&font.bytes, font.face_index).ok()?;
    let mut raw = Raw {
        cmds: vec![],
        open: false,
    };
    face.outline_glyph(GlyphId(glyph), &mut raw);
    if raw.open {
        raw.cmds.push(RawCommand::Close);
    }
    Some((face.units_per_em(), raw.cmds))
}

/// The glyph's commands in node coordinates, or None outside the schema's limits.
fn placed(glyph: &TextGlyph) -> Option<Vec<PathCommand>> {
    let (o, t) = (&glyph.outline, &glyph.transform);
    if o.is_empty() || o.data.commands.len() > CHUNK {
        return None;
    }
    let b = o.bounds;
    // Control points stay inside the transformed bounds' corners.
    let inside = [[b[0], b[1]], [b[2], b[1]], [b[0], b[3]], [b[2], b[3]]]
        .iter()
        .all(|p| {
            let x = t[0] * p[0] + t[2] * p[1] + t[4];
            let y = t[1] * p[0] + t[3] * p[1] + t[5];
            x.is_finite() && y.is_finite() && x.abs() <= LIMIT && y.abs() <= LIMIT
        });
    inside.then(|| glyph.to_path().data.commands)
}

/// Appends a glyph's commands, merging consecutive glyphs of one color into a path.
fn push(out: &mut Vec<StyledPath>, cmds: Vec<PathCommand>, color: alto_vector::Color) {
    match out.last_mut() {
        Some(p) if p.fill == Some(color) && p.data.commands.len() + cmds.len() <= CHUNK => {
            p.data.commands.extend(cmds)
        }
        _ => out.push(StyledPath {
            data: PathData { commands: cmds },
            fill: Some(color),
            stroke: None,
            fill_rule: FillRule::NonZero,
        }),
    }
}

/// Converts a layout to filled paths, grouped by color and chunked to the path limit.
pub fn glyph_paths(layout: &TextLayout, fonts: &FontSet, cache: &OutlineCache) -> Vec<StyledPath> {
    let mut faces = HashMap::new();
    let mut out: Vec<StyledPath> = vec![];
    for g in &layout.glyphs {
        let glyph = TextGlyph {
            outline: cache.outline(fonts, &mut faces, g.glyph),
            transform: g.transform,
            color: g.color,
        };
        if let Some(cmds) = placed(&glyph) {
            push(&mut out, cmds, g.color);
        }
    }
    out
}

/// Groups glyphs by font size (in order of first use) with both representations: the
/// paths for the vector renderer and the outlines for the glyph atlas.
pub fn glyph_runs(layout: &TextLayout, fonts: &FontSet, cache: &OutlineCache) -> Vec<TextRun> {
    let mut faces = HashMap::new();
    let mut runs: Vec<TextRun> = vec![];
    for g in &layout.glyphs {
        let glyph = TextGlyph {
            outline: cache.outline(fonts, &mut faces, g.glyph),
            transform: g.transform,
            color: g.color,
        };
        let Some(cmds) = placed(&glyph) else {
            continue;
        };
        let run = match runs.iter().position(|r| r.em == g.size) {
            Some(i) => &mut runs[i],
            None => {
                runs.push(TextRun {
                    em: g.size,
                    paths: vec![],
                    glyphs: vec![],
                });
                runs.last_mut().expect("pushed")
            }
        };
        push(&mut run.paths, cmds, g.color);
        run.glyphs.push(glyph);
    }
    runs
}
