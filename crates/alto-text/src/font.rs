//! Font assets: parsing, licensing (OS/2 fsType) and the metrics layout needs.
use alto_core::AssetId;
use rustybuzz::ttf_parser::{self, Tag};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

pub const MAX_FONT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontEmbedding {
    /// fsType 0: may be embedded and installed.
    #[default]
    Installable,
    /// fsType bit 1: the vendor does not permit embedding.
    Restricted,
    PreviewAndPrint,
    Editable,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FontInfo {
    pub family: String,
    pub subfamily: String,
    pub face_index: u32,
    pub faces_in_file: u32,
    pub embedding: FontEmbedding,
    pub subsetting_allowed: bool,
    pub units_per_em: u16,
    pub glyph_count: u16,
    pub has_vertical_metrics: bool,
    pub has_vertical_alternates: bool,
    pub outline: String,
}
impl FontInfo {
    pub fn warnings(&self) -> Vec<String> {
        let mut w = vec![];
        match self.embedding {
            FontEmbedding::Restricted => w.push(format!(
                "フォント「{}」はライセンス（OS/2 fsType）で埋め込みが禁止されています。配布前に権利を確認してください",
                self.family
            )),
            FontEmbedding::PreviewAndPrint => w.push(format!(
                "フォント「{}」は閲覧・印刷用の埋め込みのみ許可されています",
                self.family
            )),
            _ => {}
        }
        if !self.subsetting_allowed {
            w.push(format!(
                "フォント「{}」はサブセット化が許可されていません",
                self.family
            ));
        }
        w
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("Invalid font: {0}")]
pub struct FontError(pub String);

fn name(face: &ttf_parser::Face, ids: &[u16]) -> Option<String> {
    for id in ids {
        let mut fallback = None;
        for n in face.names() {
            if n.name_id != *id {
                continue;
            }
            if let Some(s) = n.to_string() {
                // Prefer English names so file data stays readable across locales.
                if n.language_id == 0x0409 || n.platform_id == ttf_parser::PlatformId::Macintosh {
                    return Some(s);
                }
                fallback.get_or_insert(s);
            }
        }
        if fallback.is_some() {
            return fallback;
        }
    }
    None
}

pub fn has_feature(face: &ttf_parser::Face, tags: &[&[u8; 4]]) -> bool {
    let check = |t: Option<ttf_parser::opentype_layout::LayoutTable>| {
        t.is_some_and(|t| {
            t.features
                .into_iter()
                .any(|f| tags.iter().any(|tag| f.tag == Tag::from_bytes(tag)))
        })
    };
    check(face.tables().gsub)
}

pub fn inspect(data: &[u8], face_index: u32) -> Result<FontInfo, FontError> {
    if data.len() > MAX_FONT_BYTES {
        return Err(FontError("font exceeds 64 MiB".into()));
    }
    let faces = ttf_parser::fonts_in_collection(data).unwrap_or(1);
    if face_index >= faces {
        return Err(FontError("face index out of range".into()));
    }
    let face = ttf_parser::Face::parse(data, face_index).map_err(|e| FontError(e.to_string()))?;
    let (embedding, subsetting) = match face.tables().os2 {
        Some(os2) => (
            match os2.permissions() {
                Some(ttf_parser::Permissions::Restricted) => FontEmbedding::Restricted,
                Some(ttf_parser::Permissions::PreviewAndPrint) => FontEmbedding::PreviewAndPrint,
                Some(ttf_parser::Permissions::Editable) => FontEmbedding::Editable,
                _ => FontEmbedding::Installable,
            },
            os2.is_subsetting_allowed(),
        ),
        None => (FontEmbedding::Installable, true),
    };
    let t = face.tables();
    Ok(FontInfo {
        family: name(&face, &[16, 1]).unwrap_or_else(|| "Unknown".into()),
        subfamily: name(&face, &[17, 2]).unwrap_or_default(),
        face_index,
        faces_in_file: faces,
        embedding,
        subsetting_allowed: subsetting,
        units_per_em: face.units_per_em(),
        glyph_count: face.number_of_glyphs(),
        has_vertical_metrics: t.vmtx.is_some(),
        has_vertical_alternates: has_feature(&face, &[b"vert", b"vrt2"]),
        outline: if t.glyf.is_some() {
            "truetype"
        } else if t.cff.is_some() {
            "cff"
        } else if t.cff2.is_some() {
            "cff2"
        } else {
            "none"
        }
        .into(),
    })
}

/// Font bytes and metrics resolved from a document asset.
#[derive(Clone, Debug)]
pub struct FontData {
    pub id: AssetId,
    pub bytes: Arc<Vec<u8>>,
    pub face_index: u32,
    pub upem: f64,
    /// Ideographic em box / typographic ascent and descent (positive), font units.
    pub ascent: f64,
    pub descent: f64,
    pub has_vert: bool,
}
impl FontData {
    pub fn new(id: AssetId, bytes: Arc<Vec<u8>>, face_index: u32) -> Result<Self, FontError> {
        let face =
            ttf_parser::Face::parse(&bytes, face_index).map_err(|e| FontError(e.to_string()))?;
        let upem = face.units_per_em() as f64;
        let (ascent, descent) = match (face.typographic_ascender(), face.typographic_descender()) {
            (Some(a), Some(d)) if a > d => (a as f64, -d as f64),
            _ => (face.ascender() as f64, -(face.descender() as f64)),
        };
        let (ascent, descent) = if ascent + descent > 0.0 {
            (ascent, descent)
        } else {
            (upem * 0.88, upem * 0.12)
        };
        let has_vert = has_feature(&face, &[b"vert", b"vrt2"]);
        Ok(Self {
            id,
            bytes,
            face_index,
            upem,
            ascent,
            descent,
            has_vert,
        })
    }
}

/// Fonts available to a layout, keyed by document asset.
#[derive(Clone, Debug, Default)]
pub struct FontSet {
    pub fonts: HashMap<AssetId, FontData>,
}
impl FontSet {
    pub fn insert(&mut self, f: FontData) {
        self.fonts.insert(f.id, f);
    }
    pub fn get(&self, id: &AssetId) -> Option<&FontData> {
        self.fonts.get(id)
    }
}
