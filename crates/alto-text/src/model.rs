//! Canonical, editable text data. Offsets are UTF-16 code units so the SDK can use
//! JavaScript string indices directly; layout converts them to byte offsets.
use alto_core::AssetId;
use serde::{Deserialize, Serialize};

pub type Color = [f32; 4];

pub const MAX_TEXT_UTF16: usize = 16384;
pub const MAX_SPANS: usize = 1024;
pub const MAX_FONTS: usize = 16;
pub const MAX_FEATURES: usize = 64;
pub const MAX_TCY_UTF16: u32 = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritingMode {
    #[default]
    HorizontalTb,
    VerticalRl,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    #[default]
    Start,
    Center,
    End,
    Justify,
}

/// Point text grows from its origin without wrapping. Area text wraps at the inline
/// length (width for horizontal, height for vertical) and columns flow right to left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TextFrame {
    #[default]
    Point,
    Area {
        width: f64,
        height: f64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FontFeature {
    /// OpenType feature tag such as "palt", "liga" or "fwid".
    pub tag: String,
    pub value: u32,
}

fn one() -> f32 {
    1.0
}
fn line_height() -> f32 {
    1.5
}
fn black() -> Color {
    [0.0, 0.0, 0.0, 1.0]
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextStyle {
    /// Fallback order. The first font with every character of a grapheme is used.
    #[serde(default)]
    pub fonts: Vec<AssetId>,
    /// Font size in node pixels.
    pub size: f32,
    #[serde(default = "black")]
    pub color: Color,
    /// Line advance as a multiple of the size.
    #[serde(default = "line_height")]
    pub line_height: f32,
    /// Extra inline advance after each cluster, in em.
    #[serde(default)]
    pub letter_spacing: f32,
    /// Horizontal scale applied to glyphs (condensed / expanded type).
    #[serde(default = "one")]
    pub scale: f32,
    #[serde(default)]
    pub features: Vec<FontFeature>,
    /// BCP 47 language tag. "ja" selects Japanese locl forms.
    #[serde(default)]
    pub language: Option<String>,
}
impl Default for TextStyle {
    fn default() -> Self {
        Self {
            fonts: vec![],
            size: 32.0,
            color: black(),
            line_height: line_height(),
            letter_spacing: 0.0,
            scale: 1.0,
            features: vec![],
            language: Some("ja".into()),
        }
    }
}

/// Character-level overrides. Unset fields inherit the base style.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StylePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fonts: Option<Vec<AssetId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letter_spacing: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<FontFeature>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextSpan {
    pub start: u32,
    pub end: u32,
    pub style: StylePatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

fn auto_tcy() -> u8 {
    2
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextContent {
    /// Paragraphs are separated by "\n".
    pub text: String,
    #[serde(default)]
    pub style: TextStyle,
    /// Sorted, non-overlapping overrides.
    #[serde(default)]
    pub spans: Vec<TextSpan>,
    #[serde(default)]
    pub frame: TextFrame,
    #[serde(default)]
    pub writing_mode: WritingMode,
    #[serde(default)]
    pub align: TextAlign,
    /// Explicit tate-chu-yoko (horizontal-in-vertical) ranges.
    #[serde(default)]
    pub tcy: Vec<TextRange>,
    /// Isolated runs of up to this many ASCII digits become tate-chu-yoko in vertical text.
    #[serde(default = "auto_tcy")]
    pub auto_tcy: u8,
}
impl Default for TextContent {
    fn default() -> Self {
        Self {
            text: String::new(),
            style: TextStyle::default(),
            spans: vec![],
            frame: TextFrame::Point,
            writing_mode: WritingMode::HorizontalTb,
            align: TextAlign::Start,
            tcy: vec![],
            auto_tcy: auto_tcy(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("Invalid text: {0}")]
pub struct TextError(pub String);
fn invalid(s: impl Into<String>) -> TextError {
    TextError(s.into())
}

fn color_ok(c: &Color) -> bool {
    c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
}
fn size_ok(v: f32) -> bool {
    v.is_finite() && v > 0.0 && v <= 4096.0
}
fn spacing_ok(v: f32) -> bool {
    v.is_finite() && (-0.5..=10.0).contains(&v)
}
fn scale_ok(v: f32) -> bool {
    v.is_finite() && (0.1..=10.0).contains(&v)
}
fn features_ok(f: &[FontFeature]) -> Result<(), TextError> {
    if f.len() > MAX_FEATURES {
        return Err(invalid("at most 64 OpenType features"));
    }
    for feature in f {
        if feature.tag.len() != 4 || !feature.tag.bytes().all(|b| (0x20..0x7f).contains(&b)) {
            return Err(invalid(format!("feature tag {:?}", feature.tag)));
        }
        if feature.value > 65535 {
            return Err(invalid("feature value"));
        }
    }
    Ok(())
}
fn fonts_ok(f: &[AssetId]) -> Result<(), TextError> {
    if f.len() > MAX_FONTS {
        return Err(invalid("at most 16 fallback fonts"));
    }
    let mut seen = std::collections::BTreeSet::new();
    if !f.iter().all(|id| seen.insert(*id)) {
        return Err(invalid("duplicate fallback font"));
    }
    Ok(())
}
fn language_ok(l: &Option<String>) -> Result<(), TextError> {
    if let Some(l) = l
        && (l.is_empty()
            || l.len() > 35
            || !l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    {
        return Err(invalid("language tag"));
    }
    Ok(())
}

impl TextStyle {
    pub fn validate(&self) -> Result<(), TextError> {
        fonts_ok(&self.fonts)?;
        features_ok(&self.features)?;
        language_ok(&self.language)?;
        if !size_ok(self.size)
            || !color_ok(&self.color)
            || !self.line_height.is_finite()
            || !(0.5..=10.0).contains(&self.line_height)
            || !spacing_ok(self.letter_spacing)
            || !scale_ok(self.scale)
        {
            return Err(invalid("style values out of range"));
        }
        Ok(())
    }
    pub fn patched(&self, p: &StylePatch) -> Self {
        let mut s = self.clone();
        if let Some(v) = &p.fonts {
            s.fonts = v.clone();
        }
        if let Some(v) = p.size {
            s.size = v;
        }
        if let Some(v) = p.color {
            s.color = v;
        }
        if let Some(v) = p.letter_spacing {
            s.letter_spacing = v;
        }
        if let Some(v) = p.scale {
            s.scale = v;
        }
        if let Some(v) = &p.features {
            s.features = v.clone();
        }
        if let Some(v) = &p.language {
            s.language = Some(v.clone());
        }
        s
    }
}
impl StylePatch {
    pub fn validate(&self) -> Result<(), TextError> {
        if let Some(f) = &self.fonts {
            fonts_ok(f)?;
        }
        if let Some(f) = &self.features {
            features_ok(f)?;
        }
        language_ok(&self.language)?;
        if self.size.is_some_and(|v| !size_ok(v))
            || self.color.as_ref().is_some_and(|c| !color_ok(c))
            || self.letter_spacing.is_some_and(|v| !spacing_ok(v))
            || self.scale.is_some_and(|v| !scale_ok(v))
        {
            return Err(invalid("span style values out of range"));
        }
        Ok(())
    }
}

/// Maps UTF-16 offsets to byte offsets for a string.
pub struct Utf16Index {
    /// (utf16 offset, byte offset) for every char boundary, including the end.
    bounds: Vec<(u32, usize)>,
}
impl Utf16Index {
    pub fn new(text: &str) -> Self {
        let mut bounds = Vec::with_capacity(text.len() + 1);
        let mut u = 0u32;
        for (b, c) in text.char_indices() {
            bounds.push((u, b));
            u += c.len_utf16() as u32;
        }
        bounds.push((u, text.len()));
        Self { bounds }
    }
    pub fn len_utf16(&self) -> u32 {
        self.bounds.last().map_or(0, |b| b.0)
    }
    /// Byte offset for a UTF-16 offset that lies on a character boundary.
    pub fn byte(&self, u: u32) -> Option<usize> {
        self.bounds
            .binary_search_by_key(&u, |b| b.0)
            .ok()
            .map(|i| self.bounds[i].1)
    }
    pub fn utf16(&self, byte: usize) -> u32 {
        match self.bounds.binary_search_by_key(&byte, |b| b.1) {
            Ok(i) => self.bounds[i].0,
            Err(i) => self.bounds[i.saturating_sub(1)].0,
        }
    }
}

fn ranges_ok(ranges: &[(u32, u32)], index: &Utf16Index, what: &str) -> Result<(), TextError> {
    let mut last = 0;
    for (i, &(s, e)) in ranges.iter().enumerate() {
        if s >= e || (i > 0 && s < last) || index.byte(s).is_none() || index.byte(e).is_none() {
            return Err(invalid(format!(
                "{what} ranges must be sorted, non-empty, non-overlapping and on character boundaries"
            )));
        }
        last = e;
    }
    Ok(())
}

impl TextContent {
    pub fn validate(&self) -> Result<(), TextError> {
        let index = Utf16Index::new(&self.text);
        if index.len_utf16() as usize > MAX_TEXT_UTF16 {
            return Err(invalid("text exceeds 16384 UTF-16 units"));
        }
        if self.text.chars().any(|c| {
            (c.is_control() && c != '\n' && c != '\t') || c == '\u{2028}' || c == '\u{2029}'
        }) {
            return Err(invalid("control characters other than tab and newline"));
        }
        self.style.validate()?;
        if self.spans.len() > MAX_SPANS || self.tcy.len() > MAX_SPANS {
            return Err(invalid("at most 1024 spans and tate-chu-yoko ranges"));
        }
        let spans: Vec<_> = self.spans.iter().map(|s| (s.start, s.end)).collect();
        ranges_ok(&spans, &index, "span")?;
        for s in &self.spans {
            s.style.validate()?;
        }
        let tcy: Vec<_> = self.tcy.iter().map(|s| (s.start, s.end)).collect();
        ranges_ok(&tcy, &index, "tate-chu-yoko")?;
        for r in &self.tcy {
            let (Some(a), Some(b)) = (index.byte(r.start), index.byte(r.end)) else {
                unreachable!("checked above")
            };
            if r.end - r.start > MAX_TCY_UTF16 || self.text[a..b].contains('\n') {
                return Err(invalid(
                    "tate-chu-yoko ranges are limited to 8 UTF-16 units in one paragraph",
                ));
            }
        }
        if self.auto_tcy > 4 {
            return Err(invalid("auto tate-chu-yoko accepts at most 4 digits"));
        }
        if let TextFrame::Area { width, height } = self.frame
            && !(width.is_finite()
                && height.is_finite()
                && width > 0.0
                && height > 0.0
                && width <= 1_048_576.0
                && height <= 1_048_576.0)
        {
            return Err(invalid("frame size"));
        }
        Ok(())
    }
    /// All fonts referenced by the base style and spans.
    pub fn font_ids(&self) -> Vec<AssetId> {
        let mut out = self.style.fonts.clone();
        for s in &self.spans {
            if let Some(f) = &s.style.fonts {
                for id in f {
                    if !out.contains(id) {
                        out.push(*id);
                    }
                }
            }
        }
        out
    }
    pub fn utf16_len(&self) -> u32 {
        self.text.encode_utf16().count() as u32
    }
    /// Replaces a UTF-16 range, shifting or trimming spans and tate-chu-yoko ranges.
    pub fn replace_range(&mut self, start: u32, end: u32, insert: &str) -> Result<(), TextError> {
        let index = Utf16Index::new(&self.text);
        let (Some(a), Some(b)) = (index.byte(start), index.byte(end)) else {
            return Err(invalid("replacement range is not on character boundaries"));
        };
        if a > b {
            return Err(invalid("replacement range is reversed"));
        }
        let added = insert.encode_utf16().count() as i64;
        let removed = (end - start) as i64;
        // Text inserted at a boundary belongs to the span that ends there, not the one
        // that starts there, so typing continues the preceding style.
        let shift = |p: u32| -> u32 {
            if p < start {
                p
            } else if p >= end {
                (p as i64 - removed + added) as u32
            } else {
                start + added as u32
            }
        };
        self.text.replace_range(a..b, insert);
        for s in &mut self.spans {
            let ne = if s.end == start {
                s.end + added as u32
            } else {
                shift(s.end)
            };
            // Typing over a selection keeps the style of the replaced text.
            s.start = if s.start == start && removed > 0 {
                s.start
            } else {
                shift(s.start)
            };
            s.end = ne;
        }
        self.spans.retain(|s| s.start < s.end);
        // An edit inside a tate-chu-yoko group dissolves the group.
        self.tcy.retain(|r| {
            !(start < r.end && end > r.start) && !(start == end && start > r.start && start < r.end)
        });
        for r in &mut self.tcy {
            r.start = shift(r.start);
            r.end = shift(r.end);
        }
        self.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16_offsets_and_surrogates() {
        let i = Utf16Index::new("a𠮷野");
        assert_eq!(i.len_utf16(), 4);
        assert_eq!(i.byte(1), Some(1));
        assert_eq!(i.byte(2), None, "inside a surrogate pair");
        assert_eq!(i.byte(3), Some(5));
        assert_eq!(i.utf16(5), 3);
    }
    #[test]
    fn validation_rejects_bad_ranges_and_styles() {
        let mut c = TextContent {
            text: "縦中横12".into(),
            ..Default::default()
        };
        assert!(c.validate().is_ok());
        c.tcy = vec![TextRange { start: 3, end: 2 }];
        assert!(c.validate().is_err());
        c.tcy = vec![TextRange { start: 3, end: 5 }];
        assert!(c.validate().is_ok());
        c.style.size = -1.0;
        assert!(c.validate().is_err());
        c.style.size = 12.0;
        c.style.features = vec![FontFeature {
            tag: "toolong".into(),
            value: 1,
        }];
        assert!(c.validate().is_err());
        c.style.features.clear();
        c.text.push('\u{7}');
        assert!(c.validate().is_err());
    }
    #[test]
    fn replace_range_moves_spans() {
        let mut c = TextContent {
            text: "あいうえお".into(),
            spans: vec![TextSpan {
                start: 2,
                end: 4,
                style: StylePatch {
                    size: Some(10.0),
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        c.replace_range(0, 1, "かき").unwrap();
        assert_eq!(c.text, "かきいうえお");
        assert_eq!((c.spans[0].start, c.spans[0].end), (3, 5));
        c.replace_range(5, 5, "ん").unwrap();
        assert_eq!((c.spans[0].start, c.spans[0].end), (3, 6));
        c.replace_range(3, 4, "く").unwrap();
        assert_eq!(
            (c.spans[0].start, c.spans[0].end),
            (3, 6),
            "replacement keeps the style"
        );
        c.replace_range(2, 6, "").unwrap();
        assert!(c.spans.is_empty());
    }
}
