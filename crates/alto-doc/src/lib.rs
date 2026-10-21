use alto_core::{
    Affine2, AssetId, BitDepth, ColorSpace, ContentHash, MAX_ISOLATED_GROUPS, NodeId, PixelFormat,
    Rect, TILE, TileKey,
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
    pub effects: Vec<serde_json::Value>,
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
pub fn validate_effect(value: &serde_json::Value) -> Result<(), DocError> {
    let Some(obj) = value.as_object() else {
        return Err(DocError::Invalid("Unknown effect type".into()));
    };
    let kind = obj.get("type").and_then(|v| v.as_str());
    let allowed: &[&str] = match kind {
        Some("gaussian_blur") => &["type", "radius", "selection"],
        Some("color_adjust") => &["type", "brightness", "contrast", "saturation", "selection"],
        Some("levels") => &[
            "type",
            "in_black",
            "in_white",
            "gamma",
            "out_black",
            "out_white",
            "selection",
        ],
        Some("hue_saturation") => &["type", "hue", "saturation", "lightness", "selection"],
        Some("invert") => &["type", "selection"],
        Some("curves") => &["type", "channel", "points", "selection"],
        _ => return Err(DocError::Invalid("Unknown effect type".into())),
    };
    if let Some(key) = obj.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(DocError::Invalid(format!("Unknown effect field {key}")));
    }
    let finite = |key: &str, default: f64, min: f64, max: f64| {
        let n = value
            .get(key)
            .map_or(Some(default), |v| v.as_f64())
            .ok_or_else(|| DocError::Invalid(format!("Invalid {key}")))?;
        if !n.is_finite() || !(min..=max).contains(&n) {
            return Err(DocError::Invalid(format!("Invalid {key}")));
        }
        Ok(())
    };
    match value.get("type").and_then(|v| v.as_str()) {
        Some("gaussian_blur") => finite("radius", 8.0, 0.0, 64.0)?,
        Some("color_adjust") => {
            finite("brightness", 0.0, -1.0, 1.0)?;
            finite("contrast", 1.0, 0.0, 4.0)?;
            finite("saturation", 1.0, 0.0, 4.0)?;
        }
        Some("levels") => {
            finite("in_black", 0.0, 0.0, 1.0)?;
            finite("in_white", 1.0, 0.0, 1.0)?;
            finite("gamma", 1.0, 0.1, 10.0)?;
            finite("out_black", 0.0, 0.0, 1.0)?;
            finite("out_white", 1.0, 0.0, 1.0)?;
            let black = value
                .get("in_black")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let white = value
                .get("in_white")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0);
            if black >= white {
                return Err(DocError::Invalid("Invalid levels range".into()));
            }
        }
        Some("hue_saturation") => {
            finite("hue", 0.0, -180.0, 180.0)?;
            finite("saturation", 1.0, 0.0, 4.0)?;
            finite("lightness", 0.0, -1.0, 1.0)?;
        }
        Some("invert") => {}
        Some("curves") => validate_curves(value)?,
        _ => return Err(DocError::Invalid("Unknown effect type".into())),
    }
    if let Some(v) = value.get("selection").filter(|v| !v.is_null()) {
        let map: TileMap =
            serde_json::from_value(v.clone()).map_err(|e| DocError::Invalid(e.to_string()))?;
        validate_mask(&map)?;
    }
    Ok(())
}

fn validate_curves(value: &serde_json::Value) -> Result<(), DocError> {
    let channel = value
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("master");
    if !matches!(channel, "master" | "red" | "green" | "blue") {
        return Err(DocError::Invalid("Invalid curve channel".into()));
    }
    let points = value
        .get("points")
        .and_then(|v| v.as_array())
        .ok_or_else(|| DocError::Invalid("Curve points required".into()))?;
    if !(2..=16).contains(&points.len()) {
        return Err(DocError::Invalid("Curve point count".into()));
    }
    let mut previous = -1.0;
    for point in points {
        let x = point
            .get("x")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| DocError::Invalid("Invalid curve point".into()))?;
        let y = point
            .get("y")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| DocError::Invalid("Invalid curve point".into()))?;
        if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) || x <= previous {
            return Err(DocError::Invalid("Invalid curve point".into()));
        }
        previous = x;
    }
    Ok(())
}

pub fn extended_effect(value: &serde_json::Value) -> bool {
    matches!(
        value.get("type").and_then(|v| v.as_str()),
        Some("levels" | "curves" | "hue_saturation" | "invert")
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
        adjustment: serde_json::Value,
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
    pub effects: Option<Vec<serde_json::Value>>,
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
        adjustment: serde_json::Value,
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
}
impl ChangeSet {
    fn merge(&mut self, other: Self) {
        self.nodes.extend(other.nodes);
        self.structure_changed |= other.structure_changed;
        self.dirty.extend(other.dirty);
        self.tiles.extend(other.tiles);
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum DocError {
    #[error("invalid operation: {0}")]
    Invalid(String),
    #[error("node not found: {0:?}")]
    NotFound(NodeId),
    #[error("revision conflict: expected {expected}, actual {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error("serialization error: {0}")]
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
            if hops > 128 {
                return Err(DocError::Invalid("Group nesting exceeds 128".into()));
            }
            let node = self.node(current)?;
            if node.props.locked {
                return Ok(true);
            }
            cursor = node.parent;
        }
        Ok(false)
    }
    /// A locked node rejects every edit except a `SetProps` that clears its own
    /// `locked` flag. That exception is what makes undo of a lock possible, and
    /// it is also how a caller explicitly unlocks in the same patch. A locked
    /// ancestor rejects the clear as well: the child cannot unlock itself out
    /// of a locked group.
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
            Op::RemoveNode { id }
            | Op::SetMask { layer: id, .. }
            | Op::SetTiles { layer: id, .. }
            | Op::SetAdjustment { id, .. }
            | Op::SetVector { id, .. }
            | Op::SetVectorMask { id, .. }
            | Op::SetClipPath { id, .. }
            | Op::SetText { id, .. } => editable(self, *id),
            Op::MoveNode { id, parent, .. } => {
                editable(self, *id)?;
                editable(self, *parent)
            }
            Op::SetProps { id, patch } => {
                if patch.locked == Some(false) {
                    if self.ancestor_locked(*id)? {
                        Err(locked())
                    } else {
                        Ok(())
                    }
                } else {
                    editable(self, *id)
                }
            }
            Op::ReplaceNode { id, node } => {
                if self.ancestor_locked(*id)? {
                    return Err(locked());
                }
                if self.node(*id)?.props.locked && node.props.locked {
                    return Err(locked());
                }
                Ok(())
            }
            Op::SetSelection { .. } | Op::SetAsset { .. } | Op::SetMeta { .. } => Ok(()),
        }
    }
    fn apply(&mut self, op: Op) -> Result<(Op, ChangeSet), DocError> {
        self.enforce_lock(&op)?;
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
                Op::SetAsset { id, record: old }
            }
            Op::SetMeta { meta } => {
                meta.validate()?;
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
            if depth > 128 {
                return Err(DocError::Invalid("Group nesting exceeds 128".into()));
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
    fn replay(&mut self, ops: Vec<Op>) -> Result<(Vec<AppliedOp>, ChangeSet), DocError> {
        let revision = self.doc.revision;
        let mut applied = Vec::with_capacity(ops.len());
        let mut changes = ChangeSet::default();
        for op in ops {
            match self.doc.apply(op.clone()) {
                Ok((inverse, change)) => {
                    applied.push(AppliedOp { op, inverse });
                    changes.merge(change);
                }
                Err(e) => {
                    self.revert(&applied);
                    self.doc.revision = revision;
                    return Err(e);
                }
            }
        }
        if let Err(e) = self.doc.validate_graph() {
            self.revert(&applied);
            self.doc.revision = revision;
            return Err(e);
        }
        Ok((applied, changes))
    }
    fn revert(&mut self, applied: &[AppliedOp]) {
        for item in applied.iter().rev() {
            self.doc
                .apply(item.inverse.clone())
                .expect("inverse of an applied edit");
        }
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
        let (applied, changes) = self.replay(ops)?;
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
        let (_, changes) = self.replay(inverses)?;
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
        let (_, changes) = self.replay(ops)?;
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
    pub fn retained_ops(&self) -> Vec<Op> {
        self.undo
            .iter()
            .chain(&self.redo)
            .flat_map(|t| t.ops.iter().flat_map(|o| [o.op.clone(), o.inverse.clone()]))
            .collect()
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
    fn effect_rejects_unknown_fields() {
        let err = validate_effect(&serde_json::json!({"type":"invert","extra":1}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("extra"), "{err}");
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
