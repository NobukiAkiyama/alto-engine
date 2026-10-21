//! M7: font assets, cached text layout, glyph paths for rendering, and selection
//! geometry in document coordinates.
use crate::{DocHandle, Engine, EngineError};
use alto_core::{AssetId, ContentHash, NodeId};
use alto_doc::{AssetKind, AssetRecord, DocError, Document, NodeKind};
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
    pub warnings: Vec<String>,
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

impl Engine {
    /// Parses and stores a font file; the returned record is added to a document with
    /// a `set_asset` operation. A font whose fsType forbids embedding is refused
    /// before its bytes enter the package. Preview-and-print licences still warn.
    pub fn import_font(
        &mut self,
        bytes: Vec<u8>,
        face_index: u32,
    ) -> Result<ImportedFont, EngineError> {
        let info = alto_text::inspect(&bytes, face_index)
            .map_err(|e| DocError::Invalid(format!("INVALID_ARGUMENT: {e}")))?;
        if info.embedding == FontEmbedding::Restricted {
            return Err(DocError::Invalid(format!(
                "INVALID_ARGUMENT: フォント「{}」はライセンス（OS/2 fsType）で埋め込みが禁止されています",
                info.family
            ))
            .into());
        }
        let byte_len = bytes.len() as u64;
        let hash = self.put_blob(bytes);
        let warnings = info.warnings();
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

    fn font_set(&self, doc: &Document, content: &TextContent) -> FontSet {
        let mut set = FontSet::default();
        for id in content.font_ids() {
            let Some(AssetRecord {
                hash,
                kind: AssetKind::Font { face_index, .. },
                ..
            }) = doc.assets.get(&id)
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
        let fonts = self.font_set(doc, content);
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
            NodeKind::Text { content } => Ok((doc, content)),
            _ => Err(DocError::Invalid("INVALID_ARGUMENT: text layer required".into()).into()),
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
            .ok_or_else(|| DocError::Invalid("INVALID_ARGUMENT: singular text transform".into()))?;
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
    pub fn font_warnings(&self, h: DocHandle) -> Result<Vec<String>, EngineError> {
        let doc = self.document(h)?;
        let mut out = vec![];
        for a in doc.assets.values() {
            if let AssetKind::Font {
                family, embedding, ..
            } = &a.kind
            {
                match embedding {
                    alto_text::FontEmbedding::Restricted => out.push(format!(
                        "フォント「{family}」は埋め込みが禁止されています（OS/2 fsType）"
                    )),
                    alto_text::FontEmbedding::PreviewAndPrint => out.push(format!(
                        "フォント「{family}」は閲覧・印刷用の埋め込みのみ許可されています"
                    )),
                    _ => {}
                }
                if !self.blobs.contains_key(&a.hash) {
                    out.push(format!("フォント「{family}」のデータがありません"));
                }
            }
        }
        Ok(out)
    }
}
