//! M7: font assets, cached text layout, glyph paths for rendering, and selection
//! geometry in document coordinates.
use crate::{DocHandle, Engine, EngineError};
use alto_core::text_bake::{BakeFont, BakeGlyph, BakeOutline, TextBake};
use alto_core::{AssetId, ContentHash, NodeId};
use alto_doc::{AssetKind, AssetRecord, DocError, Document, NodeKind, Op};

type Assets = std::collections::BTreeMap<AssetId, AssetRecord>;
use alto_text::{
    FontData, FontEmbedding, FontInfo, FontSet, Motion, OutlineCache, TextContent, TextLayout,
};
use alto_vector::{StyledPath, TextRun};
use serde::Serialize;
use std::{cell::RefCell, collections::HashMap, sync::Arc};

pub struct TextEntry {
    pub layout: TextLayout,
    /// All glyphs as vector paths (the runs' paths in run order).
    pub paths: Arc<Vec<StyledPath>>,
    /// Glyphs grouped by font size, for the vector / glyph atlas choice per display level.
    pub runs: Arc<Vec<TextRun>>,
}

#[derive(Default)]
pub struct TextCache {
    layouts: RefCell<HashMap<ContentHash, Arc<TextEntry>>>,
    fonts: RefCell<HashMap<(ContentHash, u32), Option<FontData>>>,
    outlines: OutlineCache,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportedFont {
    pub record: AssetRecord,
    pub info: FontInfo,
    pub warnings: Vec<crate::Warning>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TextInfo {
    pub lines: Vec<LineInfo>,
    pub missing: Vec<u32>,
    pub fonts_used: Vec<AssetId>,
    pub glyphs: usize,
    pub paths: usize,
    /// Layout bounds in node coordinates [x0, y0, x1, y1].
    pub bounds: [f64; 4],
    /// The bounds' corners in document coordinates.
    pub quad: [[f64; 2]; 4],
    pub length: u32,
}
#[derive(Clone, Debug, Serialize)]
pub struct LineInfo {
    pub start: u32,
    pub end: u32,
    pub block: [f64; 2],
    pub baseline: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct CaretInfo {
    pub offset: u32,
    pub line: usize,
    pub a: [f64; 2],
    pub b: [f64; 2],
}

pub(crate) fn world(doc: &Document, id: NodeId) -> Result<[f64; 6], DocError> {
    let mut chain = vec![];
    let mut next = Some(id);
    while let Some(n) = next {
        let node = doc.node(n)?;
        chain.push(node.props.transform);
        next = node.parent;
    }
    Ok(chain
        .into_iter()
        .rev()
        .fold(alto_core::Affine2::default(), alto_core::Affine2::concat)
        .0)
}
fn apply(m: &[f64; 6], p: [f64; 2]) -> [f64; 2] {
    [
        m[0] * p[0] + m[2] * p[1] + m[4],
        m[1] * p[0] + m[3] * p[1] + m[5],
    ]
}

/// The baker's version, recorded in each bake (diagnostic only). Bump it, update
/// `tests/parity/text-bake.txt` and say why when the same input bakes to other
/// bytes (a dependency version, a rounding change).
pub const BAKER_VERSION: u16 = 1;
/// The vector schema's per-path command limit (`placed` in `alto-text`).
const BAKE_CHUNK: usize = 4000;
/// Transformed control-point bounds beyond this are skipped (`placed`).
const BAKE_LIMIT: f64 = 1_048_576.0;

/// A text colour channel in Q14: `floor(clamp(c, 0, 1) * 16384 + 1/2)`, 0 when not
/// finite (memo "文字色"). Edit side only.
pub fn text_color_q14(c: f32) -> u16 {
    if !c.is_finite() {
        return 0;
    }
    (f64::from(c).clamp(0.0, 1.0) * 16384.0).round() as u16
}

/// Font units in Q6, rounded half away from zero (`f64::round`).
fn q6(v: f32) -> Option<i32> {
    let q = (f64::from(v) * 64.0).round();
    (q.abs() <= f64::from(alto_core::text_bake::MAX_OUTLINE_COORD)).then_some(q as i32)
}

/// The placeholder `glyph_paths` draws for a missing glyph: a 1000-unit box with
/// a hole of opposite winding.
fn placeholder_outline() -> Vec<alto_text::RawCommand> {
    use alto_text::RawCommand::{Close, Line, Move};
    vec![
        Move([80.0, 0.0]),
        Line([920.0, 0.0]),
        Line([920.0, 1000.0]),
        Line([80.0, 1000.0]),
        Close,
        Move([160.0, 80.0]),
        Line([160.0, 920.0]),
        Line([840.0, 920.0]),
        Line([840.0, 80.0]),
        Close,
    ]
}

/// The outline in Q6, or `None` outside the bake's ranges.
fn bake_commands(raw: &[alto_text::RawCommand]) -> Option<Vec<alto_core::text_bake::BakeCommand>> {
    use alto_core::text_bake::BakeCommand as B;
    use alto_text::RawCommand as R;
    let p = |p: [f32; 2]| Some([q6(p[0])?, q6(p[1])?]);
    raw.iter()
        .map(|c| {
            Some(match *c {
                R::Move(a) => B::Move(p(a)?),
                R::Line(a) => B::Line(p(a)?),
                R::Quad(a, b) => B::Quad(p(a)?, p(b)?),
                R::Cubic(a, b, c) => B::Cubic(p(a)?, p(b)?, p(c)?),
                R::Close => B::Close,
            })
        })
        .collect()
}

/// Whether `placed` (`alto-text`) keeps the glyph: a non-empty outline within the
/// command limit whose control-point bounds stay finite and within the limit.
fn bake_placed(raw: &[alto_text::RawCommand], t: &[f64; 6]) -> bool {
    use alto_text::RawCommand as R;
    if raw.is_empty() || raw.len() > BAKE_CHUNK {
        return false;
    }
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut visit = |p: [f32; 2]| {
        let (x, y) = (f64::from(p[0]), f64::from(p[1]));
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    };
    for c in raw {
        match *c {
            R::Move(a) | R::Line(a) => visit(a),
            R::Quad(a, c) => {
                visit(a);
                visit(c);
            }
            R::Cubic(a, c, d) => {
                visit(a);
                visit(c);
                visit(d);
            }
            R::Close => {}
        }
    }
    if !b.iter().all(|v| v.is_finite()) {
        return false;
    }
    [[b[0], b[1]], [b[2], b[1]], [b[0], b[3]], [b[2], b[3]]]
        .iter()
        .all(|p| {
            let x = t[0] * p[0] + t[2] * p[1] + t[4];
            let y = t[1] * p[0] + t[3] * p[1] + t[5];
            x.is_finite() && y.is_finite() && x.abs() <= BAKE_LIMIT && y.abs() <= BAKE_LIMIT
        })
}

impl Engine {
    /// The render version 3 bake of `content` in `doc` (memo "焼き込み"): the
    /// layout's glyphs in drawing order with their Q6 outlines (deduplicated by
    /// font and glyph, in first-use order), Q24 / Q30 placements and Q14 colours.
    /// Only fonts that produced a kept glyph are listed.
    pub fn bake_text(&self, assets: &Assets, content: &TextContent) -> TextBake {
        let fonts = self.font_set(assets, content);
        let layout = alto_text::layout(content, &fonts);
        let mut bake = TextBake {
            baker: BAKER_VERSION,
            ..TextBake::default()
        };
        let mut font_index: HashMap<AssetId, u32> = HashMap::new();
        let mut outline_index: HashMap<(Option<AssetId>, u16), u32> = HashMap::new();
        for g in &layout.glyphs {
            let (upem, raw) = match g.glyph.font {
                None => (1000, placeholder_outline()),
                Some(id) => match fonts
                    .get(&id)
                    .and_then(|f| alto_text::raw_outline(f, g.glyph.glyph))
                {
                    Some(outline) => outline,
                    None => continue,
                },
            };
            if !(1..=16384).contains(&upem) || !bake_placed(&raw, &g.transform) {
                continue;
            }
            let q = |v: f64, bits: i32| (v * 2f64.powi(bits)).round();
            let m = [0, 1, 2, 3].map(|i| q(g.transform[i], 24));
            if m.iter().any(|v| v.abs() > f64::from(i32::MAX)) {
                continue;
            }
            let key = (g.glyph.font, g.glyph.glyph);
            let outline = match outline_index.get(&key) {
                Some(&index) => index,
                None => {
                    let Some(commands) = bake_commands(&raw) else {
                        continue;
                    };
                    let font = match g.glyph.font {
                        None => alto_core::text_bake::PLACEHOLDER_FONT,
                        Some(id) => {
                            let Some(AssetRecord {
                                hash,
                                kind: AssetKind::Font { face_index, .. },
                                ..
                            }) = assets.get(&id)
                            else {
                                continue;
                            };
                            *font_index.entry(id).or_insert_with(|| {
                                bake.fonts.push(BakeFont {
                                    hash: *hash,
                                    face_index: *face_index,
                                });
                                bake.fonts.len() as u32 - 1
                            })
                        }
                    };
                    bake.outlines.push(BakeOutline {
                        font,
                        glyph: if g.glyph.font.is_some() {
                            g.glyph.glyph
                        } else {
                            0
                        },
                        upem,
                        commands,
                    });
                    let index = bake.outlines.len() as u32 - 1;
                    outline_index.insert(key, index);
                    index
                }
            };
            bake.glyphs.push(BakeGlyph {
                outline,
                m: m.map(|v| v as i32),
                t: [q(g.transform[4], 30) as i64, q(g.transform[5], 30) as i64],
                color: g.color.map(text_color_q14),
            });
        }
        bake
    }

    /// Fills in the bake of every text op of a render version 3 transaction that
    /// has none (`InsertNode` / `ReplaceNode` of a text node, `SetText`), baked
    /// against the asset table as the transaction leaves it at that op, and adds
    /// a `SetText` re-bake for each text node using a font that a `SetAsset`
    /// replaces (memo "字形の同一性"). A transaction that moves the document onto
    /// version 3 counts. Ops that already carry a bake are kept as they are:
    /// a bake is bytes, never recomputed from another peer's op. Returns the
    /// blobs the ops now reference, for the caller to store.
    pub(crate) fn bake_ops(&self, doc: &Document, ops: &mut Vec<Op>) -> Vec<Vec<u8>> {
        let v3 = |m: &alto_doc::DocumentMeta| m.render_version >= alto_core::RENDER_VERSION_EXACT;
        let mut exact = v3(&doc.meta);
        let mut assets = doc.assets.clone();
        let mut replaced = std::collections::BTreeSet::new();
        let mut texted = std::collections::BTreeSet::new();
        let mut blobs = vec![];
        for op in ops.iter_mut() {
            match op {
                Op::SetMeta { meta } => exact = v3(meta),
                Op::SetAsset { id, record } => {
                    let font = |r: Option<&AssetRecord>| {
                        r.and_then(|r| {
                            matches!(r.kind, AssetKind::Font { .. })
                                .then_some((r.hash, r.kind.clone()))
                        })
                    };
                    if font(assets.get(id)).is_some()
                        && font(assets.get(id)) != font(record.as_ref())
                    {
                        replaced.insert(*id);
                    }
                    match record {
                        Some(r) => assets.insert(*id, r.clone()),
                        None => assets.remove(id),
                    };
                }
                Op::SetText { id, content, bake } => {
                    texted.insert(*id);
                    if exact && bake.is_none() {
                        let bytes = self.bake_text(&assets, content).encode();
                        *bake = Some(ContentHash::of(&bytes));
                        blobs.push(bytes);
                    }
                }
                Op::InsertNode { node, .. } | Op::ReplaceNode { node, .. } => {
                    if let NodeKind::Text { content, bake } = &mut node.kind {
                        texted.insert(node.id);
                        if exact && bake.is_none() {
                            let bytes = self.bake_text(&assets, content).encode();
                            *bake = Some(ContentHash::of(&bytes));
                            blobs.push(bytes);
                        }
                    }
                }
                _ => {}
            }
        }
        if exact {
            for node in doc.nodes.values() {
                let NodeKind::Text { content, .. } = &node.kind else {
                    continue;
                };
                if texted.contains(&node.id)
                    || !content.font_ids().iter().any(|f| replaced.contains(f))
                {
                    continue;
                }
                let bytes = self.bake_text(&assets, content).encode();
                ops.push(Op::SetText {
                    id: node.id,
                    content: content.clone(),
                    bake: Some(ContentHash::of(&bytes)),
                });
                blobs.push(bytes);
            }
        }
        blobs
    }

    /// Parses and stores a font file; the returned record is added to a document with
    /// a `set_asset` operation. A font whose fsType forbids embedding is refused
    /// before its bytes enter the package. Preview-and-print licences still warn.
    pub fn import_font(
        &mut self,
        bytes: Vec<u8>,
        face_index: u32,
    ) -> Result<ImportedFont, EngineError> {
        let info =
            alto_text::inspect(&bytes, face_index).map_err(|e| DocError::Invalid(e.to_string()))?;
        if info.embedding == FontEmbedding::Restricted {
            return Err(DocError::Invalid(format!(
                "INVALID_ARGUMENT: the licence of font {} (OS/2 fsType) forbids embedding it",
                info.family
            ))
            .into());
        }
        let byte_len = bytes.len() as u64;
        let hash = self.put_import_blob(bytes);
        // Restricted fonts were refused above.
        let mut warnings = vec![];
        if info.embedding == FontEmbedding::PreviewAndPrint {
            warnings.push(
                crate::Warning::new("FONT_EMBEDDING_RESTRICTED")
                    .with("family", &info.family)
                    .with("level", "preview_and_print"),
            );
        }
        if !info.subsetting_allowed {
            warnings.push(
                crate::Warning::new("FONT_SUBSETTING_FORBIDDEN").with("family", &info.family),
            );
        }
        Ok(ImportedFont {
            record: AssetRecord {
                id: AssetId::new(),
                hash,
                kind: AssetKind::Font {
                    family: info.family.clone(),
                    face_index,
                    embedding: info.embedding,
                },
                byte_len,
                dims: None,
            },
            info,
            warnings,
        })
    }

    fn font_set(&self, assets: &Assets, content: &TextContent) -> FontSet {
        let mut set = FontSet::default();
        for id in content.font_ids() {
            let Some(AssetRecord {
                hash,
                kind: AssetKind::Font { face_index, .. },
                ..
            }) = assets.get(&id)
            else {
                continue;
            };
            let key = (*hash, *face_index);
            let mut fonts = self.text.fonts.borrow_mut();
            let data = fonts.entry(key).or_insert_with(|| {
                self.blobs
                    .get(hash)
                    .and_then(|b| FontData::new(id, Arc::new(b.clone()), *face_index).ok())
            });
            if let Some(d) = data {
                let mut d = d.clone();
                d.id = id;
                set.insert(d);
            }
        }
        set
    }

    pub(crate) fn text_entry(&self, doc: &Document, content: &TextContent) -> Arc<TextEntry> {
        let mut key = serde_json::to_vec(content).unwrap_or_default();
        for id in content.font_ids() {
            if let Some(a) = doc.assets.get(&id) {
                key.extend_from_slice(&a.hash.0);
            }
        }
        let key = ContentHash::of(&key);
        if let Some(hit) = self.text.layouts.borrow().get(&key) {
            return hit.clone();
        }
        let fonts = self.font_set(&doc.assets, content);
        let layout = alto_text::layout(content, &fonts);
        let runs = alto_text::glyph_runs(&layout, &fonts, &self.text.outlines);
        let paths = Arc::new(runs.iter().flat_map(|r| r.paths.clone()).collect());
        let entry = Arc::new(TextEntry {
            layout,
            paths,
            runs: Arc::new(runs),
        });
        let mut cache = self.text.layouts.borrow_mut();
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(key, entry.clone());
        entry
    }

    fn text_node(
        &self,
        h: DocHandle,
        id: NodeId,
    ) -> Result<(&Document, &TextContent), EngineError> {
        let doc = self.document(h)?;
        match &doc.node(id)?.kind {
            NodeKind::Text { content, .. } => Ok((doc, content)),
            _ => Err(DocError::Invalid("text layer required".into()).into()),
        }
    }

    pub fn text_info(&self, h: DocHandle, id: NodeId) -> Result<TextInfo, EngineError> {
        let (doc, content) = self.text_node(h, id)?;
        let e = self.text_entry(doc, content);
        let m = world(doc, id)?;
        let b = e.layout.bounds;
        Ok(TextInfo {
            lines: e
                .layout
                .lines
                .iter()
                .map(|l| LineInfo {
                    start: l.start,
                    end: l.end,
                    block: l.block,
                    baseline: l.baseline,
                })
                .collect(),
            missing: e.layout.missing.clone(),
            fonts_used: e.layout.fonts_used.clone(),
            glyphs: e.layout.glyphs.len(),
            paths: e.paths.len(),
            bounds: b,
            quad: [[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]].map(|p| apply(&m, p)),
            length: e.layout.text_len,
        })
    }

    fn local(&self, doc: &Document, id: NodeId, p: [f64; 2]) -> Result<[f64; 2], EngineError> {
        let inv = alto_core::Affine2(world(doc, id)?)
            .inverse()
            .ok_or_else(|| DocError::Invalid("singular text transform".into()))?;
        Ok(apply(&inv.0, p))
    }

    /// UTF-16 offset nearest to a document point.
    pub fn text_hit_test(&self, h: DocHandle, id: NodeId, p: [f64; 2]) -> Result<u32, EngineError> {
        let (doc, content) = self.text_node(h, id)?;
        let local = self.local(doc, id, p)?;
        Ok(alto_text::hit_test(
            &self.text_entry(doc, content).layout,
            &content.text,
            local,
        ))
    }

    pub fn text_caret(
        &self,
        h: DocHandle,
        id: NodeId,
        offset: u32,
    ) -> Result<Option<CaretInfo>, EngineError> {
        let (doc, content) = self.text_node(h, id)?;
        let m = world(doc, id)?;
        Ok(
            alto_text::caret(&self.text_entry(doc, content).layout, &content.text, offset).map(
                |c| CaretInfo {
                    offset: c.offset,
                    line: c.line,
                    a: apply(&m, c.a),
                    b: apply(&m, c.b),
                },
            ),
        )
    }

    /// Selection highlight quads in document coordinates.
    pub fn text_selection(
        &self,
        h: DocHandle,
        id: NodeId,
        start: u32,
        end: u32,
    ) -> Result<Vec<[[f64; 2]; 4]>, EngineError> {
        let (doc, content) = self.text_node(h, id)?;
        let m = world(doc, id)?;
        Ok(alto_text::selection_rects(
            &self.text_entry(doc, content).layout,
            &content.text,
            start,
            end,
        )
        .into_iter()
        .map(|r| [[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]].map(|p| apply(&m, p)))
        .collect())
    }

    pub fn text_navigate(
        &self,
        h: DocHandle,
        id: NodeId,
        offset: u32,
        motion: Motion,
    ) -> Result<u32, EngineError> {
        let (doc, content) = self.text_node(h, id)?;
        Ok(alto_text::navigate(
            &self.text_entry(doc, content).layout,
            &content.text,
            offset,
            motion,
        ))
    }

    /// Licence warnings for fonts used by the document.
    pub fn font_warnings(&self, h: DocHandle) -> Result<Vec<crate::Warning>, EngineError> {
        let doc = self.document(h)?;
        let mut out = vec![];
        for a in doc.assets.values() {
            if let AssetKind::Font {
                family, embedding, ..
            } = &a.kind
            {
                match embedding {
                    alto_text::FontEmbedding::Restricted => out.push(
                        crate::Warning::new("FONT_EMBEDDING_RESTRICTED")
                            .with("family", family)
                            .with("level", "restricted"),
                    ),
                    alto_text::FontEmbedding::PreviewAndPrint => out.push(
                        crate::Warning::new("FONT_EMBEDDING_RESTRICTED")
                            .with("family", family)
                            .with("level", "preview_and_print"),
                    ),
                    _ => {}
                }
                if !self.blobs.contains_key(&a.hash) {
                    out.push(crate::Warning::new("FONT_MISSING").with("family", family));
                }
            }
        }
        Ok(out)
    }
}
