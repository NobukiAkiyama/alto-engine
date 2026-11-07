//! Edit operations and the change sets they report.
use super::*;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    pub(crate) fn clears_lock_only(&self) -> bool {
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
#[serde(deny_unknown_fields)]
pub struct TileChange {
    pub key: TileKey,
    pub before: Option<ContentHash>,
    pub after: Option<ContentHash>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
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
        /// The content's bake, set in the same op (render version 3 only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bake: Option<ContentHash>,
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
    pub(crate) fn merge(&mut self, other: Self) {
        self.nodes.extend(other.nodes);
        self.structure_changed |= other.structure_changed;
        self.dirty.extend(other.dirty);
        self.tiles.extend(other.tiles);
        self.selection_changed |= other.selection_changed;
        self.assets_changed |= other.assets_changed;
        self.meta_changed |= other.meta_changed;
    }
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
