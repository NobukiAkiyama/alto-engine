use alto_core::{
    Affine2, AssetId, BitDepth, ColorSpace, ContentHash, MAX_ISOLATED_GROUPS, MAX_TREE_DEPTH,
    NodeId, PixelFormat, Rect, TILE, TileKey,
};
use alto_raster::TileMap;
use alto_text::TextContent;
use alto_vector::{ClipPath, StyledPath, VectorMask, validate_paths};

pub mod version;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub use version::{Commit, DiffItem, IntentSummary, diff, restore_ops};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentMeta {
    pub width: u32,
    pub height: u32,
    pub color: ColorSpace,
    pub depth: BitDepth,
    pub format_version: u32,
}
impl DocumentMeta {
    pub fn srgb_u8(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            color: ColorSpace::Srgb,
            depth: BitDepth::U8,
            format_version: 1,
        }
    }
    fn validate(&self) -> Result<(), DocError> {
        if self.width == 0 || self.height == 0 {
            return Err(DocError::Invalid(
                "Canvas dimensions must be positive".into(),
            ));
        }
        if self.format_version != 1 && self.format_version != 2 {
            return Err(DocError::Invalid("Unsupported document format".into()));
        }
        if self.color != ColorSpace::Srgb || self.depth != BitDepth::U8 {
            return Err(DocError::Invalid(
                "Only sRGB 8-bit documents are supported".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
    Add,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    PassThrough,
    Isolated,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerProps {
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
    pub blend: BlendMode,
    pub transform: Affine2,
    pub mask: Option<TileMap>,
    #[serde(default = "enabled")]
    pub mask_enabled: bool,
    #[serde(default = "enabled")]
    pub mask_linked: bool,
    #[serde(default)]
    pub mask_transform: Affine2,
    #[serde(default)]
    pub vector_mask: Option<Box<VectorMask>>,
    #[serde(default)]
    pub clip_path: Option<Box<ClipPath>>,
    pub clip_to_below: bool,
    pub effects: Vec<Effect>,
}
fn enabled() -> bool {
    true
}
impl Default for LayerProps {
    fn default() -> Self {
        Self {
            visible: true,
            locked: false,
            opacity: 1.0,
            blend: BlendMode::Normal,
            transform: Affine2::default(),
            mask: None,
            mask_enabled: true,
            mask_linked: true,
            mask_transform: Affine2::default(),
            vector_mask: None,
            clip_path: None,
            clip_to_below: false,
            effects: vec![],
        }
    }
}
impl LayerProps {
    fn validate(&self) -> Result<(), DocError> {
        if !self.opacity.is_finite()
            || !(0.0..=1.0).contains(&self.opacity)
            || !self.transform.is_finite()
            || !self.mask_transform.is_finite()
        {
            return Err(DocError::Invalid("Invalid opacity or transform".into()));
        }
        if let Some(mask) = &self.vector_mask {
            mask.validate()
                .map_err(|e| DocError::Invalid(e.to_string()))?;
        }
        if let Some(path) = &self.clip_path {
            path.validate()
                .map_err(|e| DocError::Invalid(e.to_string()))?;
        }
        if let Some(mask) = &self.mask {
            validate_mask(mask)?;
        }
        if self.effects.len() > 32 {
            return Err(DocError::Invalid("At most 32 effects per node".into()));
        }
        for effect in &self.effects {
            validate_effect(effect)?;
        }
        Ok(())
    }
}

pub fn validate_mask(map: &TileMap) -> Result<(), DocError> {
    if map.format != PixelFormat::R8 {
        return Err(DocError::Invalid("Masks and selection must use R8".into()));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CurveChannel {
    #[default]
    Master,
    Red,
    Green,
    Blue,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub x: f64,
    pub y: f64,
}

/// A layer or adjustment effect. The JSON shape is the document format:
/// `{"type":"gaussian_blur","radius":12}`. Unknown types do not parse. Fields
/// this version does not know are kept and written back, so a document that
/// opened before effects were typed still round-trips.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    GaussianBlur {
        radius: f64,
        selection: Option<TileMap>,
        extra: serde_json::Map<String, serde_json::Value>,
    },
    ColorAdjust {
        brightness: f64,
        contrast: f64,
        saturation: f64,
        selection: Option<TileMap>,
        extra: serde_json::Map<String, serde_json::Value>,
    },
    Levels {
        in_black: f64,
        in_white: f64,
        gamma: f64,
        out_black: f64,
        out_white: f64,
        selection: Option<TileMap>,
        extra: serde_json::Map<String, serde_json::Value>,
    },
    HueSaturation {
        hue: f64,
        saturation: f64,
        lightness: f64,
        selection: Option<TileMap>,
        extra: serde_json::Map<String, serde_json::Value>,
    },
    Invert {
        selection: Option<TileMap>,
        extra: serde_json::Map<String, serde_json::Value>,
    },
    Curves {
        channel: CurveChannel,
        points: Vec<CurvePoint>,
        selection: Option<TileMap>,
        extra: serde_json::Map<String, serde_json::Value>,
    },
}

fn effect_f64(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    default: f64,
) -> Result<f64, String> {
    let Some(value) = obj.remove(key) else {
        return Ok(default);
    };
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| format!("Invalid {key}"))
}
fn effect_selection(
    obj: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<Option<TileMap>, String> {
    match obj.remove("selection") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value).map_err(|e| e.to_string()),
    }
}
fn effect_from_map(mut obj: serde_json::Map<String, serde_json::Value>) -> Result<Effect, String> {
    let type_name = obj
        .remove("type")
        .and_then(|v| v.as_str().map(str::to_string))
        .ok_or_else(|| "effect type required".to_string())?;
    match type_name.as_str() {
        "gaussian_blur" => Ok(Effect::GaussianBlur {
            radius: effect_f64(&mut obj, "radius", 8.0)?,
            selection: effect_selection(&mut obj)?,
            extra: obj,
        }),
        "color_adjust" => Ok(Effect::ColorAdjust {
            brightness: effect_f64(&mut obj, "brightness", 0.0)?,
            contrast: effect_f64(&mut obj, "contrast", 1.0)?,
            saturation: effect_f64(&mut obj, "saturation", 1.0)?,
            selection: effect_selection(&mut obj)?,
            extra: obj,
        }),
        "levels" => Ok(Effect::Levels {
            in_black: effect_f64(&mut obj, "in_black", 0.0)?,
            in_white: effect_f64(&mut obj, "in_white", 1.0)?,
            gamma: effect_f64(&mut obj, "gamma", 1.0)?,
            out_black: effect_f64(&mut obj, "out_black", 0.0)?,
            out_white: effect_f64(&mut obj, "out_white", 1.0)?,
            selection: effect_selection(&mut obj)?,
            extra: obj,
        }),
        "hue_saturation" => Ok(Effect::HueSaturation {
            hue: effect_f64(&mut obj, "hue", 0.0)?,
            saturation: effect_f64(&mut obj, "saturation", 1.0)?,
            lightness: effect_f64(&mut obj, "lightness", 0.0)?,
            selection: effect_selection(&mut obj)?,
            extra: obj,
        }),
        "invert" => Ok(Effect::Invert {
            selection: effect_selection(&mut obj)?,
            extra: obj,
        }),
        "curves" => {
            let channel = match obj.remove("channel") {
                None => CurveChannel::Master,
                Some(value) => serde_json::from_value(value).map_err(|e| e.to_string())?,
            };
            let points = serde_json::from_value(
                obj.remove("points")
                    .ok_or_else(|| "Curve points required".to_string())?,
            )
            .map_err(|e| e.to_string())?;
            Ok(Effect::Curves {
                channel,
                points,
                selection: effect_selection(&mut obj)?,
                extra: obj,
            })
        }
        _ => Err(format!("unknown effect type {type_name}")),
    }
}
impl Serialize for Effect {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serde_json::Map::new();
        let (type_name, selection, extra) = match self {
            Self::GaussianBlur {
                radius,
                selection,
                extra,
            } => {
                map.insert("radius".into(), serde_json::json!(radius));
                ("gaussian_blur", selection, extra)
            }
            Self::ColorAdjust {
                brightness,
                contrast,
                saturation,
                selection,
                extra,
            } => {
                map.insert("brightness".into(), serde_json::json!(brightness));
                map.insert("contrast".into(), serde_json::json!(contrast));
                map.insert("saturation".into(), serde_json::json!(saturation));
                ("color_adjust", selection, extra)
            }
            Self::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
                selection,
                extra,
            } => {
                map.insert("in_black".into(), serde_json::json!(in_black));
                map.insert("in_white".into(), serde_json::json!(in_white));
                map.insert("gamma".into(), serde_json::json!(gamma));
                map.insert("out_black".into(), serde_json::json!(out_black));
                map.insert("out_white".into(), serde_json::json!(out_white));
                ("levels", selection, extra)
            }
            Self::HueSaturation {
                hue,
                saturation,
                lightness,
                selection,
                extra,
            } => {
                map.insert("hue".into(), serde_json::json!(hue));
                map.insert("saturation".into(), serde_json::json!(saturation));
                map.insert("lightness".into(), serde_json::json!(lightness));
                ("hue_saturation", selection, extra)
            }
            Self::Invert { selection, extra } => ("invert", selection, extra),
            Self::Curves {
                channel,
                points,
                selection,
                extra,
            } => {
                map.insert(
                    "channel".into(),
                    serde_json::to_value(channel).map_err(serde::ser::Error::custom)?,
                );
                map.insert(
                    "points".into(),
                    serde_json::to_value(points).map_err(serde::ser::Error::custom)?,
                );
                ("curves", selection, extra)
            }
        };
        map.insert("type".into(), serde_json::Value::String(type_name.into()));
        if let Some(selection) = selection {
            map.insert(
                "selection".into(),
                serde_json::to_value(selection).map_err(serde::ser::Error::custom)?,
            );
        }
        for (key, value) in extra {
            map.entry(key.clone()).or_insert_with(|| value.clone());
        }
        serde_json::Value::Object(map).serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Effect {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let obj = match value {
            serde_json::Value::Object(map) => map,
            _ => return Err(serde::de::Error::custom("effect must be an object")),
        };
        effect_from_map(obj).map_err(serde::de::Error::custom)
    }
}

impl Effect {
    pub fn selection(&self) -> Option<&TileMap> {
        match self {
            Self::GaussianBlur { selection, .. }
            | Self::ColorAdjust { selection, .. }
            | Self::Levels { selection, .. }
            | Self::HueSaturation { selection, .. }
            | Self::Invert { selection, .. }
            | Self::Curves { selection, .. } => selection.as_ref(),
        }
    }
    pub fn selection_mut(&mut self) -> &mut Option<TileMap> {
        match self {
            Self::GaussianBlur { selection, .. }
            | Self::ColorAdjust { selection, .. }
            | Self::Levels { selection, .. }
            | Self::HueSaturation { selection, .. }
            | Self::Invert { selection, .. }
            | Self::Curves { selection, .. } => selection,
        }
    }
    pub fn validate(&self) -> Result<(), DocError> {
        let finite = |name: &str, n: f64, min: f64, max: f64| {
            if n.is_finite() && (min..=max).contains(&n) {
                Ok(())
            } else {
                Err(DocError::Invalid(format!("Invalid {name}")))
            }
        };
        match self {
            Self::GaussianBlur { radius, .. } => finite("radius", *radius, 0.0, 64.0)?,
            Self::ColorAdjust {
                brightness,
                contrast,
                saturation,
                ..
            } => {
                finite("brightness", *brightness, -1.0, 1.0)?;
                finite("contrast", *contrast, 0.0, 4.0)?;
                finite("saturation", *saturation, 0.0, 4.0)?;
            }
            Self::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
                ..
            } => {
                finite("in_black", *in_black, 0.0, 1.0)?;
                finite("in_white", *in_white, 0.0, 1.0)?;
                finite("gamma", *gamma, 0.1, 10.0)?;
                finite("out_black", *out_black, 0.0, 1.0)?;
                finite("out_white", *out_white, 0.0, 1.0)?;
                if in_black >= in_white {
                    return Err(DocError::Invalid("Invalid levels range".into()));
                }
            }
            Self::HueSaturation {
                hue,
                saturation,
                lightness,
                ..
            } => {
                finite("hue", *hue, -180.0, 180.0)?;
                finite("saturation", *saturation, 0.0, 4.0)?;
                finite("lightness", *lightness, -1.0, 1.0)?;
            }
            Self::Invert { .. } => {}
            Self::Curves { points, .. } => {
                if !(2..=16).contains(&points.len()) {
                    return Err(DocError::Invalid("Curve point count".into()));
                }
                let mut previous = -1.0;
                for point in points {
                    if !(0.0..=1.0).contains(&point.x)
                        || !(0.0..=1.0).contains(&point.y)
                        || point.x <= previous
                    {
                        return Err(DocError::Invalid("Invalid curve point".into()));
                    }
                    previous = point.x;
                }
            }
        }
        if let Some(map) = self.selection() {
            validate_mask(map)?;
        }
        Ok(())
    }
}

pub fn validate_effect(effect: &Effect) -> Result<(), DocError> {
    effect.validate()
}

pub fn extended_effect(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Levels { .. }
            | Effect::Curves { .. }
            | Effect::HueSaturation { .. }
            | Effect::Invert { .. }
    )
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    Group {
        children: Vec<NodeId>,
        isolation: Isolation,
    },
    Raster {
        tiles: TileMap,
    },
    Vector {
        paths: Vec<StyledPath>,
    },
    Text {
        content: Box<TextContent>,
    },
    Adjustment {
        adjustment: Effect,
    },
    Unsupported {
        raw: serde_json::Value,
    },
}

impl Serialize for NodeKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let value = match self {
            Self::Group {
                children,
                isolation,
            } => serde_json::json!({"kind":"group","children":children,"isolation":isolation}),
            Self::Raster { tiles } => serde_json::json!({"kind":"raster","tiles":tiles}),
            Self::Vector { paths } => serde_json::json!({"kind":"vector","paths":paths}),
            Self::Text { content } => serde_json::json!({"kind":"text","content":content}),
            Self::Adjustment { adjustment } => {
                serde_json::json!({"kind":"adjustment","adjustment":adjustment})
            }
            Self::Unsupported { raw } => raw.clone(),
        };
        value.serialize(s)
    }
}
impl<'de> Deserialize<'de> for NodeKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        fn field<T: serde::de::DeserializeOwned>(
            v: &serde_json::Value,
            k: &str,
        ) -> Result<T, String> {
            serde_json::from_value(v.get(k).ok_or_else(|| format!("missing {k}"))?.clone())
                .map_err(|e| e.to_string())
        }
        let result: Result<Self, String> = match v.get("kind").and_then(|k| k.as_str()) {
            Some("group") => field(&v, "children").and_then(|children| {
                field(&v, "isolation").map(|isolation| Self::Group {
                    children,
                    isolation,
                })
            }),
            Some("raster") => field(&v, "tiles").map(|tiles| Self::Raster { tiles }),
            Some("vector") => field(&v, "paths").map(|paths| Self::Vector { paths }),
            Some("text") => field(&v, "content").map(|content| Self::Text { content }),
            Some("adjustment") => {
                field(&v, "adjustment").map(|adjustment| Self::Adjustment { adjustment })
            }
            Some(_) => Ok(Self::Unsupported { raw: v }),
            None => Err("missing node kind".into()),
        };
        result.map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub name: String,
    pub props: LayerProps,
    pub kind: NodeKind,
}
impl Node {
    pub fn group(name: impl Into<String>) -> Self {
        Self {
            id: NodeId::new(),
            parent: None,
            name: name.into(),
            props: LayerProps::default(),
            kind: NodeKind::Group {
                children: vec![],
                isolation: Isolation::PassThrough,
            },
        }
    }
    pub fn raster(name: impl Into<String>) -> Self {
        Self {
            id: NodeId::new(),
            parent: None,
            name: name.into(),
            props: LayerProps::default(),
            kind: NodeKind::Raster {
                tiles: TileMap::new(PixelFormat::Rgba8),
            },
        }
    }
    pub fn vector(name: impl Into<String>, paths: Vec<StyledPath>) -> Self {
        let mut node = Self::raster(name);
        node.kind = NodeKind::Vector { paths };
        node
    }
    pub fn text(name: impl Into<String>, content: TextContent) -> Self {
        let mut node = Self::raster(name);
        node.kind = NodeKind::Text {
            content: Box::new(content),
        };
        node
    }
    pub fn kind_name(&self) -> &str {
        match &self.kind {
            NodeKind::Group { .. } => "group",
            NodeKind::Raster { .. } => "raster",
            NodeKind::Vector { .. } => "vector",
            NodeKind::Text { .. } => "text",
            NodeKind::Adjustment { .. } => "adjustment",
            NodeKind::Unsupported { raw } => raw
                .get("kind")
                .and_then(|k| k.as_str())
                .unwrap_or("unknown"),
        }
    }
    pub fn children(&self) -> Option<&[NodeId]> {
        match &self.kind {
            NodeKind::Group { children, .. } => Some(children),
            _ => None,
        }
    }
    fn children_mut(&mut self) -> Option<&mut Vec<NodeId>> {
        match &mut self.kind {
            NodeKind::Group { children, .. } => Some(children),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRecord {
    pub id: AssetId,
    pub hash: ContentHash,
    pub kind: AssetKind,
    pub byte_len: u64,
    pub dims: Option<(u32, u32)>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssetKind {
    EncodedImage {
        mime: String,
    },
    Font {
        #[serde(default)]
        family: String,
        #[serde(default)]
        face_index: u32,
        #[serde(default)]
        embedding: alto_text::FontEmbedding,
    },
    BrushTip,
    Pattern,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub meta: DocumentMeta,
    pub nodes: BTreeMap<NodeId, Node>,
    pub root: NodeId,
    pub assets: BTreeMap<AssetId, AssetRecord>,
    pub selection: Option<TileMap>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Snapshot {
    meta: DocumentMeta,
    root: NodeId,
    nodes: Vec<Node>,
    assets: Vec<AssetRecord>,
    selection: Option<TileMap>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PropPatch {
    pub name: Option<String>,
    pub visible: Option<bool>,
    pub locked: Option<bool>,
    pub opacity: Option<f32>,
    pub blend: Option<BlendMode>,
    pub transform: Option<Affine2>,
    pub clip_to_below: Option<bool>,
    pub mask_enabled: Option<bool>,
    pub mask_linked: Option<bool>,
    pub mask_transform: Option<Affine2>,
    pub effects: Option<Vec<Effect>>,
}
impl PropPatch {
    /// An unlock is only the `locked` flag. Other fields in the same patch would
    /// edit a locked layer through the one exception that lets a caller unlock.
    fn clears_lock_only(&self) -> bool {
        self.locked == Some(false)
            && self.name.is_none()
            && self.visible.is_none()
            && self.opacity.is_none()
            && self.blend.is_none()
            && self.transform.is_none()
            && self.clip_to_below.is_none()
            && self.mask_enabled.is_none()
            && self.mask_linked.is_none()
            && self.mask_transform.is_none()
            && self.effects.is_none()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileChange {
    pub key: TileKey,
    pub before: Option<ContentHash>,
    pub after: Option<ContentHash>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Op {
    InsertNode {
        node: Node,
        parent: NodeId,
        index: usize,
    },
    RemoveNode {
        id: NodeId,
    },
    RestoreSubtree {
        nodes: Vec<Node>,
        parent: NodeId,
        index: usize,
    },
    MoveNode {
        id: NodeId,
        parent: NodeId,
        index: usize,
    },
    SetProps {
        id: NodeId,
        patch: PropPatch,
    },
    SetMask {
        layer: NodeId,
        mask: Option<TileMap>,
    },
    SetAdjustment {
        id: NodeId,
        adjustment: Effect,
    },
    SetTiles {
        layer: NodeId,
        changes: Vec<TileChange>,
    },
    SetVector {
        id: NodeId,
        paths: Vec<StyledPath>,
    },
    SetVectorMask {
        id: NodeId,
        mask: Option<VectorMask>,
    },
    SetClipPath {
        id: NodeId,
        path: Option<ClipPath>,
    },
    SetText {
        id: NodeId,
        content: Box<TextContent>,
    },
    SetSelection {
        selection: Option<TileMap>,
    },
    SetAsset {
        id: AssetId,
        record: Option<AssetRecord>,
    },
    SetMeta {
        meta: DocumentMeta,
    },
    ReplaceNode {
        id: NodeId,
        node: Node,
    },
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ChangeSet {
    pub nodes: BTreeSet<NodeId>,
    pub structure_changed: bool,
    pub dirty: Vec<(NodeId, Rect)>,
    pub tiles: Vec<(NodeId, TileKey)>,
    /// Document-level fields that are not nodes. A patch can send these alone.
    pub selection_changed: bool,
    pub assets_changed: bool,
    pub meta_changed: bool,
}
impl ChangeSet {
    fn merge(&mut self, other: Self) {
        self.nodes.extend(other.nodes);
        self.structure_changed |= other.structure_changed;
        self.dirty.extend(other.dirty);
        self.tiles.extend(other.tiles);
        self.selection_changed |= other.selection_changed;
        self.assets_changed |= other.assets_changed;
        self.meta_changed |= other.meta_changed;
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
/// Messages start with the code the SDK reports (`CODE: detail`).
pub enum DocError {
    #[error("INVALID_ARGUMENT: invalid operation: {0}")]
    Invalid(String),
    #[error("NOT_FOUND: node not found: {0:?}")]
    NotFound(NodeId),
    #[error("CONFLICT: revision conflict: expected {expected}, actual {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error("INVALID_ARGUMENT: serialization error: {0}")]
    Serialization(String),
}

impl Document {
    pub fn new(meta: DocumentMeta) -> Result<Self, DocError> {
        meta.validate()?;
        let root = Node::group("Root");
        let root_id = root.id;
        let mut nodes = BTreeMap::new();
        nodes.insert(root_id, root);
        Ok(Self {
            meta,
            nodes,
            root: root_id,
            assets: BTreeMap::new(),
            selection: None,
            revision: 0,
        })
    }
    pub fn node(&self, id: NodeId) -> Result<&Node, DocError> {
        self.nodes.get(&id).ok_or(DocError::NotFound(id))
    }
    fn node_mut(&mut self, id: NodeId) -> Result<&mut Node, DocError> {
        self.nodes.get_mut(&id).ok_or(DocError::NotFound(id))
    }
    fn group_children(&self, id: NodeId) -> Result<&[NodeId], DocError> {
        self.node(id)?
            .children()
            .ok_or_else(|| DocError::Invalid("Parent must be a group".into()))
    }
    fn group_children_mut(&mut self, id: NodeId) -> Result<&mut Vec<NodeId>, DocError> {
        self.node_mut(id)?
            .children_mut()
            .ok_or_else(|| DocError::Invalid("Parent must be a group".into()))
    }
    fn subtree(&self, id: NodeId, out: &mut Vec<Node>) -> Result<(), DocError> {
        let node = self.node(id)?.clone();
        if let Some(children) = node.children() {
            for child in children {
                self.subtree(*child, out)?;
            }
        }
        out.push(node);
        Ok(())
    }
    fn is_descendant(&self, candidate: NodeId, ancestor: NodeId) -> bool {
        let mut cursor = Some(candidate);
        while let Some(id) = cursor {
            if id == ancestor {
                return true;
            }
            cursor = self.nodes.get(&id).and_then(|node| node.parent);
        }
        false
    }
    fn ancestor_locked(&self, id: NodeId) -> Result<bool, DocError> {
        let mut cursor = self.node(id)?.parent;
        let mut hops = 0;
        while let Some(current) = cursor {
            hops += 1;
            if hops > MAX_TREE_DEPTH {
                return Err(DocError::Invalid(format!(
                    "Group nesting exceeds {MAX_TREE_DEPTH}"
                )));
            }
            let node = self.node(current)?;
            if node.props.locked {
                return Ok(true);
            }
            cursor = node.parent;
        }
        Ok(false)
    }
    fn descendant_locked(&self, id: NodeId) -> Result<bool, DocError> {
        fn walk(doc: &Document, id: NodeId, hops: u32) -> Result<bool, DocError> {
            if hops > MAX_TREE_DEPTH {
                return Err(DocError::Invalid(format!(
                    "Group nesting exceeds {MAX_TREE_DEPTH}"
                )));
            }
            let Some(children) = doc.node(id)?.children().map(|c| c.to_vec()) else {
                return Ok(false);
            };
            for child in children {
                if doc.node(child)?.props.locked || walk(doc, child, hops + 1)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        walk(self, id, 0)
    }
    /// A locked node rejects every edit except a `SetProps` whose only change is
    /// `locked: false`. Other fields in that patch are rejected: unlock first,
    /// then edit. A locked ancestor rejects the clear as well. `ReplaceNode`
    /// does not bypass a lock by supplying an unlocked replacement, and
    /// `RemoveNode` fails when any descendant is locked. Inverse ops of the
    /// transaction that produced them skip this check.
    fn enforce_lock(&self, op: &Op) -> Result<(), DocError> {
        fn locked() -> DocError {
            DocError::Invalid("layer is locked".into())
        }
        let editable = |doc: &Self, id: NodeId| -> Result<(), DocError> {
            if doc.node(id)?.props.locked || doc.ancestor_locked(id)? {
                Err(locked())
            } else {
                Ok(())
            }
        };
        match op {
            Op::InsertNode { parent, .. } | Op::RestoreSubtree { parent, .. } => {
                editable(self, *parent)
            }
            Op::SetMask { layer: id, .. }
            | Op::SetTiles { layer: id, .. }
            | Op::SetAdjustment { id, .. }
            | Op::SetVector { id, .. }
            | Op::SetVectorMask { id, .. }
            | Op::SetClipPath { id, .. }
            | Op::SetText { id, .. } => editable(self, *id),
            Op::RemoveNode { id } => {
                editable(self, *id)?;
                if self.descendant_locked(*id)? {
                    Err(locked())
                } else {
                    Ok(())
                }
            }
            Op::MoveNode { id, parent, .. } => {
                editable(self, *id)?;
                editable(self, *parent)
            }
            Op::SetProps { id, patch } => {
                let locked_node = self.node(*id)?.props.locked;
                let ancestor = self.ancestor_locked(*id)?;
                if !ancestor && (!locked_node || patch.clears_lock_only()) {
                    Ok(())
                } else {
                    Err(locked())
                }
            }
            Op::ReplaceNode { id, .. } => editable(self, *id),
            Op::SetSelection { .. } | Op::SetAsset { .. } | Op::SetMeta { .. } => Ok(()),
        }
    }
    /// `enforce` is false for an inverse of an op this transaction already
    /// applied. The forward op passed the lock check (or was itself an inverse);
    /// applying the inverse must not trip over a lock that op created.
    fn apply(&mut self, op: Op, enforce: bool) -> Result<(Op, ChangeSet), DocError> {
        if enforce {
            self.enforce_lock(&op)?;
        }
        let mut change = ChangeSet::default();
        let inverse = match op {
            Op::InsertNode {
                mut node,
                parent,
                index,
            } => {
                if self.nodes.contains_key(&node.id) {
                    return Err(DocError::Invalid("Node ID already exists".into()));
                }
                node.props.validate()?;
                if node.children().is_some_and(|children| !children.is_empty()) {
                    return Err(DocError::Invalid("Inserted group must be empty".into()));
                }
                if index > self.group_children(parent)?.len() {
                    return Err(DocError::Invalid("Child index out of range".into()));
                }
                node.parent = Some(parent);
                let id = node.id;
                self.group_children_mut(parent)?.insert(index, id);
                self.nodes.insert(id, node);
                change.nodes.extend([parent, id]);
                change.structure_changed = true;
                Op::RemoveNode { id }
            }
            Op::RemoveNode { id } => {
                if id == self.root {
                    return Err(DocError::Invalid("Cannot remove root".into()));
                }
                let parent = self
                    .node(id)?
                    .parent
                    .ok_or_else(|| DocError::Invalid("Orphan node".into()))?;
                let index = self
                    .group_children(parent)?
                    .iter()
                    .position(|child| *child == id)
                    .ok_or_else(|| DocError::Invalid("Parent link broken".into()))?;
                let mut nodes = Vec::new();
                self.subtree(id, &mut nodes)?;
                self.group_children_mut(parent)?.remove(index);
                for node in &nodes {
                    self.nodes.remove(&node.id);
                    change.nodes.insert(node.id);
                }
                change.nodes.insert(parent);
                change.structure_changed = true;
                Op::RestoreSubtree {
                    nodes,
                    parent,
                    index,
                }
            }
            Op::RestoreSubtree {
                nodes,
                parent,
                index,
            } => {
                if nodes.is_empty() {
                    return Err(DocError::Invalid("Empty subtree".into()));
                }
                if index > self.group_children(parent)?.len()
                    || nodes.iter().any(|node| self.nodes.contains_key(&node.id))
                {
                    return Err(DocError::Invalid("Cannot restore subtree".into()));
                }
                let top = nodes
                    .last()
                    .ok_or_else(|| DocError::Invalid("Empty subtree".into()))?
                    .id;
                if nodes.last().and_then(|node| node.parent) != Some(parent) {
                    return Err(DocError::Invalid("Subtree parent mismatch".into()));
                }
                for node in nodes {
                    change.nodes.insert(node.id);
                    self.nodes.insert(node.id, node);
                }
                self.group_children_mut(parent)?.insert(index, top);
                change.nodes.insert(parent);
                change.structure_changed = true;
                Op::RemoveNode { id: top }
            }
            Op::MoveNode { id, parent, index } => {
                if id == self.root || self.is_descendant(parent, id) {
                    return Err(DocError::Invalid("Move would create a cycle".into()));
                }
                let old_parent = self
                    .node(id)?
                    .parent
                    .ok_or_else(|| DocError::Invalid("Orphan node".into()))?;
                let old_index = self
                    .group_children(old_parent)?
                    .iter()
                    .position(|child| *child == id)
                    .ok_or_else(|| DocError::Invalid("Parent link broken".into()))?;
                let max_index =
                    self.group_children(parent)?.len() - usize::from(parent == old_parent);
                if index > max_index {
                    return Err(DocError::Invalid("Child index out of range".into()));
                }
                self.group_children_mut(old_parent)?.remove(old_index);
                self.group_children_mut(parent)?.insert(index, id);
                self.node_mut(id)?.parent = Some(parent);
                change.nodes.extend([id, old_parent, parent]);
                change.structure_changed = true;
                Op::MoveNode {
                    id,
                    parent: old_parent,
                    index: old_index,
                }
            }
            Op::SetProps { id, patch } => {
                let previous = self.node(id)?;
                let mask_transform = if patch.mask_transform.is_none()
                    && patch
                        .mask_linked
                        .is_some_and(|linked| linked != previous.props.mask_linked)
                {
                    let mut chain = vec![];
                    let mut next = Some(id);
                    while let Some(node) = next {
                        let node = self.node(node)?;
                        chain.push(node.props.transform);
                        next = node.parent;
                    }
                    let world = chain
                        .iter()
                        .rev()
                        .copied()
                        .fold(Affine2::default(), Affine2::concat);
                    Some(if patch.mask_linked == Some(false) {
                        world.concat(previous.props.mask_transform)
                    } else {
                        let final_world = if let Some(transform) = patch.transform {
                            chain[0] = transform;
                            chain
                                .iter()
                                .rev()
                                .copied()
                                .fold(Affine2::default(), Affine2::concat)
                        } else {
                            world
                        };
                        final_world
                            .inverse()
                            .ok_or_else(|| {
                                DocError::Invalid("Cannot link mask to singular transform".into())
                            })?
                            .concat(previous.props.mask_transform)
                    })
                } else {
                    patch.mask_transform
                };
                let saved = self.node(id)?.clone();
                let validated = (|| -> Result<PropPatch, DocError> {
                    let node = self.node_mut(id)?;
                    let mut old = PropPatch::default();
                    if let Some(name) = patch.name {
                        old.name = Some(std::mem::replace(&mut node.name, name));
                    }
                    if let Some(v) = patch.visible {
                        old.visible = Some(std::mem::replace(&mut node.props.visible, v));
                    }
                    if let Some(v) = patch.locked {
                        old.locked = Some(std::mem::replace(&mut node.props.locked, v));
                    }
                    if let Some(v) = patch.opacity {
                        old.opacity = Some(std::mem::replace(&mut node.props.opacity, v));
                    }
                    if let Some(v) = patch.blend {
                        old.blend = Some(std::mem::replace(&mut node.props.blend, v));
                    }
                    if let Some(v) = patch.transform {
                        old.transform = Some(std::mem::replace(&mut node.props.transform, v));
                    }
                    if let Some(v) = patch.clip_to_below {
                        old.clip_to_below =
                            Some(std::mem::replace(&mut node.props.clip_to_below, v));
                    }
                    if let Some(v) = patch.mask_enabled {
                        old.mask_enabled = Some(std::mem::replace(&mut node.props.mask_enabled, v));
                    }
                    if let Some(v) = mask_transform {
                        old.mask_transform =
                            Some(std::mem::replace(&mut node.props.mask_transform, v));
                    }
                    if let Some(v) = patch.mask_linked {
                        old.mask_linked = Some(std::mem::replace(&mut node.props.mask_linked, v));
                    }
                    if let Some(v) = patch.effects {
                        old.effects = Some(std::mem::replace(&mut node.props.effects, v));
                    }
                    node.props.validate()?;
                    Ok(old)
                })();
                let old = match validated {
                    Ok(old) => old,
                    Err(e) => {
                        self.nodes.insert(id, saved);
                        return Err(e);
                    }
                };
                change.nodes.insert(id);
                Op::SetProps { id, patch: old }
            }
            Op::SetMask { layer, mask } => {
                if let Some(m) = &mask {
                    validate_mask(m)?;
                }
                change.nodes.insert(layer);
                Op::SetMask {
                    layer,
                    mask: std::mem::replace(&mut self.node_mut(layer)?.props.mask, mask),
                }
            }
            Op::SetAdjustment { id, adjustment } => {
                validate_effect(&adjustment)?;
                let NodeKind::Adjustment {
                    adjustment: current,
                } = &mut self.node_mut(id)?.kind
                else {
                    return Err(DocError::Invalid("Adjustment node required".into()));
                };
                change.nodes.insert(id);
                Op::SetAdjustment {
                    id,
                    adjustment: std::mem::replace(current, adjustment),
                }
            }
            Op::SetTiles { layer, changes } => {
                let node = self.node(layer)?;
                let NodeKind::Raster { tiles } = &node.kind else {
                    return Err(DocError::Invalid("Target must be raster".into()));
                };
                let mut seen = BTreeSet::new();
                for item in &changes {
                    if !seen.insert(item.key) || tiles.tiles.get(&item.key).copied() != item.before
                    {
                        return Err(DocError::Invalid("Tile precondition failed".into()));
                    }
                }
                let NodeKind::Raster { tiles } = &mut self.node_mut(layer)?.kind else {
                    unreachable!()
                };
                for item in &changes {
                    match item.after {
                        Some(hash) => {
                            tiles.tiles.insert(item.key, hash);
                        }
                        None => {
                            tiles.tiles.remove(&item.key);
                        }
                    }
                    change.tiles.push((layer, item.key));
                    let x = item.key.tx as f64 * TILE as f64;
                    let y = item.key.ty as f64 * TILE as f64;
                    change.dirty.push((
                        layer,
                        Rect {
                            x0: x,
                            y0: y,
                            x1: x + TILE as f64,
                            y1: y + TILE as f64,
                        },
                    ));
                }
                change.nodes.insert(layer);
                Op::SetTiles {
                    layer,
                    changes: changes
                        .into_iter()
                        .map(|item| TileChange {
                            key: item.key,
                            before: item.after,
                            after: item.before,
                        })
                        .collect(),
                }
            }
            Op::SetVectorMask { id, mask } => {
                if let Some(m) = &mask {
                    m.validate().map_err(|e| DocError::Invalid(e.to_string()))?;
                }
                change.nodes.insert(id);
                Op::SetVectorMask {
                    id,
                    mask: std::mem::replace(
                        &mut self.node_mut(id)?.props.vector_mask,
                        mask.map(Box::new),
                    )
                    .map(|m| *m),
                }
            }
            Op::SetClipPath { id, path } => {
                if let Some(m) = &path {
                    m.validate().map_err(|e| DocError::Invalid(e.to_string()))?;
                }
                change.nodes.insert(id);
                Op::SetClipPath {
                    id,
                    path: std::mem::replace(
                        &mut self.node_mut(id)?.props.clip_path,
                        path.map(Box::new),
                    )
                    .map(|m| *m),
                }
            }
            Op::SetVector { id, paths } => {
                validate_paths(&paths).map_err(|e| DocError::Invalid(e.to_string()))?;
                let NodeKind::Vector { paths: old } = &mut self.node_mut(id)?.kind else {
                    return Err(DocError::Invalid("Target must be vector".into()));
                };
                change.nodes.insert(id);
                Op::SetVector {
                    id,
                    paths: std::mem::replace(old, paths),
                }
            }
            Op::SetText { id, content } => {
                content
                    .validate()
                    .map_err(|e| DocError::Invalid(e.to_string()))?;
                let NodeKind::Text { content: old } = &mut self.node_mut(id)?.kind else {
                    return Err(DocError::Invalid("Target must be text".into()));
                };
                change.nodes.insert(id);
                Op::SetText {
                    id,
                    content: std::mem::replace(old, content),
                }
            }
            Op::SetSelection { selection } => {
                if let Some(m) = &selection {
                    validate_mask(m)?;
                }
                change.selection_changed = true;
                Op::SetSelection {
                    selection: std::mem::replace(&mut self.selection, selection),
                }
            }
            Op::SetAsset { id, record } => {
                if record.as_ref().is_some_and(|record| record.id != id) {
                    return Err(DocError::Invalid("Asset ID mismatch".into()));
                }
                let old = match record {
                    Some(record) => self.assets.insert(id, record),
                    None => self.assets.remove(&id),
                };
                change.assets_changed = true;
                Op::SetAsset { id, record: old }
            }
            Op::SetMeta { meta } => {
                meta.validate()?;
                change.meta_changed = true;
                Op::SetMeta {
                    meta: std::mem::replace(&mut self.meta, meta),
                }
            }
            Op::ReplaceNode { id, mut node } => {
                if node.id != id {
                    return Err(DocError::Invalid("Replacement node id mismatch".into()));
                }
                let current = self.node(id)?.clone();
                if current.parent != node.parent && node.parent.is_some() {
                    return Err(DocError::Invalid("Replacement changes parent".into()));
                }
                node.parent = current.parent;
                let children = |kind: &NodeKind| match kind {
                    NodeKind::Group { children, .. } => children.clone(),
                    _ => Vec::new(),
                };
                if children(&current.kind) != children(&node.kind) {
                    return Err(DocError::Invalid(
                        "Replacement cannot change group children".into(),
                    ));
                }
                node.props.validate()?;
                self.nodes.insert(id, node);
                change.nodes.insert(id);
                Op::ReplaceNode { id, node: current }
            }
        };
        Ok((inverse, change))
    }
    pub fn snapshot_bytes(&self) -> Result<Vec<u8>, DocError> {
        let mut ordered = Vec::new();
        fn walk(doc: &Document, id: NodeId, out: &mut Vec<Node>) -> Result<(), DocError> {
            let node = doc.node(id)?.clone();
            out.push(node.clone());
            if let Some(children) = node.children() {
                for child in children {
                    walk(doc, *child, out)?;
                }
            }
            Ok(())
        }
        walk(self, self.root, &mut ordered)?;
        let mut meta = self.meta.clone();
        let extended = ordered.iter().any(|node| {
            node.props.effects.iter().any(extended_effect)
                || matches!(&node.kind, NodeKind::Adjustment { adjustment } if extended_effect(adjustment))
        });
        meta.format_version = if extended { 2 } else { 1 };
        let snapshot = Snapshot {
            meta,
            root: self.root,
            nodes: ordered,
            assets: self.assets.values().cloned().collect(),
            selection: self.selection.clone(),
        };
        // Pretty, tree-ordered JSON: identical documents give identical bytes, and the
        // folder format produces line-oriented Git diffs.
        let mut bytes = serde_json::to_vec_pretty(&snapshot)
            .map_err(|error| DocError::Serialization(error.to_string()))?;
        bytes.push(b'\n');
        Ok(bytes)
    }
    pub fn from_snapshot_bytes(bytes: &[u8]) -> Result<Self, DocError> {
        let snapshot: Snapshot = serde_json::from_slice(bytes)
            .map_err(|error| DocError::Serialization(error.to_string()))?;
        snapshot.meta.validate()?;
        let count = snapshot.nodes.len();
        let nodes: BTreeMap<_, _> = snapshot
            .nodes
            .into_iter()
            .map(|node| (node.id, node))
            .collect();
        if nodes.len() != count {
            return Err(DocError::Invalid("Duplicate node ID".into()));
        }
        let doc = Self {
            meta: snapshot.meta,
            nodes,
            root: snapshot.root,
            assets: snapshot
                .assets
                .into_iter()
                .map(|asset| (asset.id, asset))
                .collect(),
            selection: snapshot.selection,
            revision: 0,
        };
        doc.validate_graph()?;
        Ok(doc)
    }
    pub fn validate_graph(&self) -> Result<(), DocError> {
        if let Some(selection) = &self.selection {
            validate_mask(selection)?;
        }
        let root = self.node(self.root)?;
        if root.parent.is_some() || root.children().is_none() {
            return Err(DocError::Invalid("Invalid root".into()));
        }
        let mut visited = BTreeSet::new();
        fn visit(
            doc: &Document,
            id: NodeId,
            visited: &mut BTreeSet<NodeId>,
            depth: usize,
            stack: u32,
            drawn: bool,
        ) -> Result<(), DocError> {
            if depth > MAX_TREE_DEPTH as usize {
                return Err(DocError::Invalid(format!(
                    "Group nesting exceeds {MAX_TREE_DEPTH}"
                )));
            }
            if !visited.insert(id) {
                return Err(DocError::Invalid("Cycle or duplicate child".into()));
            }
            let node = doc.node(id)?;
            node.props.validate()?;
            let drawn = drawn && node.props.visible && node.props.opacity > 0.0;
            let isolate = drawn
                && match &node.kind {
                    NodeKind::Group { isolation, .. } => {
                        *isolation == Isolation::Isolated
                            || node.props.opacity < 1.0
                            || node.props.blend != BlendMode::Normal
                    }
                    _ => false,
                };
            if isolate && stack >= MAX_ISOLATED_GROUPS {
                return Err(DocError::Invalid(format!(
                    "Group nesting exceeds {MAX_ISOLATED_GROUPS}"
                )));
            }
            let stack = stack + u32::from(isolate);
            if let NodeKind::Vector { paths } = &node.kind {
                validate_paths(paths).map_err(|e| DocError::Invalid(e.to_string()))?;
            }
            if let NodeKind::Adjustment { adjustment } = &node.kind {
                validate_effect(adjustment)?;
            }
            if let NodeKind::Text { content } = &node.kind {
                content
                    .validate()
                    .map_err(|e| DocError::Invalid(e.to_string()))?;
                for font in content.font_ids() {
                    if !matches!(
                        doc.assets.get(&font).map(|a| &a.kind),
                        Some(AssetKind::Font { .. })
                    ) {
                        return Err(DocError::Invalid(format!(
                            "Text references missing font asset {}",
                            font.0
                        )));
                    }
                }
            }
            if let Some(children) = node.children() {
                for child in children {
                    if doc.node(*child)?.parent != Some(id) {
                        return Err(DocError::Invalid("Parent link mismatch".into()));
                    }
                    visit(doc, *child, visited, depth + 1, stack, drawn)?;
                }
            }
            Ok(())
        }
        visit(self, self.root, &mut visited, 0, 0, true)?;
        if visited.len() != self.nodes.len() {
            return Err(DocError::Invalid("Unreachable node".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppliedOp {
    pub op: Op,
    pub inverse: Op,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transaction {
    pub intent: String,
    pub ops: Vec<AppliedOp>,
    pub base_revision: u64,
    #[serde(default)]
    pub seq: u64,
}

/// Edits since the last version commit, summarised into the commit's intents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IntentLogEntry {
    pub seq: u64,
    pub intent: String,
    pub targets: Vec<NodeId>,
}

impl Op {
    /// The node an operation edits, for intent summaries.
    pub fn target(&self) -> Option<NodeId> {
        match self {
            Op::InsertNode { node, .. } => Some(node.id),
            Op::RestoreSubtree { nodes, .. } => nodes.last().map(|n| n.id),
            Op::RemoveNode { id }
            | Op::MoveNode { id, .. }
            | Op::SetProps { id, .. }
            | Op::SetAdjustment { id, .. }
            | Op::SetVector { id, .. }
            | Op::SetVectorMask { id, .. }
            | Op::SetClipPath { id, .. }
            | Op::SetText { id, .. }
            | Op::ReplaceNode { id, .. } => Some(*id),
            Op::SetMask { layer, .. } | Op::SetTiles { layer, .. } => Some(*layer),
            Op::SetSelection { .. } | Op::SetAsset { .. } | Op::SetMeta { .. } => None,
        }
    }
}

#[derive(Clone)]
pub struct Editor {
    pub doc: Document,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    max_undo: usize,
    seq: u64,
    log: Vec<IntentLogEntry>,
}
impl Editor {
    pub fn new(doc: Document) -> Self {
        Self {
            doc,
            undo: vec![],
            redo: vec![],
            max_undo: 100,
            seq: 0,
            log: vec![],
        }
    }
    /// Edits recorded since the last call (the last commit).
    pub fn take_intent_log(&mut self) -> Vec<IntentLogEntry> {
        std::mem::take(&mut self.log)
    }
    pub fn intent_log(&self) -> &[IntentLogEntry] {
        &self.log
    }
    fn log_tx(&mut self, tx: &Transaction, prefix: &str) {
        let mut targets = vec![];
        for op in &tx.ops {
            if let Some(t) = op.op.target()
                && !targets.contains(&t)
            {
                targets.push(t);
            }
        }
        self.log.push(IntentLogEntry {
            seq: tx.seq,
            intent: format!("{prefix}{}", tx.intent),
            targets,
        });
    }
    /// Applies `ops` to the live document. A failed op or a failed graph check
    /// rolls the completed ops back, so the caller does not keep a second copy
    /// of the document just to discard a rejected edit.
    fn replay(
        &mut self,
        ops: Vec<Op>,
        enforce: bool,
    ) -> Result<(Vec<AppliedOp>, ChangeSet), DocError> {
        let revision = self.doc.revision;
        let mut applied = Vec::with_capacity(ops.len());
        let mut changes = ChangeSet::default();
        for op in ops {
            match self.doc.apply(op.clone(), enforce) {
                Ok((inverse, change)) => {
                    applied.push(AppliedOp { op, inverse });
                    changes.merge(change);
                }
                Err(e) => {
                    let revert_err = self.revert(&applied).err();
                    self.doc.revision = revision;
                    return Err(revert_err.unwrap_or(e));
                }
            }
        }
        if let Err(e) = self.doc.validate_graph() {
            let revert_err = self.revert(&applied).err();
            self.doc.revision = revision;
            return Err(revert_err.unwrap_or(e));
        }
        Ok((applied, changes))
    }
    /// Rolls back completed ops. Lock checks are off: the inverse of an op this
    /// transaction applied is part of that transaction. Any other failure is
    /// returned; this does not panic.
    fn revert(&mut self, applied: &[AppliedOp]) -> Result<(), DocError> {
        for item in applied.iter().rev() {
            self.doc
                .apply(item.inverse.clone(), false)
                .map_err(|error| {
                    DocError::Invalid(format!("inverse of an applied edit: {error}"))
                })?;
        }
        Ok(())
    }
    pub fn transact(
        &mut self,
        intent: impl Into<String>,
        ops: Vec<Op>,
        base_revision: u64,
    ) -> Result<ChangeSet, DocError> {
        if base_revision != self.doc.revision {
            return Err(DocError::Conflict {
                expected: base_revision,
                actual: self.doc.revision,
            });
        }
        if ops.is_empty() {
            return Ok(ChangeSet::default());
        }
        let (applied, changes) = self.replay(ops, true)?;
        self.doc.revision += 1;
        self.seq += 1;
        let tx = Transaction {
            intent: intent.into(),
            ops: applied,
            base_revision,
            seq: self.seq,
        };
        self.log_tx(&tx, "");
        self.undo.push(tx);
        if self.undo.len() > self.max_undo {
            self.undo.remove(0);
        }
        self.redo.clear();
        Ok(changes)
    }
    pub fn undo(&mut self) -> Result<Option<ChangeSet>, DocError> {
        let Some(tx) = self.undo.last().cloned() else {
            return Ok(None);
        };
        let inverses = tx
            .ops
            .iter()
            .rev()
            .map(|item| item.inverse.clone())
            .collect();
        let (_, changes) = self.replay(inverses, false)?;
        self.doc.revision += 1;
        self.undo.pop();
        if self.log.last().is_some_and(|e| e.seq == tx.seq) {
            self.log.pop();
        } else {
            self.log_tx(&tx, "取り消し: ");
        }
        self.redo.push(tx);
        Ok(Some(changes))
    }
    pub fn redo(&mut self) -> Result<Option<ChangeSet>, DocError> {
        let Some(tx) = self.redo.last().cloned() else {
            return Ok(None);
        };
        let ops = tx.ops.iter().map(|item| item.op.clone()).collect();
        let (_, changes) = self.replay(ops, true)?;
        self.doc.revision += 1;
        self.redo.pop();
        self.log_tx(&tx, "");
        self.undo.push(tx);
        Ok(Some(changes))
    }
    pub fn drop_oldest_undo(&mut self) -> bool {
        if self.undo.is_empty() {
            false
        } else {
            self.undo.remove(0);
            true
        }
    }
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }
    pub fn undo_ops(&self) -> Vec<Op> {
        self.undo
            .last()
            .map(|t| t.ops.iter().rev().map(|o| o.inverse.clone()).collect())
            .unwrap_or_default()
    }
    pub fn redo_ops(&self) -> Vec<Op> {
        self.redo
            .last()
            .map(|t| t.ops.iter().map(|o| o.op.clone()).collect())
            .unwrap_or_default()
    }
    /// Oldest first.
    pub fn undo_history(&self) -> &[Transaction] {
        &self.undo
    }
    /// The next redo is last.
    pub fn redo_history(&self) -> &[Transaction] {
        &self.redo
    }
    pub fn drop_oldest_undos(&mut self, count: usize) {
        self.undo.drain(..count.min(self.undo.len()));
    }
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn editor() -> Editor {
        Editor::new(Document::new(DocumentMeta::srgb_u8(1920, 1080)).unwrap())
    }
    fn content(mut doc: Document) -> Document {
        doc.revision = 0;
        doc
    }

    #[test]
    fn transaction_undo_redo_and_canonical_snapshot() {
        let mut editor = editor();
        let before = editor.doc.clone();
        let layer = Node::raster("Background");
        let id = layer.id;
        let root = editor.doc.root;
        editor
            .transact(
                "Add layer",
                vec![Op::InsertNode {
                    node: layer,
                    parent: root,
                    index: 0,
                }],
                0,
            )
            .unwrap();
        let changed = editor.doc.clone();
        let bytes = changed.snapshot_bytes().unwrap();
        let reopened = Document::from_snapshot_bytes(&bytes).unwrap();
        assert_eq!(reopened.snapshot_bytes().unwrap(), bytes);
        assert_eq!(content(reopened), content(changed.clone()));
        assert!(editor.undo().unwrap().unwrap().structure_changed);
        assert_eq!(content(editor.doc.clone()), content(before.clone()));
        assert_eq!(
            editor.doc.snapshot_bytes().unwrap(),
            before.snapshot_bytes().unwrap()
        );
        assert!(editor.redo().unwrap().unwrap().nodes.contains(&id));
        assert_eq!(content(editor.doc.clone()), content(changed));
    }

    #[test]
    fn failed_transaction_is_atomic_and_cycle_is_rejected() {
        let mut editor = editor();
        let root = editor.doc.root;
        let group = Node::group("Group");
        let id = group.id;
        let before = editor.doc.clone();
        let result = editor.transact(
            "Invalid",
            vec![
                Op::InsertNode {
                    node: group.clone(),
                    parent: root,
                    index: 0,
                },
                Op::MoveNode {
                    id,
                    parent: id,
                    index: 0,
                },
            ],
            0,
        );
        assert!(matches!(result, Err(DocError::Invalid(_))));
        assert_eq!(editor.doc, before);
        assert_eq!(editor.undo_len(), 0);
    }

    #[test]
    fn tile_hash_changes_are_undone_without_pixel_copies() {
        let mut editor = editor();
        let root = editor.doc.root;
        let layer = Node::raster("Paint");
        let id = layer.id;
        editor
            .transact(
                "Add",
                vec![Op::InsertNode {
                    node: layer,
                    parent: root,
                    index: 0,
                }],
                0,
            )
            .unwrap();
        let base = editor.doc.clone();
        let key = TileKey { tx: -1, ty: 2 };
        let hash = ContentHash::of(b"tile pixels");
        let changed = editor
            .transact(
                "Stroke",
                vec![Op::SetTiles {
                    layer: id,
                    changes: vec![TileChange {
                        key,
                        before: None,
                        after: Some(hash),
                    }],
                }],
                1,
            )
            .unwrap();
        assert_eq!(changed.tiles, vec![(id, key)]);
        assert!(editor.undo().unwrap().is_some());
        assert_eq!(content(editor.doc.clone()), content(base));
        assert!(editor.redo().unwrap().is_some());
        let NodeKind::Raster { tiles } = &editor.doc.node(id).unwrap().kind else {
            panic!("raster expected")
        };
        assert_eq!(tiles.tiles.get(&key), Some(&hash));
    }

    #[test]
    fn lock_rejects_edits_and_a_clearing_patch_unlocks() {
        let mut editor = editor();
        let root = editor.doc.root;
        let layer = Node::raster("Paint");
        let id = layer.id;
        let child = Node::raster("Child");
        let child_id = child.id;
        editor
            .transact(
                "add",
                vec![
                    Op::InsertNode {
                        node: layer,
                        parent: root,
                        index: 0,
                    },
                    Op::InsertNode {
                        node: Node::group("Group"),
                        parent: root,
                        index: 1,
                    },
                ],
                0,
            )
            .unwrap();
        let group = editor.doc.node(root).unwrap().children().unwrap()[1];
        editor
            .transact(
                "child",
                vec![Op::InsertNode {
                    node: child,
                    parent: group,
                    index: 0,
                }],
                1,
            )
            .unwrap();
        let lock = |id| Op::SetProps {
            id,
            patch: PropPatch {
                locked: Some(true),
                ..Default::default()
            },
        };
        editor
            .transact("lock", vec![lock(id), lock(group)], 2)
            .unwrap();
        let rev = editor.doc.revision;
        let stroke = Op::SetTiles {
            layer: id,
            changes: vec![],
        };
        assert!(
            editor
                .transact("stroke", vec![stroke], rev)
                .unwrap_err()
                .to_string()
                .contains("locked")
        );
        assert!(
            editor
                .transact(
                    "opacity",
                    vec![Op::SetProps {
                        id,
                        patch: PropPatch {
                            opacity: Some(0.5),
                            ..Default::default()
                        },
                    }],
                    rev,
                )
                .is_err()
        );
        assert!(
            editor
                .transact("remove", vec![Op::RemoveNode { id }], rev)
                .is_err()
        );
        assert!(
            editor
                .transact(
                    "child stroke",
                    vec![Op::SetTiles {
                        layer: child_id,
                        changes: vec![],
                    }],
                    rev,
                )
                .is_err(),
            "a locked group blocks its children"
        );
        assert!(
            editor
                .transact(
                    "child unlock",
                    vec![Op::SetProps {
                        id: child_id,
                        patch: PropPatch {
                            locked: Some(false),
                            ..Default::default()
                        },
                    }],
                    rev,
                )
                .is_err(),
            "a child cannot unlock itself out of a locked group"
        );
        editor
            .transact(
                "unlock",
                vec![Op::SetProps {
                    id,
                    patch: PropPatch {
                        locked: Some(false),
                        ..Default::default()
                    },
                }],
                rev,
            )
            .unwrap();
        assert!(!editor.doc.node(id).unwrap().props.locked);
        editor.undo().unwrap();
        assert!(editor.doc.node(id).unwrap().props.locked);
        editor.undo().unwrap();
        assert!(!editor.doc.node(id).unwrap().props.locked);
        let rev = editor.doc.revision;
        editor
            .transact(
                "stroke",
                vec![Op::SetTiles {
                    layer: id,
                    changes: vec![],
                }],
                rev,
            )
            .unwrap();
    }

    #[test]
    fn locked_insert_rolls_back_when_a_later_op_fails() {
        let mut editor = editor();
        let root = editor.doc.root;
        let mut layer = Node::raster("Locked");
        layer.props.locked = true;
        let id = layer.id;
        let err = editor
            .transact(
                "insert then edit",
                vec![
                    Op::InsertNode {
                        node: layer,
                        parent: root,
                        index: 0,
                    },
                    Op::SetProps {
                        id,
                        patch: PropPatch {
                            opacity: Some(0.2),
                            ..Default::default()
                        },
                    },
                ],
                0,
            )
            .unwrap_err();
        assert!(err.to_string().contains("locked"), "{err}");
        assert!(editor.doc.node(id).is_err());
        assert_eq!(editor.doc.nodes.len(), 1);
        assert_eq!(editor.doc.revision, 0);
        assert!(editor.undo().unwrap().is_none());
    }

    #[test]
    fn undo_removes_a_node_inserted_already_locked() {
        let mut editor = editor();
        let root = editor.doc.root;
        let mut layer = Node::raster("Locked");
        layer.props.locked = true;
        let id = layer.id;
        editor
            .transact(
                "insert locked",
                vec![Op::InsertNode {
                    node: layer,
                    parent: root,
                    index: 0,
                }],
                0,
            )
            .unwrap();
        assert!(editor.doc.node(id).unwrap().props.locked);
        editor.undo().unwrap();
        assert!(editor.doc.node(id).is_err());
        editor.redo().unwrap();
        assert!(editor.doc.node(id).unwrap().props.locked);
    }

    #[test]
    fn unlock_patch_cannot_carry_other_fields() {
        let mut editor = editor();
        let root = editor.doc.root;
        let layer = Node::raster("Paint");
        let id = layer.id;
        editor
            .transact(
                "add",
                vec![Op::InsertNode {
                    node: layer,
                    parent: root,
                    index: 0,
                }],
                0,
            )
            .unwrap();
        editor
            .transact(
                "lock",
                vec![Op::SetProps {
                    id,
                    patch: PropPatch {
                        locked: Some(true),
                        ..Default::default()
                    },
                }],
                1,
            )
            .unwrap();
        let err = editor
            .transact(
                "unlock and fade",
                vec![Op::SetProps {
                    id,
                    patch: PropPatch {
                        locked: Some(false),
                        opacity: Some(0.4),
                        ..Default::default()
                    },
                }],
                2,
            )
            .unwrap_err();
        assert!(err.to_string().contains("locked"), "{err}");
        let node = editor.doc.node(id).unwrap();
        assert!(node.props.locked);
        assert_eq!(node.props.opacity, 1.0);
        editor
            .transact(
                "unlock then fade",
                vec![
                    Op::SetProps {
                        id,
                        patch: PropPatch {
                            locked: Some(false),
                            ..Default::default()
                        },
                    },
                    Op::SetProps {
                        id,
                        patch: PropPatch {
                            opacity: Some(0.4),
                            ..Default::default()
                        },
                    },
                ],
                2,
            )
            .unwrap();
        let node = editor.doc.node(id).unwrap();
        assert!(!node.props.locked);
        assert_eq!(node.props.opacity, 0.4);
    }

    #[test]
    fn replace_and_remove_respect_locks() {
        let mut editor = editor();
        let root = editor.doc.root;
        let group = Node::group("Group");
        let group_id = group.id;
        let mut child = Node::raster("Child");
        child.props.locked = true;
        let child_id = child.id;
        let sibling = Node::raster("Sibling");
        let sibling_id = sibling.id;
        editor
            .transact(
                "tree",
                vec![
                    Op::InsertNode {
                        node: group,
                        parent: root,
                        index: 0,
                    },
                    Op::InsertNode {
                        node: child,
                        parent: group_id,
                        index: 0,
                    },
                    Op::InsertNode {
                        node: sibling,
                        parent: root,
                        index: 1,
                    },
                ],
                0,
            )
            .unwrap();
        editor
            .transact(
                "lock sibling",
                vec![Op::SetProps {
                    id: sibling_id,
                    patch: PropPatch {
                        locked: Some(true),
                        ..Default::default()
                    },
                }],
                1,
            )
            .unwrap();
        let mut replacement = editor.doc.node(sibling_id).unwrap().clone();
        replacement.name = "Renamed".into();
        replacement.props.locked = false;
        let err = editor
            .transact(
                "replace",
                vec![Op::ReplaceNode {
                    id: sibling_id,
                    node: replacement,
                }],
                2,
            )
            .unwrap_err();
        assert!(err.to_string().contains("locked"), "{err}");
        assert_eq!(editor.doc.node(sibling_id).unwrap().name, "Sibling");
        assert!(editor.doc.node(sibling_id).unwrap().props.locked);
        let err = editor
            .transact("remove group", vec![Op::RemoveNode { id: group_id }], 2)
            .unwrap_err();
        assert!(err.to_string().contains("locked"), "{err}");
        assert!(editor.doc.node(child_id).is_ok());
        assert!(editor.doc.node(group_id).is_ok());
    }

    #[test]
    fn isolated_groups_stop_at_the_compositor_limit() {
        let mut editor = editor();
        let mut parent = editor.doc.root;
        let mut ops = vec![];
        for i in 0..MAX_ISOLATED_GROUPS {
            let mut node = Node::group(format!("g{i}"));
            node.props.opacity = 0.5;
            let id = node.id;
            ops.push(Op::InsertNode {
                node,
                parent,
                index: 0,
            });
            parent = id;
        }
        editor.transact("stack", ops, 0).unwrap();
        let mut extra = Node::group("one more");
        extra.props.opacity = 0.5;
        let err = editor
            .transact(
                "too deep",
                vec![Op::InsertNode {
                    node: extra,
                    parent,
                    index: 0,
                }],
                editor.doc.revision,
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("31"), "{err}");
        assert_eq!(editor.doc.nodes.len(), MAX_ISOLATED_GROUPS as usize + 1);
    }

    #[test]
    fn a_hidden_stack_can_exceed_the_compositor_until_it_is_shown() {
        let mut editor = editor();
        let mut parent = editor.doc.root;
        let mut ops = vec![];
        let mut top = None;
        for i in 0..40 {
            let mut node = Node::group(format!("h{i}"));
            node.props.opacity = 0.5;
            if i == 0 {
                node.props.visible = false;
                top = Some(node.id);
            }
            let id = node.id;
            ops.push(Op::InsertNode {
                node,
                parent,
                index: 0,
            });
            parent = id;
        }
        editor.transact("hidden", ops, 0).unwrap();
        let err = editor
            .transact(
                "show",
                vec![Op::SetProps {
                    id: top.unwrap(),
                    patch: PropPatch {
                        visible: Some(true),
                        ..Default::default()
                    },
                }],
                editor.doc.revision,
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("31"), "{err}");
        assert!(!editor.doc.node(top.unwrap()).unwrap().props.visible);
    }

    #[test]
    fn meta_rejects_unimplemented_color_and_depth() {
        let mut meta = DocumentMeta::srgb_u8(8, 8);
        meta.color = ColorSpace::DisplayP3;
        assert!(Document::new(meta).is_err());
        let mut meta = DocumentMeta::srgb_u8(8, 8);
        meta.depth = BitDepth::F16;
        assert!(Document::new(meta).is_err());
    }

    #[test]
    fn old_effect_shapes_open_and_round_trip() {
        let shapes = [
            r#"{"type":"gaussian_blur","radius":12}"#,
            r#"{"type":"gaussian_blur"}"#,
            r#"{"type":"gaussian_blur","radius":12,"legacy":true}"#,
            r#"{"type":"color_adjust","brightness":0.25,"contrast":1.5,"saturation":0.5}"#,
            r#"{"type":"color_adjust"}"#,
            r#"{"type":"levels","in_black":0.1,"in_white":0.9,"gamma":1.2,"out_black":0.0,"out_white":1.0}"#,
            r#"{"type":"hue_saturation","hue":20.0,"saturation":1.25,"lightness":-0.2}"#,
            r#"{"type":"invert"}"#,
            r#"{"type":"invert","selection":null,"note":"stored on the untyped format"}"#,
            r#"{"type":"curves","points":[{"x":0.0,"y":0.0},{"x":1.0,"y":1.0}]}"#,
            r#"{"type":"curves","channel":"red","points":[{"x":0,"y":0},{"x":0.5,"y":0.25},{"x":1,"y":1}]}"#,
        ];
        for raw in shapes {
            let effect: Effect = serde_json::from_str(raw).unwrap();
            effect.validate().unwrap();
            let again: Effect =
                serde_json::from_str(&serde_json::to_string(&effect).unwrap()).unwrap();
            assert_eq!(effect, again, "{raw}");
        }
        assert_eq!(
            serde_json::from_str::<Effect>(r#"{"type":"gaussian_blur"}"#).unwrap(),
            Effect::GaussianBlur {
                radius: 8.0,
                selection: None,
                extra: Default::default(),
            }
        );
        assert_eq!(
            serde_json::from_str::<Effect>(
                r#"{"type":"curves","points":[{"x":0,"y":0},{"x":1,"y":1}]}"#
            )
            .unwrap(),
            Effect::Curves {
                channel: CurveChannel::Master,
                points: vec![CurvePoint { x: 0.0, y: 0.0 }, CurvePoint { x: 1.0, y: 1.0 }],
                selection: None,
                extra: Default::default(),
            }
        );
        assert!(serde_json::from_str::<Effect>(r#"{"type":"not_an_effect"}"#).is_err());

        let mut editor = editor();
        let root = editor.doc.root;
        let mut layer = Node::raster("Old");
        layer.props.effects = vec![Effect::GaussianBlur {
            radius: 12.0,
            selection: None,
            extra: Default::default(),
        }];
        editor
            .transact(
                "add",
                vec![Op::InsertNode {
                    node: layer,
                    parent: root,
                    index: 0,
                }],
                0,
            )
            .unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&editor.doc.snapshot_bytes().unwrap()).unwrap();
        let nodes = value["nodes"].as_array_mut().unwrap();
        let layer_json = nodes.iter_mut().find(|node| node["name"] == "Old").unwrap();
        layer_json["props"]["effects"][0]["vendor"] = serde_json::json!("alto-0.3");
        let opened = Document::from_snapshot_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
        let opened_layer = opened
            .nodes
            .values()
            .find(|node| node.name == "Old")
            .unwrap();
        let Effect::GaussianBlur {
            radius,
            selection,
            extra,
        } = &opened_layer.props.effects[0]
        else {
            panic!("gaussian blur");
        };
        assert_eq!(*radius, 12.0);
        assert!(selection.is_none());
        assert_eq!(
            extra.get("vendor").and_then(|v| v.as_str()),
            Some("alto-0.3")
        );
        let written = serde_json::to_value(&opened_layer.props.effects[0]).unwrap();
        assert_eq!(written["vendor"], "alto-0.3");
        assert_eq!(written["radius"], 12.0);
        assert_eq!(written["type"], "gaussian_blur");
    }

    proptest! {
        #[test]
        fn reversible_op_preserves_document(name in "[a-zA-Z0-9]{1,32}", opacity in 0.0f32..1.0f32) {
            let mut editor = editor();
            let root = editor.doc.root;
            let node = Node::raster(name.clone());
            let id = node.id;
            let before = editor.doc.clone();
            editor.transact("insert and change", vec![
                Op::InsertNode { node, parent: root, index: 0 },
                Op::SetProps { id, patch: PropPatch { opacity: Some(opacity), ..Default::default() } },
            ], 0).unwrap();
            editor.undo().unwrap();
            prop_assert_eq!(content(editor.doc), content(before));
        }
    }
}
#[cfg(test)]
mod compatibility_tests {
    use super::*;
    #[test]
    fn future_node_payload_is_preserved() {
        let raw = serde_json::json!({"kind":"future_plugin_node","custom":{"strength":3},"asset":"opaque"});
        let kind: NodeKind = serde_json::from_value(raw.clone()).unwrap();
        assert!(matches!(kind, NodeKind::Unsupported { .. }));
        assert_eq!(serde_json::to_value(kind).unwrap(), raw);
    }
    #[test]
    fn invalid_known_node_is_not_silently_accepted() {
        assert!(
            serde_json::from_value::<NodeKind>(serde_json::json!({"kind":"raster","tiles":null}))
                .is_err()
        );
    }
}
