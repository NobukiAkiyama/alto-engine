//! The document tree: applying operations, validation and snapshots.
use super::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub meta: DocumentMeta,
    pub nodes: BTreeMap<NodeId, Node>,
    pub root: NodeId,
    pub assets: BTreeMap<AssetId, AssetRecord>,
    pub selection: Option<TileMap>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    meta: DocumentMeta,
    root: NodeId,
    nodes: Vec<Node>,
    assets: Vec<AssetRecord>,
    selection: Option<TileMap>,
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
    pub(crate) fn apply(&mut self, op: Op, enforce: bool) -> Result<(Op, ChangeSet), DocError> {
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
                let NodeKind::Raster { tiles, .. } = &node.kind else {
                    return Err(DocError::Invalid("Target must be raster".into()));
                };
                let old_node = node.clone();
                let NodeKind::Raster {
                    bounds: old_bounds, ..
                } = node.kind
                else {
                    unreachable!()
                };
                let mut new_bounds = old_bounds;
                let mut seen = BTreeSet::new();
                for item in &changes {
                    if !seen.insert(item.key) || tiles.tiles.get(&item.key).copied() != item.before
                    {
                        return Err(DocError::Invalid("Tile precondition failed".into()));
                    }
                }
                for item in &changes {
                    if item.after.is_some() {
                        new_bounds = new_bounds.union(ImageBounds::tile(item.key)?)?;
                    }
                }
                let NodeKind::Raster { tiles, bounds } = &mut self.node_mut(layer)?.kind else {
                    unreachable!()
                };
                *bounds = new_bounds;
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
                Op::ReplaceNode {
                    id: layer,
                    node: old_node,
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
            Op::SetText { id, content, bake } => {
                content
                    .validate()
                    .map_err(|e| DocError::Invalid(e.to_string()))?;
                let NodeKind::Text {
                    content: old,
                    bake: old_bake,
                } = &mut self.node_mut(id)?.kind
                else {
                    return Err(DocError::Invalid("Target must be text".into()));
                };
                change.nodes.insert(id);
                Op::SetText {
                    id,
                    content: std::mem::replace(old, content),
                    bake: std::mem::replace(old_bake, bake),
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
        let snapshot = Snapshot {
            meta: self.meta.clone(),
            root: self.root,
            nodes: ordered,
            assets: self.assets.values().cloned().collect(),
            selection: self.selection.clone(),
        };
        // Pretty, tree-ordered JSON: identical documents give identical bytes, and the
        // folder format produces line-oriented Git diffs.
        alto_core::json::canonical_bytes(&snapshot)
            .map_err(|error| DocError::Serialization(error.to_string()))
    }
    pub fn from_snapshot_bytes(bytes: &[u8]) -> Result<Self, DocError> {
        let snapshot: Snapshot = alto_core::json::from_slice(bytes)
            .map_err(|error| DocError::Serialization(error.to_string()))?;
        snapshot.meta.validate()?;
        let asset_count = snapshot.assets.len();
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
        if doc.assets.len() != asset_count {
            return Err(DocError::Invalid("Duplicate asset ID".into()));
        }
        doc.validate_graph()?;
        Ok(doc)
    }
    pub fn validate_graph(&self) -> Result<(), DocError> {
        self.meta.validate()?;
        if self.assets.iter().any(|(id, a)| *id != a.id) {
            return Err(DocError::Invalid("asset map key/id mismatch".into()));
        }
        if self.nodes.iter().any(|(id, n)| *id != n.id) {
            return Err(DocError::Invalid("node map key/id mismatch".into()));
        }
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
            if let NodeKind::Raster { bounds, tiles } = &node.kind {
                bounds.validate()?;
                if tiles.format != PixelFormat::Rgba8 {
                    return Err(DocError::Invalid("raster requires RGBA8".into()));
                }
                for key in tiles.tiles.keys() {
                    let tile = ImageBounds::tile(*key)?;
                    if tile.x >= bounds.right()
                        || tile.y >= bounds.bottom()
                        || tile.right() <= bounds.x
                        || tile.bottom() <= bounds.y
                    {
                        return Err(DocError::Invalid(
                            "raster tile does not intersect image bounds".into(),
                        ));
                    }
                }
            }
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
            if let NodeKind::Text { content, .. } = &node.kind {
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

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    #[test]
    fn future_node_payload_is_rejected() {
        let raw = serde_json::json!({"kind":"future_plugin_node","custom":{"strength":3}});
        assert!(
            serde_json::from_value::<NodeKind>(raw)
                .unwrap_err()
                .to_string()
                .contains("UNSUPPORTED")
        );
    }
    #[test]
    fn invalid_known_node_is_not_silently_accepted() {
        assert!(
            serde_json::from_value::<NodeKind>(serde_json::json!({"kind":"raster","tiles":null}))
                .is_err()
        );
    }

    #[test]
    fn only_render_version_three_and_format_five_load() {
        let bytes = Document::new(DocumentMeta::srgb_u8(8, 8))
            .unwrap()
            .snapshot_bytes()
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let load = |meta: serde_json::Value| {
            let mut v = value.clone();
            v["meta"] = meta;
            Document::from_snapshot_bytes(&serde_json::to_vec(&v).unwrap())
                .unwrap_err()
                .to_string()
        };
        let mut meta = value["meta"].clone();
        // Render version 1 files had no `render_version`.
        meta.as_object_mut().unwrap().remove("render_version");
        meta["format_version"] = serde_json::json!(1);
        assert!(load(meta).contains("render_version"));
        for (format, render, message) in [
            (3, 2, "Unsupported document format"),
            (5, 2, "Unsupported render version"),
            (5, 1, "Unsupported render version"),
            (4, 3, "Unsupported document format"),
            (5, 4, "Unsupported render version"),
        ] {
            let mut meta = value["meta"].clone();
            meta["format_version"] = serde_json::json!(format);
            meta["render_version"] = serde_json::json!(render);
            let err = load(meta);
            assert!(err.contains(message), "{format}/{render}: {err}");
        }
    }

    #[test]
    fn render_version_three_saves_format_five_and_reloads() {
        let doc = Document::new(DocumentMeta::srgb_u8(8, 8)).unwrap();
        let bytes = doc.snapshot_bytes().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["meta"]["format_version"], 5);
        assert_eq!(value["meta"]["render_version"], 3);
        let loaded = Document::from_snapshot_bytes(&bytes).unwrap();
        assert_eq!(loaded.meta.render_version, alto_core::RENDER_VERSION_EXACT);
        assert_eq!(loaded.meta.format_version, 5);
        assert_eq!(loaded.snapshot_bytes().unwrap(), bytes);
    }
}
