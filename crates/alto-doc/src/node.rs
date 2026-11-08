//! Layer properties, nodes and asset records.
use super::*;

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
#[serde(deny_unknown_fields)]
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
    pub(crate) fn validate(&self) -> Result<(), DocError> {
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

#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    Group {
        children: Vec<NodeId>,
        isolation: Isolation,
    },
    Raster {
        bounds: ImageBounds,
        tiles: TileMap,
    },
    Vector {
        paths: Vec<StyledPath>,
    },
    Text {
        content: Box<TextContent>,
        /// The content's glyphs baked to integers (an `ALTB` blob,
        /// doc/design/2026-10-28-v3-integer-text.md "焼き込み"), which is all a
        /// render reads. `None` until baked, and then not serialized.
        bake: Option<ContentHash>,
    },
    Adjustment {
        adjustment: Effect,
    },
    Unsupported {
        raw: serde_json::Value,
    },
}

impl NodeKind {
    pub fn raster(tiles: TileMap) -> Result<Self, DocError> {
        let mut bounds: Option<ImageBounds> = None;
        for key in tiles.tiles.keys() {
            let b = ImageBounds::tile(*key)?;
            bounds = Some(match bounds {
                Some(a) => a.union(b)?,
                None => b,
            });
        }
        Ok(Self::Raster {
            bounds: bounds.unwrap_or_default(),
            tiles,
        })
    }
}
impl Serialize for NodeKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let value = match self {
            Self::Group {
                children,
                isolation,
            } => serde_json::json!({"kind":"group","children":children,"isolation":isolation}),
            Self::Raster { tiles, bounds } => {
                serde_json::json!({"kind":"raster","bounds":bounds,"tiles":tiles})
            }
            Self::Vector { paths } => serde_json::json!({"kind":"vector","paths":paths}),
            Self::Text {
                content,
                bake: None,
            } => serde_json::json!({"kind":"text","content":content}),
            Self::Text {
                content,
                bake: Some(bake),
            } => serde_json::json!({"kind":"text","content":content,"bake":bake}),
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
        let allowed: &[&str] = match v.get("kind").and_then(|k| k.as_str()) {
            Some("group") => &["kind", "children", "isolation"],
            Some("raster") => &["kind", "bounds", "tiles"],
            Some("vector") => &["kind", "paths"],
            Some("text") => &["kind", "content", "bake"],
            Some("adjustment") => &["kind", "adjustment"],
            _ => &[],
        };
        if v.as_object()
            .is_some_and(|m| m.keys().any(|key| !allowed.contains(&key.as_str())))
        {
            return Err(serde::de::Error::custom("UNSUPPORTED: node field or kind"));
        }
        let result: Result<Self, String> = match v.get("kind").and_then(|k| k.as_str()) {
            Some("group") => field(&v, "children").and_then(|children| {
                field(&v, "isolation").map(|isolation| Self::Group {
                    children,
                    isolation,
                })
            }),
            Some("raster") => field(&v, "tiles")
                .and_then(|tiles| field(&v, "bounds").map(|bounds| Self::Raster { tiles, bounds })),
            Some("vector") => field(&v, "paths").map(|paths| Self::Vector { paths }),
            Some("text") => field(&v, "content").and_then(|content| {
                let bake = match v.get("bake") {
                    None => None,
                    Some(_) => Some(field(&v, "bake")?),
                };
                Ok(Self::Text { content, bake })
            }),
            Some("adjustment") => {
                field(&v, "adjustment").map(|adjustment| Self::Adjustment { adjustment })
            }
            Some(_) => Err("UNSUPPORTED: node kind".into()),
            None => Err("missing node kind".into()),
        };
        result.map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
                bounds: ImageBounds::default(),
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
            bake: None,
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
    pub(crate) fn children_mut(&mut self) -> Option<&mut Vec<NodeId>> {
        match &mut self.kind {
            NodeKind::Group { children, .. } => Some(children),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRecord {
    pub id: AssetId,
    pub hash: ContentHash,
    pub kind: AssetKind,
    pub byte_len: u64,
    pub dims: Option<(u32, u32)>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
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
