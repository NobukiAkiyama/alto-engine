mod edit;
pub mod m5;
pub mod text;
pub mod versions;
use alto_core::ContentHash;
use alto_doc::{BlendMode, ChangeSet, DocError, Document, DocumentMeta, Editor, NodeKind, Op};
use alto_raster::{MemoryTileStore, TileData, TileStore};
use alto_scene::{RContent, RNode, RenderBlend, RenderScene, RenderTile};
use std::collections::BTreeMap;
pub use text::{CaretInfo, ImportedFont, TextInfo};

fn render_blend(mode: BlendMode) -> RenderBlend {
    match mode {
        BlendMode::Normal => RenderBlend::Normal,
        BlendMode::Multiply => RenderBlend::Multiply,
        BlendMode::Screen => RenderBlend::Screen,
        BlendMode::Overlay => RenderBlend::Overlay,
        BlendMode::Darken => RenderBlend::Darken,
        BlendMode::Lighten => RenderBlend::Lighten,
        BlendMode::ColorDodge => RenderBlend::ColorDodge,
        BlendMode::ColorBurn => RenderBlend::ColorBurn,
        BlendMode::HardLight => RenderBlend::HardLight,
        BlendMode::SoftLight => RenderBlend::SoftLight,
        BlendMode::Difference => RenderBlend::Difference,
        BlendMode::Exclusion => RenderBlend::Exclusion,
        BlendMode::Hue => RenderBlend::Hue,
        BlendMode::Saturation => RenderBlend::Saturation,
        BlendMode::Color => RenderBlend::Color,
        BlendMode::Luminosity => RenderBlend::Luminosity,
        BlendMode::Add => RenderBlend::Add,
    }
}
pub use versions::{CommitView, VersionRef, VersionStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocHandle(pub u64);

#[derive(Debug, thiserror::Error)]
/// Messages start with the code the SDK reports (`CODE: detail`).
pub enum EngineError {
    #[error("NOT_FOUND: document not found: {0:?}")]
    NotFound(DocHandle),
    #[error("NOT_FOUND: content tile not found: {0}")]
    MissingTile(ContentHash),
    #[error("INVALID_ARGUMENT: canvas dimensions exceed renderer coordinate range")]
    InvalidDimensions,
    #[error(transparent)]
    Document(#[from] DocError),
    #[error(transparent)]
    Storage(#[from] alto_store::StoreError),
    #[error(transparent)]
    Tile(#[from] alto_raster::TileError),
}

pub struct Engine {
    documents: BTreeMap<DocHandle, Editor>,
    next_handle: u64,
    tiles: MemoryTileStore,
    blobs: BTreeMap<ContentHash, Vec<u8>>,
    imported: std::collections::BTreeSet<ContentHash>,
    /// Blobs stored by import and not yet referenced by a committed transaction.
    pending_blobs: std::collections::BTreeSet<ContentHash>,
    versions: BTreeMap<DocHandle, versions::VersionStore>,
    empty_versions: versions::VersionStore,
    text: text::TextCache,
    /// Set once a package has been saved or opened, so history tiles can leave RAM.
    backed: bool,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn clear_decoded_tile_cache(&mut self) {
        self.tiles.clear_decoded_cache();
    }
    pub fn new() -> Self {
        Self {
            documents: BTreeMap::new(),
            next_handle: 1,
            tiles: MemoryTileStore::default(),
            blobs: BTreeMap::new(),
            imported: std::collections::BTreeSet::new(),
            pending_blobs: std::collections::BTreeSet::new(),
            versions: BTreeMap::new(),
            empty_versions: Default::default(),
            text: Default::default(),
            backed: false,
        }
    }

    pub fn create_document(&mut self, meta: DocumentMeta) -> Result<DocHandle, EngineError> {
        self.add_document(Document::new(meta)?)
    }

    fn add_document(&mut self, doc: Document) -> Result<DocHandle, EngineError> {
        let handle = DocHandle(self.next_handle);
        self.next_handle = self
            .next_handle
            .checked_add(1)
            .ok_or_else(|| DocError::Invalid("Document handle overflow".into()))?;
        self.documents.insert(handle, Editor::new(doc));
        Ok(handle)
    }

    pub fn document(&self, handle: DocHandle) -> Result<&Document, EngineError> {
        self.documents
            .get(&handle)
            .map(|editor| &editor.doc)
            .ok_or(EngineError::NotFound(handle))
    }

    pub fn put_tile(&mut self, tile: TileData) -> Option<ContentHash> {
        if tile.is_empty() {
            None
        } else {
            Some(self.tiles.put(tile))
        }
    }

    pub fn tile(
        &self,
        hash: &ContentHash,
    ) -> Result<Option<std::sync::Arc<TileData>>, EngineError> {
        Ok(self.tiles.get(hash)?)
    }
    /// For tiles the document references: absence is an error, not transparency.
    pub fn require_tile(
        &self,
        hash: &ContentHash,
    ) -> Result<std::sync::Arc<TileData>, EngineError> {
        self.tiles.get(hash)?.ok_or(EngineError::MissingTile(*hash))
    }

    pub fn transact(
        &mut self,
        handle: DocHandle,
        intent: impl Into<String>,
        ops: Vec<Op>,
        base_revision: u64,
    ) -> Result<ChangeSet, EngineError> {
        let mut referenced = std::collections::BTreeSet::new();
        for op in &ops {
            op_hashes(op, &mut referenced);
        }
        for hash in referenced {
            if !self.tiles.reachable(&hash) && !self.blobs.contains_key(&hash) {
                return Err(EngineError::MissingTile(hash));
            }
        }
        for op in &ops {
            if let Op::SetTiles { changes, .. } = op {
                for change in changes {
                    if let Some(hash) = change.after
                        && !self.tiles.contains(&hash)
                    {
                        return Err(EngineError::MissingTile(hash));
                    }
                }
            }
        }
        let changes = self
            .documents
            .get_mut(&handle)
            .ok_or(EngineError::NotFound(handle))?
            .transact(intent, ops, base_revision)?;
        self.maintain();
        self.unpin_referenced();
        Ok(changes)
    }
    pub fn undo(&mut self, handle: DocHandle) -> Result<Option<ChangeSet>, EngineError> {
        let c = self
            .documents
            .get_mut(&handle)
            .ok_or(EngineError::NotFound(handle))?
            .undo()?;
        self.maintain();
        self.unpin_referenced();
        Ok(c)
    }
    pub fn redo(&mut self, handle: DocHandle) -> Result<Option<ChangeSet>, EngineError> {
        let c = self
            .documents
            .get_mut(&handle)
            .ok_or(EngineError::NotFound(handle))?
            .redo()?;
        self.maintain();
        self.unpin_referenced();
        Ok(c)
    }

    pub fn snapshot(&self, handle: DocHandle) -> Result<Vec<u8>, EngineError> {
        Ok(self.document(handle)?.snapshot_bytes()?)
    }
    pub fn open_snapshot(&mut self, bytes: &[u8]) -> Result<DocHandle, EngineError> {
        self.add_document(Document::from_snapshot_bytes(bytes)?)
    }

    pub fn render_scene(&self, handle: DocHandle) -> Result<RenderScene, EngineError> {
        let doc = self.document(handle)?;
        let width = i32::try_from(doc.meta.width).map_err(|_| EngineError::InvalidDimensions)?;
        let height = i32::try_from(doc.meta.height).map_err(|_| EngineError::InvalidDimensions)?;
        let mut nodes = Vec::new();
        fn append(
            engine: &Engine,
            doc: &Document,
            id: alto_core::NodeId,
            nodes: &mut Vec<RNode>,
        ) -> Result<usize, DocError> {
            let node = doc.node(id)?;
            let index = nodes.len();
            let transform = node.props.transform.0;
            let blend = render_blend(node.props.blend);
            nodes.push(RNode {
                key: id,
                version: doc.revision,
                visible: node.props.visible,
                transform,
                opacity: node.props.opacity,
                blend,
                effects: node.props.effects.len(),
                features: alto_scene::RFeatures {
                    mask: node
                        .props
                        .mask
                        .as_ref()
                        .filter(|_| node.props.mask_enabled)
                        .map(|m| {
                            let mut mask = m5::render_mask(m, node.props.mask_linked);
                            mask.transform = node.props.mask_transform.0;
                            mask
                        }),
                    vector_mask: node
                        .props
                        .vector_mask
                        .clone()
                        .filter(|m| m.enabled)
                        .map(|m| *m),
                    clip_path: node
                        .props
                        .clip_path
                        .clone()
                        .filter(|m| m.enabled)
                        .map(|m| *m),
                    clip: node.props.clip_to_below,
                    effects: node.props.effects.iter().map(m5::render_effect).collect(),
                },
                content: RContent::Unsupported,
            });
            let content = match &node.kind {
                NodeKind::Group {
                    children,
                    isolation,
                } => {
                    let mut indices = Vec::with_capacity(children.len());
                    for child in children {
                        indices.push(append(engine, doc, *child, nodes)?);
                    }
                    RContent::Group {
                        children: indices,
                        isolated: matches!(isolation, alto_doc::Isolation::Isolated),
                    }
                }
                NodeKind::Raster { tiles } => RContent::Tiles(
                    tiles
                        .tiles
                        .iter()
                        .map(|(key, hash)| RenderTile {
                            key: *key,
                            hash: *hash,
                        })
                        .collect(),
                ),
                NodeKind::Vector { paths } => RContent::Mesh(std::sync::Arc::new(paths.clone())),
                NodeKind::Adjustment { adjustment } => {
                    RContent::Adjustment(m5::render_effect(adjustment))
                }
                // Text renders per size run: large glyphs through the vector path (glyph
                // outlines -> lyon -> 4x MSAA), small ones through the glyph atlas.
                NodeKind::Text { content } => {
                    RContent::Text(engine.text_entry(doc, content).runs.clone())
                }
                NodeKind::Unsupported { .. } => RContent::Unsupported,
            };
            nodes[index].content = content;
            Ok(index)
        }
        let root = append(self, doc, doc.root, &mut nodes)?;
        Ok(RenderScene {
            canvas: alto_core::IRect {
                x0: 0,
                y0: 0,
                x1: width,
                y1: height,
            },
            root,
            nodes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alto_core::{PixelFormat, TILE, TileKey};
    use alto_doc::{Node, NodeKind, TileChange};

    #[test]
    fn engine_integrates_tiles_document_and_history() {
        let mut engine = Engine::new();
        let handle = engine
            .create_document(DocumentMeta::srgb_u8(512, 512))
            .unwrap();
        let layer = Node::raster("Paint");
        let id = layer.id;
        let root = engine.document(handle).unwrap().root;
        engine
            .transact(
                handle,
                "Layer",
                vec![Op::InsertNode {
                    node: layer,
                    parent: root,
                    index: 0,
                }],
                0,
            )
            .unwrap();
        let mut pixels = vec![0; (TILE * TILE * 4) as usize];
        pixels[0..4].copy_from_slice(&[255, 0, 0, 255]);
        let hash = engine
            .put_tile(TileData::new(PixelFormat::Rgba8, pixels).unwrap())
            .unwrap();
        let key = TileKey { tx: 0, ty: 0 };
        engine
            .transact(
                handle,
                "Paint",
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
        let scene = engine.render_scene(handle).unwrap();
        assert_eq!(scene.nodes.len(), 2);
        let RContent::Tiles(render_tiles) = &scene.nodes[1].content else {
            panic!("render tiles expected")
        };
        assert_eq!(render_tiles[0].hash, hash);
        assert_eq!(scene.compile(scene.canvas).unwrap().passes.len(), 2);
        let bytes = engine.snapshot(handle).unwrap();
        let reopened = engine.open_snapshot(&bytes).unwrap();
        let NodeKind::Raster { tiles } = &engine.document(reopened).unwrap().node(id).unwrap().kind
        else {
            panic!("raster expected")
        };
        assert_eq!(tiles.tiles.get(&key), Some(&hash));
        engine.undo(handle).unwrap();
        let NodeKind::Raster { tiles } = &engine.document(handle).unwrap().node(id).unwrap().kind
        else {
            panic!("raster expected")
        };
        assert!(tiles.tiles.is_empty());
        engine.redo(handle).unwrap();
        assert_eq!(engine.snapshot(handle).unwrap(), bytes);
    }
}

impl TileStore for Engine {
    fn is_opaque(&self, h: &ContentHash) -> bool {
        self.tiles.is_opaque(h)
    }
    fn get(
        &self,
        h: &ContentHash,
    ) -> Result<Option<std::sync::Arc<TileData>>, alto_raster::TileError> {
        self.tiles.get(h)
    }
    fn put(&mut self, d: TileData) -> ContentHash {
        self.tiles.put(d)
    }
}
impl Engine {
    pub fn history_state(&self, h: DocHandle) -> Result<(usize, usize), EngineError> {
        let e = self.documents.get(&h).ok_or(EngineError::NotFound(h))?;
        Ok((e.undo_len(), e.redo_len()))
    }
    pub fn undo_operations(&self, h: DocHandle) -> Result<Vec<Op>, EngineError> {
        Ok(self
            .documents
            .get(&h)
            .ok_or(EngineError::NotFound(h))?
            .undo_ops())
    }
    pub fn redo_operations(&self, h: DocHandle) -> Result<Vec<Op>, EngineError> {
        Ok(self
            .documents
            .get(&h)
            .ok_or(EngineError::NotFound(h))?
            .redo_ops())
    }
    pub fn put_blob(&mut self, bytes: Vec<u8>) -> ContentHash {
        let h = ContentHash::of(&bytes);
        self.blobs.entry(h).or_insert(bytes);
        h
    }
    /// Stores bytes that a later `SetAsset` will reference.
    /// The pin survives unrelated transactions. It comes off when a document,
    /// the undo stack, or a version references the hash. `release_import`
    /// abandons one pin, and closing the last document abandons the rest.
    pub fn put_import_blob(&mut self, bytes: Vec<u8>) -> ContentHash {
        let hash = self.put_blob(bytes);
        self.pending_blobs.insert(hash);
        hash
    }
    /// Drops one import pin. The bytes are removed when nothing else references them.
    pub fn release_import(&mut self, hash: ContentHash) -> bool {
        if !self.pending_blobs.remove(&hash) {
            return false;
        }
        if !self.rooted_hashes().contains(&hash) {
            self.blobs.remove(&hash);
        }
        true
    }
    /// Removes a document and its versions. The last document to close abandons
    /// import pins that were never referenced.
    pub fn close_document(&mut self, handle: DocHandle) -> Result<(), EngineError> {
        if self.documents.remove(&handle).is_none() {
            return Err(EngineError::NotFound(handle));
        }
        self.versions.remove(&handle);
        if self.documents.is_empty() {
            let referenced = self.rooted_hashes();
            let pending = std::mem::take(&mut self.pending_blobs);
            for hash in pending {
                if !referenced.contains(&hash) {
                    self.blobs.remove(&hash);
                }
            }
        }
        self.maintain();
        Ok(())
    }
    fn encoded(&self, h: ContentHash) -> Result<Option<Vec<u8>>, EngineError> {
        if let Some(t) = self.tile(&h)? {
            Ok(Some(alto_store::encode_object(
                match t.format {
                    alto_core::PixelFormat::Rgba8 => 4,
                    alto_core::PixelFormat::R8 => 1,
                },
                &t.pixels,
            )))
        } else {
            Ok(self.blobs.get(&h).map(|b| alto_store::encode_object(0, b)))
        }
    }
    pub fn save_package(&mut self, h: DocHandle) -> Result<Vec<u8>, EngineError> {
        let bytes = alto_store::encode_parts(&self.package_parts(h)?)?;
        self.backed = true;
        self.maintain();
        Ok(bytes)
    }
    pub fn tile_resident(&self, hash: &ContentHash) -> bool {
        self.tiles.contains(hash)
    }
    pub fn resident_bytes(&self) -> usize {
        self.tiles.allocated_bytes()
    }
    #[cfg(test)]
    pub fn tamper_parked_tile(&mut self, hash: &ContentHash) -> bool {
        self.tiles.tamper_cold(hash)
    }
    pub fn install_object(&mut self, hash: ContentHash, encoded: &[u8]) -> Result<(), EngineError> {
        let (tag, data) = alto_store::decode_object(encoded)?;
        if alto_store::object_hash(tag, &data) != hash {
            return Err(alto_store::StoreError::Corrupt("hash mismatch".into()).into());
        }
        match tag {
            0 => {
                self.blobs.insert(hash, data);
            }
            1 | 4 => {
                let format = if tag == 4 {
                    alto_core::PixelFormat::Rgba8
                } else {
                    alto_core::PixelFormat::R8
                };
                let tile = TileData::new(format, data)
                    .map_err(|_| alto_store::StoreError::Corrupt("tile length".into()))?;
                self.tiles.put(tile);
            }
            _ => return Err(alto_store::StoreError::Corrupt("pixel format".into()).into()),
        }
        Ok(())
    }
    pub fn open_package(&mut self, bytes: &[u8]) -> Result<(DocHandle, Vec<String>), EngineError> {
        self.open_entries(alto_store::read_entries(bytes)?)
    }
    pub(crate) fn install_package(
        &mut self,
        objects: BTreeMap<ContentHash, Vec<u8>>,
        opaque: std::collections::BTreeSet<ContentHash>,
        snapshot: &[u8],
        mut warnings: Vec<String>,
        deferred_verify: bool,
    ) -> Result<(DocHandle, Vec<String>), EngineError> {
        for (h, b) in objects {
            if b[8] == 1 || b[8] == 4 {
                let insert = if deferred_verify {
                    MemoryTileStore::insert_deferred
                } else {
                    MemoryTileStore::insert_verified
                };
                insert(
                    &mut self.tiles,
                    h,
                    if b[8] == 4 {
                        alto_core::PixelFormat::Rgba8
                    } else {
                        alto_core::PixelFormat::R8
                    },
                    b[9..].to_vec(),
                    opaque.contains(&h),
                );
            } else {
                self.install_object(h, &b)?;
            }
        }
        let handle = self.open_snapshot(snapshot)?;
        let doc = self.document(handle)?;
        let mut hashes = std::collections::BTreeSet::new();
        for n in doc.nodes.values() {
            node_hashes(n, &mut hashes);
        }
        if let Some(selection) = &doc.selection {
            hashes.extend(selection.tiles.values().copied());
        }
        for h in hashes {
            if !self.tiles.contains(&h) {
                warnings.push(format!("missing tile {h}"));
            }
        }
        Ok((handle, warnings))
    }
    pub fn journal_record(&self, intent: &str, ops: &[Op]) -> Result<Vec<u8>, EngineError> {
        let mut entries = BTreeMap::new();
        entries.insert(
            "transaction.json".into(),
            serde_json::to_vec(&serde_json::json!({"intent":intent,"ops":ops}))
                .map_err(|e| DocError::Serialization(e.to_string()))?,
        );
        let mut hashes = std::collections::BTreeSet::new();
        for op in ops {
            op_hashes(op, &mut hashes);
        }
        // A record without its objects cannot be replayed after a crash.
        for h in hashes {
            let b = self.encoded(h)?.ok_or(EngineError::MissingTile(h))?;
            entries.insert(format!("objects/{h}"), b);
        }
        Ok(alto_store::write_entries(&entries)?)
    }
    pub fn replay_record(&mut self, h: DocHandle, bytes: &[u8]) -> Result<(), EngineError> {
        let mut entries = alto_store::read_entries(bytes)?;
        let tx = entries
            .remove("transaction.json")
            .ok_or_else(|| alto_store::StoreError::Corrupt("journal transaction missing".into()))?;
        let v: serde_json::Value =
            serde_json::from_slice(&tx).map_err(|e| DocError::Serialization(e.to_string()))?;
        let ops: Vec<Op> = serde_json::from_value(v["ops"].clone())
            .map_err(|e| DocError::Serialization(e.to_string()))?;
        for (name, data) in entries {
            if let Some(s) = name.strip_prefix("objects/") {
                let hash = s
                    .parse()
                    .map_err(|_| alto_store::StoreError::Corrupt("journal object hash".into()))?;
                self.install_object(hash, &data)?;
            }
        }
        let revision = self.document(h)?.revision;
        self.transact(
            h,
            v["intent"].as_str().unwrap_or("Recovered edit"),
            ops,
            revision,
        )?;
        Ok(())
    }
}
pub(crate) fn node_hashes(n: &alto_doc::Node, h: &mut std::collections::BTreeSet<ContentHash>) {
    if let NodeKind::Raster { tiles } = &n.kind {
        h.extend(tiles.tiles.values().copied());
    }
    for effect in &n.props.effects {
        m5::effect_hashes(effect, h);
    }
    if let NodeKind::Adjustment { adjustment } = &n.kind {
        m5::effect_hashes(adjustment, h);
    }
    if let Some(mask) = &n.props.mask {
        h.extend(mask.tiles.values().copied());
    }
}
fn op_hashes(op: &Op, h: &mut std::collections::BTreeSet<ContentHash>) {
    match op {
        Op::SetTiles { changes, .. } => h.extend(changes.iter().filter_map(|c| c.after)),
        Op::InsertNode { node, .. } => node_hashes(node, h),
        Op::RestoreSubtree { nodes, .. } => {
            for n in nodes {
                node_hashes(n, h);
            }
        }
        Op::SetAsset {
            record: Some(a), ..
        } => {
            h.insert(a.hash);
        }
        Op::SetSelection { selection: Some(t) } | Op::SetMask { mask: Some(t), .. } => {
            h.extend(t.tiles.values().copied())
        }
        Op::SetProps { patch, .. } => {
            if let Some(effects) = &patch.effects {
                for e in effects {
                    m5::effect_hashes(e, h);
                }
            }
        }
        Op::SetAdjustment { adjustment, .. } => m5::effect_hashes(adjustment, h),
        Op::ReplaceNode { node, .. } => node_hashes(node, h),
        _ => {}
    }
}

fn tx_hashes(tx: &alto_doc::Transaction, h: &mut std::collections::BTreeSet<ContentHash>) {
    for applied in &tx.ops {
        op_hashes(&applied.op, h);
        op_hashes(&applied.inverse, h);
    }
}

/// Objects only the undo/redo history keeps alive. Older undos are dropped past this.
const MAX_HISTORY_ONLY_OBJECTS: usize = 2048;

/// How many of the oldest undo transactions to drop so that the history references at
/// most `limit` objects the document does not. One pass, newest first: each older
/// transaction can only add objects.
fn undos_over_budget(
    editor: &Editor,
    current: &std::collections::BTreeSet<ContentHash>,
    limit: usize,
) -> usize {
    let mut extra = std::collections::BTreeSet::new();
    let collect = |tx: &alto_doc::Transaction, extra: &mut std::collections::BTreeSet<_>| {
        let mut h = std::collections::BTreeSet::new();
        tx_hashes(tx, &mut h);
        extra.extend(h.into_iter().filter(|hash| !current.contains(hash)));
    };
    for tx in editor.redo_history() {
        collect(tx, &mut extra);
    }
    let undo = editor.undo_history();
    if extra.len() > limit {
        return undo.len();
    }
    for (kept, tx) in undo.iter().rev().enumerate() {
        collect(tx, &mut extra);
        if extra.len() > limit {
            return undo.len() - kept;
        }
    }
    0
}

impl Engine {
    pub fn pin_import(&mut self, h: ContentHash) {
        self.imported.insert(h);
    }
    fn maintain(&mut self) {
        for editor in self.documents.values_mut() {
            let mut current = std::collections::BTreeSet::new();
            for n in editor.doc.nodes.values() {
                node_hashes(n, &mut current);
            }
            current.extend(editor.doc.assets.values().map(|a| a.hash));
            let drop = undos_over_budget(editor, &current, MAX_HISTORY_ONLY_OBJECTS);
            editor.drop_oldest_undos(drop);
        }
        let mut current = self.live_hashes();
        let mut reachable = current.clone();
        for v in self.versions.values() {
            reachable.extend(v.retained.iter().copied());
        }
        current.extend(self.pending_blobs.iter().copied());
        reachable.extend(self.pending_blobs.iter().copied());
        if self.backed {
            self.tiles.separate(&current, &reachable);
        } else {
            self.tiles.retain(&reachable);
        }
        self.blobs.retain(|hash, _| reachable.contains(hash));
    }
    fn rooted_hashes(&self) -> std::collections::BTreeSet<ContentHash> {
        let mut referenced = self.live_hashes();
        for v in self.versions.values() {
            referenced.extend(v.retained.iter().copied());
        }
        referenced
    }
    /// A referenced import no longer needs a pin. Unreferenced pins stay.
    fn unpin_referenced(&mut self) {
        if self.pending_blobs.is_empty() {
            return;
        }
        let referenced = self.rooted_hashes();
        self.pending_blobs.retain(|hash| !referenced.contains(hash));
    }
    fn live_hashes(&self) -> std::collections::BTreeSet<ContentHash> {
        let mut current = self.imported.clone();
        for editor in self.documents.values() {
            for n in editor.doc.nodes.values() {
                node_hashes(n, &mut current);
            }
            if let Some(t) = &editor.doc.selection {
                current.extend(t.tiles.values().copied());
            }
            current.extend(editor.doc.assets.values().map(|a| a.hash));
            for tx in editor.undo_history().iter().chain(editor.redo_history()) {
                tx_hashes(tx, &mut current);
            }
        }
        current
    }
}
#[cfg(test)]
mod package_tests {
    use super::*;
    use alto_core::{PixelFormat, TileKey};
    use alto_doc::{Node, TileChange};
    #[test]
    fn unreferenced_blobs_are_dropped_and_live_assets_stay() {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(64, 64)).unwrap();
        let orphan = ContentHash::of(b"orphan");
        e.blobs.insert(orphan, b"orphan".to_vec());
        let hash = ContentHash::of(b"font-bytes");
        e.blobs.insert(hash, b"font-bytes".to_vec());
        let id = alto_core::AssetId::new();
        let record = alto_doc::AssetRecord {
            id,
            hash,
            kind: alto_doc::AssetKind::EncodedImage {
                mime: "image/png".into(),
            },
            byte_len: 10,
            dims: None,
        };
        e.transact(
            d,
            "asset",
            vec![Op::SetAsset {
                id,
                record: Some(record),
            }],
            0,
        )
        .unwrap();
        assert!(!e.blobs.contains_key(&orphan));
        assert_eq!(
            e.blobs.get(&hash).map(Vec::as_slice),
            Some(b"font-bytes".as_slice())
        );
    }
    #[test]
    fn imported_font_survives_maintain_until_set_asset() {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(64, 64)).unwrap();
        let font = e
            .import_font(
                include_bytes!("../../../tests/fonts/AltoTestLatin.ttf").to_vec(),
                0,
            )
            .unwrap();
        let hash = font.record.hash;
        e.maintain();
        assert!(e.blobs.contains_key(&hash));
        e.transact(
            d,
            "font",
            vec![Op::SetAsset {
                id: font.record.id,
                record: Some(font.record),
            }],
            0,
        )
        .unwrap();
        assert!(e.blobs.contains_key(&hash));
        assert!(e.pending_blobs.is_empty());
    }
    #[test]
    fn imported_font_survives_an_unrelated_transaction_until_set_asset() {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(64, 64)).unwrap();
        let font = e
            .import_font(
                include_bytes!("../../../tests/fonts/AltoTestLatin.ttf").to_vec(),
                0,
            )
            .unwrap();
        let hash = font.record.hash;
        let id = font.record.id;
        e.maintain();
        assert!(e.blobs.contains_key(&hash));
        let root = e.document(d).unwrap().root;
        let revision = e.document(d).unwrap().revision;
        e.transact(
            d,
            "other",
            vec![Op::InsertNode {
                node: Node::raster("x"),
                parent: root,
                index: 0,
            }],
            revision,
        )
        .unwrap();
        assert!(e.blobs.contains_key(&hash));
        assert!(e.pending_blobs.contains(&hash));
        let revision = e.document(d).unwrap().revision;
        e.transact(
            d,
            "font",
            vec![Op::SetAsset {
                id,
                record: Some(font.record),
            }],
            revision,
        )
        .unwrap();
        assert!(e.blobs.contains_key(&hash));
        assert!(!e.pending_blobs.contains(&hash));
    }
    #[test]
    fn release_import_and_the_last_close_abandon_an_unreferenced_font() {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(64, 64)).unwrap();
        let font = e
            .import_font(
                include_bytes!("../../../tests/fonts/AltoTestLatin.ttf").to_vec(),
                0,
            )
            .unwrap();
        let hash = font.record.hash;
        let root = e.document(d).unwrap().root;
        e.transact(
            d,
            "other",
            vec![Op::InsertNode {
                node: Node::raster("x"),
                parent: root,
                index: 0,
            }],
            0,
        )
        .unwrap();
        assert!(e.release_import(hash));
        assert!(!e.blobs.contains_key(&hash));
        let err = e
            .transact(
                d,
                "late",
                vec![Op::SetAsset {
                    id: font.record.id,
                    record: Some(font.record),
                }],
                e.document(d).unwrap().revision,
            )
            .unwrap_err();
        assert!(err.to_string().contains("content tile not found"), "{err}");
        let again = e
            .import_font(
                include_bytes!("../../../tests/fonts/AltoTestLatin.ttf").to_vec(),
                0,
            )
            .unwrap();
        e.close_document(d).unwrap();
        assert!(!e.blobs.contains_key(&again.record.hash));
        assert!(e.pending_blobs.is_empty());
    }
    #[test]
    fn encoded_image_blob_survives_maintain_until_set_asset() {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(8, 8)).unwrap();
        let hash = e.put_import_blob(b"encoded-image".to_vec());
        e.maintain();
        assert_eq!(
            e.blobs.get(&hash).map(Vec::as_slice),
            Some(b"encoded-image".as_slice())
        );
        let root = e.document(d).unwrap().root;
        e.transact(
            d,
            "other",
            vec![Op::InsertNode {
                node: Node::raster("x"),
                parent: root,
                index: 0,
            }],
            0,
        )
        .unwrap();
        assert!(e.pending_blobs.contains(&hash));
        let id = alto_core::AssetId::new();
        e.transact(
            d,
            "image",
            vec![Op::SetAsset {
                id,
                record: Some(alto_doc::AssetRecord {
                    id,
                    hash,
                    kind: alto_doc::AssetKind::EncodedImage {
                        mime: "image/png".into(),
                    },
                    byte_len: 14,
                    dims: Some((8, 8)),
                }),
            }],
            e.document(d).unwrap().revision,
        )
        .unwrap();
        assert!(e.blobs.contains_key(&hash));
        assert!(e.pending_blobs.is_empty());
    }
    #[test]
    fn fresh_engine_package_and_journal_restore_exactly() {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(256, 256)).unwrap();
        let n = Node::raster("Base");
        let id = n.id;
        let root = e.document(d).unwrap().root;
        e.transact(
            d,
            "layer",
            vec![Op::InsertNode {
                node: n,
                parent: root,
                index: 0,
            }],
            0,
        )
        .unwrap();
        let base = e.save_package(d).unwrap();
        let mut pixels = vec![0; 262144];
        pixels[3] = 255;
        let hash = e
            .put_tile(TileData::new(PixelFormat::Rgba8, pixels).unwrap())
            .unwrap();
        let ops = vec![Op::SetTiles {
            layer: id,
            changes: vec![TileChange {
                key: TileKey { tx: 0, ty: 0 },
                before: None,
                after: Some(hash),
            }],
        }];
        e.transact(d, "paint", ops.clone(), 1).unwrap();
        let record = e.journal_record("paint", &ops).unwrap();
        let mut recovered = Engine::new();
        let (r, warning) = recovered.open_package(&base).unwrap();
        assert!(warning.is_empty());
        recovered.replay_record(r, &record).unwrap();
        assert_eq!(e.snapshot(d).unwrap(), recovered.snapshot(r).unwrap());
        assert_eq!(
            e.tile(&hash).unwrap().unwrap().pixels,
            recovered.tile(&hash).unwrap().unwrap().pixels
        );
        let full = e.save_package(d).unwrap();
        let mut fresh = Engine::new();
        let (f, warnings) = fresh.open_package(&full).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(fresh.snapshot(f).unwrap(), e.snapshot(d).unwrap());
        assert_eq!(fresh.tile(&hash).unwrap().unwrap().hash(), hash);
    }

    #[test]
    fn undo_budget_matches_dropping_one_transaction_at_a_time() {
        let mut editor = Editor::new(Document::new(DocumentMeta::srgb_u8(256, 256)).unwrap());
        let root = editor.doc.root;
        let layer = Node::raster("A");
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
        // Each paint references a new tile; some reuse an earlier one.
        let mut placed = BTreeMap::new();
        for i in 0..40u32 {
            let seed = if i % 5 == 0 { 0 } else { i };
            let key = TileKey {
                tx: (i % 3) as i32,
                ty: 0,
            };
            let after = ContentHash::of(&seed.to_le_bytes());
            let before = placed.insert(key, after);
            let rev = editor.doc.revision;
            editor
                .transact(
                    "paint",
                    vec![Op::SetTiles {
                        layer: id,
                        changes: vec![TileChange {
                            key,
                            before,
                            after: Some(after),
                        }],
                    }],
                    rev,
                )
                .unwrap();
        }
        for _ in 0..4 {
            editor.undo().unwrap();
        }
        assert_eq!((editor.undo_len(), editor.redo_len()), (37, 4));
        let mut current = std::collections::BTreeSet::new();
        for n in editor.doc.nodes.values() {
            node_hashes(n, &mut current);
        }
        for limit in [0, 1, 5, 12, 30, 1000] {
            let mut slow = editor.clone();
            loop {
                let mut history = std::collections::BTreeSet::new();
                for tx in slow.undo_history().iter().chain(slow.redo_history()) {
                    tx_hashes(tx, &mut history);
                }
                if history.difference(&current).count() <= limit || !slow.drop_oldest_undo() {
                    break;
                }
            }
            let mut fast = editor.clone();
            fast.drop_oldest_undos(undos_over_budget(&editor, &current, limit));
            assert_eq!(fast.undo_len(), slow.undo_len(), "limit {limit}");
        }
    }

    #[test]
    fn journal_record_refuses_an_operation_whose_tile_is_missing() {
        let e = Engine::new();
        let absent = ContentHash::of(b"never stored");
        let ops = vec![Op::SetTiles {
            layer: alto_core::NodeId::new(),
            changes: vec![TileChange {
                key: TileKey { tx: 0, ty: 0 },
                before: None,
                after: Some(absent),
            }],
        }];
        assert!(matches!(
            e.journal_record("paint", &ops),
            Err(EngineError::MissingTile(h)) if h == absent
        ));
    }
}

#[cfg(test)]
mod m6_tests;

#[cfg(test)]
mod m78_tests;
