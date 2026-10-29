mod edit;
pub mod m5;
pub mod text;
pub mod unstable_render;
pub mod versions;
mod warning;
use alto_core::ContentHash;
use alto_doc::{BlendMode, ChangeSet, DocError, Document, DocumentMeta, Editor, NodeKind, Op};
use alto_raster::{DecodeCache, ObjectStore, TileData, TileStore};
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
pub use versions::{CommitView, RecoveryCheckpoint, VersionRef, VersionStatus};
pub use warning::Warning;

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
    tiles: ObjectStore,
    /// Decoded tiles for CPU edits, which read pixel by pixel. The renderer keeps
    /// none: its GPU atlas is its cache.
    decoded: std::cell::RefCell<DecodeCache>,
    blobs: BTreeMap<ContentHash, Vec<u8>>,
    imported: std::collections::BTreeSet<ContentHash>,
    /// Blobs stored by import and not yet referenced by a committed transaction.
    pending_blobs: std::collections::BTreeSet<ContentHash>,
    staged_tiles: BTreeMap<u64, std::collections::BTreeSet<ContentHash>>,
    next_stage: u64,
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
        self.decoded.get_mut().clear();
    }
    pub fn new() -> Self {
        Self {
            documents: BTreeMap::new(),
            next_handle: 1,
            tiles: ObjectStore::default(),
            decoded: std::cell::RefCell::new(DecodeCache::new(128 * 1024 * 1024)),
            blobs: BTreeMap::new(),
            imported: std::collections::BTreeSet::new(),
            pending_blobs: std::collections::BTreeSet::new(),
            staged_tiles: BTreeMap::new(),
            next_stage: 0,
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
            let hash = self.tiles.insert_tile(&tile);
            // New tiles are usually read again at once (fills, journal records).
            self.decoded
                .get_mut()
                .insert(hash, std::sync::Arc::new(tile));
            Some(hash)
        }
    }

    /// Encodes a pending stroke tile without growing the raw decoded cache.
    /// The caller keeps this hash rooted until its one atomic transaction finishes.
    pub fn begin_tile_stage(&mut self) -> u64 {
        self.next_stage += 1;
        self.staged_tiles
            .insert(self.next_stage, Default::default());
        self.next_stage
    }
    pub fn stage_tile(
        &mut self,
        stage: u64,
        tile: TileData,
    ) -> Result<Option<ContentHash>, EngineError> {
        if !self.staged_tiles.contains_key(&stage) {
            return Err(DocError::Invalid("tile stage expired".into()).into());
        }
        if tile.is_empty() {
            return Ok(None);
        }
        let hash = self.tiles.insert_tile(&tile);
        self.staged_tiles
            .get_mut(&stage)
            .expect("active tile stage")
            .insert(hash);
        Ok(Some(hash))
    }
    pub fn release_staged_tiles(&mut self, stage: u64) {
        self.staged_tiles.remove(&stage);
        self.maintain();
    }
    pub fn tile(
        &self,
        hash: &ContentHash,
    ) -> Result<Option<std::sync::Arc<TileData>>, EngineError> {
        Ok(self.decoded.borrow_mut().get(&self.tiles, hash)?)
    }
    /// For tiles the document references: absence is an error, not transparency.
    pub fn require_tile(
        &self,
        hash: &ContentHash,
    ) -> Result<std::sync::Arc<TileData>, EngineError> {
        self.tile(hash)?.ok_or(EngineError::MissingTile(*hash))
    }

    pub fn transact(
        &mut self,
        handle: DocHandle,
        intent: impl Into<alto_doc::Intent>,
        mut ops: Vec<Op>,
        base_revision: u64,
    ) -> Result<ChangeSet, EngineError> {
        // Render version 3 text is drawn from its bake only; local edits bake here.
        ops = self.bake_text_ops(handle, ops)?;
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
    /// `ops` with every render version 3 text op baked and the bake blobs stored
    /// (`bake_ops`). `transact` calls this itself; a caller that journals the ops
    /// calls it first, so the journal carries the bakes the transaction applies.
    pub fn bake_text_ops(
        &mut self,
        handle: DocHandle,
        mut ops: Vec<Op>,
    ) -> Result<Vec<Op>, EngineError> {
        let doc = &self
            .documents
            .get(&handle)
            .ok_or(EngineError::NotFound(handle))?
            .doc;
        for bytes in self.bake_ops(doc, &mut ops) {
            self.put_blob(bytes);
        }
        Ok(ops)
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
                NodeKind::Text { content, .. } => {
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
            render_version: doc.meta.render_version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alto_core::{PixelFormat, TILE, TileKey};
    use alto_doc::{Node, NodeKind, TileChange};

    #[test]
    fn staged_tiles_are_gc_roots_without_a_decoded_copy() {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(256, 256)).unwrap();
        let before = e.snapshot(d).unwrap();
        let stage = e.begin_tile_stage();
        let mut pixels = vec![0; 262144];
        pixels[..4].copy_from_slice(&[255, 0, 0, 255]);
        let h = e
            .stage_tile(stage, TileData::new(PixelFormat::Rgba8, pixels).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(e.decoded.borrow().bytes(), 0);
        e.maintain();
        assert!(e.tile(&h).unwrap().is_some());
        assert_eq!(e.snapshot(d).unwrap(), before);
        e.release_staged_tiles(stage);
        assert!(e.tile(&h).unwrap().is_none());
        assert!(
            e.stage_tile(
                stage,
                TileData::new(PixelFormat::Rgba8, vec![0; 262144]).unwrap()
            )
            .is_err()
        );
    }

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
    fn object(&self, h: &ContentHash) -> Option<std::sync::Arc<alto_raster::StoredTile>> {
        self.tiles.object(h)
    }
}
impl Engine {
    pub fn state_digest(&self, h: DocHandle) -> Result<ContentHash, EngineError> {
        Ok(self
            .documents
            .get(&h)
            .ok_or(EngineError::NotFound(h))?
            .state_digest())
    }
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
        if let Some(object) = self.tiles.object(&h) {
            Ok(Some(object.encoded(&h)?))
        } else {
            Ok(self.blobs.get(&h).map(|b| alto_store::encode_object(0, b)))
        }
    }
    /// Format 4 is what a render version 3 document serializes as. Writing that
    /// package is refused until integer compositing lands, unless this thread
    /// opted in. A snapshot used by view, hash, or a recovery checkpoint is not
    /// this path and still succeeds for a document that is already version 3.
    fn require_stable_package(&self, h: DocHandle) -> Result<(), EngineError> {
        let doc = self.document(h)?;
        if doc.meta.render_version >= alto_core::RENDER_VERSION_EXACT
            && !unstable_render::unstable_render_v3_enabled()
        {
            return Err(DocError::Invalid(unstable_render::UNSTABLE_RENDER_V3_OFF.into()).into());
        }
        Ok(())
    }
    pub fn save_package(&mut self, h: DocHandle) -> Result<Vec<u8>, EngineError> {
        self.require_stable_package(h)?;
        let bytes = alto_store::encode_parts(&self.package_parts(h)?)?;
        self.backed = true;
        self.maintain();
        Ok(bytes)
    }
    pub fn tile_resident(&self, hash: &ContentHash) -> bool {
        self.tiles.contains(hash)
    }
    pub fn resident_bytes(&self) -> usize {
        self.tiles.allocated_bytes() + self.decoded.borrow().bytes()
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
                self.tiles.insert_tile(&tile);
            }
            _ => return Err(alto_store::StoreError::Corrupt("pixel format".into()).into()),
        }
        Ok(())
    }
    pub fn open_package(&mut self, bytes: &[u8]) -> Result<(DocHandle, Vec<Warning>), EngineError> {
        self.open_entries(alto_store::read_entries(bytes)?)
    }
    pub(crate) fn install_package(
        &mut self,
        objects: BTreeMap<ContentHash, Vec<u8>>,
        opaque: std::collections::BTreeSet<ContentHash>,
        snapshot: &[u8],
        mut warnings: Vec<Warning>,
        deferred_verify: bool,
    ) -> Result<(DocHandle, Vec<Warning>), EngineError> {
        for (h, b) in objects {
            if b[8] == 1 || b[8] == 4 {
                let insert = if deferred_verify {
                    ObjectStore::insert_deferred
                } else {
                    ObjectStore::insert_verified
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
            // Text bakes are blobs, not tiles.
            if !self.tiles.contains(&h) && !self.blobs.contains_key(&h) {
                warnings.push(Warning::new("MISSING_TILE").with("hash", h));
            }
        }
        Ok((handle, warnings))
    }
    /// A journal record for the step just applied to `h`. `ops` are what the step
    /// applied, so a checkpoint without undo history can still replay it forward.
    pub fn journal_record(
        &self,
        h: DocHandle,
        kind: JournalKind,
        intent: impl Into<alto_doc::Intent>,
        ops: &[Op],
    ) -> Result<Vec<u8>, EngineError> {
        let intent = intent.into();
        let mut entries = BTreeMap::new();
        let after = self.state_digest(h)?;
        entries.insert(
            "transaction.json".into(),
            serde_json::to_vec(&serde_json::json!({
                "kind": kind.name(), "intent": intent.id, "label": intent.label,
                "ops": ops, "after": after,
            }))
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
    /// Replays one journal record. Undo and redo use the restored history when there
    /// is one; older checkpoints have none, and their steps replay as forward edits.
    /// A step that does not reach the recorded state is reverted and reported.
    pub fn replay_record(&mut self, h: DocHandle, bytes: &[u8]) -> Result<Replayed, EngineError> {
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
        let (undo, redo) = self.history_state(h)?;
        let applied = match v["kind"].as_str() {
            Some("undo") if undo > 0 => {
                self.undo(h)?;
                JournalKind::Undo
            }
            Some("redo") if redo > 0 => {
                self.redo(h)?;
                JournalKind::Redo
            }
            _ => {
                let revision = self.document(h)?.revision;
                let intent = alto_doc::Intent {
                    id: v["intent"].as_str().unwrap_or("recovered.edit").into(),
                    label: v["label"].as_str().map(Into::into),
                };
                self.transact(h, intent, ops, revision)?;
                JournalKind::Transact
            }
        };
        let Some(after) = v["after"].as_str() else {
            return Ok(Replayed::Applied);
        };
        if after == self.state_digest(h)?.to_string() {
            return Ok(Replayed::Applied);
        }
        match applied {
            JournalKind::Undo => {
                self.redo(h)?;
            }
            // Undoing the replayed redo puts the step back on the redo stack.
            JournalKind::Redo => {
                self.undo(h)?;
            }
            // A transaction cleared the redo stack; undoing it leaves only the
            // diverged step there, which is not offered for redo.
            JournalKind::Transact => {
                self.undo(h)?;
                self.documents
                    .get_mut(&h)
                    .ok_or(EngineError::NotFound(h))?
                    .drop_newest_redo();
            }
        }
        Ok(Replayed::Diverged)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalKind {
    Transact,
    Undo,
    Redo,
}
impl JournalKind {
    fn name(self) -> &'static str {
        match self {
            Self::Transact => "transact",
            Self::Undo => "undo",
            Self::Redo => "redo",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Replayed {
    Applied,
    /// The step did not reproduce the recorded state; it was reverted.
    Diverged,
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
    if let NodeKind::Text {
        bake: Some(bake), ..
    } = &n.kind
    {
        h.insert(*bake);
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
        Op::SetText {
            bake: Some(bake), ..
        } => {
            h.insert(*bake);
        }
        Op::ReplaceNode { node, .. } => node_hashes(node, h),
        _ => {}
    }
}

pub(crate) fn tx_hashes(
    tx: &alto_doc::Transaction,
    h: &mut std::collections::BTreeSet<ContentHash>,
) {
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
        for stage in self.staged_tiles.values() {
            reachable.extend(stage.iter().copied());
            current.extend(stage.iter().copied());
        }
        if self.backed {
            self.tiles.separate(&current, &reachable);
            self.decoded.get_mut().retain(|h| current.contains(h));
        } else {
            self.tiles.retain(&reachable);
            self.decoded.get_mut().retain(|h| reachable.contains(h));
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
        let record = e
            .journal_record(d, JournalKind::Transact, "paint", &ops)
            .unwrap();
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
    fn incremental_recovery_checkpoint_writes_each_object_once_and_reopens() {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(512, 256)).unwrap();
        let layer = Node::raster("A");
        let id = layer.id;
        let root = e.document(d).unwrap().root;
        e.transact(
            d,
            "add",
            vec![Op::InsertNode {
                node: layer,
                parent: root,
                index: 0,
            }],
            0,
        )
        .unwrap();
        let paint = |e: &mut Engine, tx: i32, v: u8| {
            let mut px = vec![v; 262144];
            px[3] = 255;
            let after = e.put_tile(TileData::new(PixelFormat::Rgba8, px).unwrap());
            let rev = e.document(d).unwrap().revision;
            let key = TileKey { tx, ty: 0 };
            let NodeKind::Raster { tiles } = &e.document(d).unwrap().node(id).unwrap().kind else {
                panic!()
            };
            let before = tiles.tiles.get(&key).copied();
            e.transact(
                d,
                "paint",
                vec![Op::SetTiles {
                    layer: id,
                    changes: vec![TileChange { key, before, after }],
                }],
                rev,
            )
            .unwrap();
        };
        paint(&mut e, 0, 10);
        let mut disk = BTreeMap::new();
        let mut stored = std::collections::BTreeSet::new();
        let mut checkpoint = |e: &Engine, disk: &mut BTreeMap<String, Vec<u8>>| {
            let c = e.recovery_checkpoint(d, &stored).unwrap();
            stored.extend(c.written.iter().copied());
            let mut rest = BTreeMap::new();
            for (name, bytes) in c.entries {
                if name.starts_with("objects/") {
                    assert!(
                        disk.insert(name, bytes).is_none(),
                        "an object was written twice"
                    );
                } else {
                    rest.insert(name, bytes);
                }
            }
            (c.written.len(), rest)
        };
        let (first, _) = checkpoint(&e, &mut disk);
        assert_eq!(first, 1);
        paint(&mut e, 1, 20);
        let (second, rest) = checkpoint(&e, &mut disk);
        assert_eq!(second, 1, "only the new tile");
        // Recovery reads the referenced objects from the store.
        let meta: serde_json::Value = serde_json::from_slice(&rest["checkpoint.json"]).unwrap();
        let mut entries = rest.clone();
        entries.remove("checkpoint.json");
        for p in meta["objects"].as_array().unwrap() {
            let p = p.as_str().unwrap();
            entries.insert(p.into(), disk[p].clone());
        }
        let opaque: std::collections::BTreeSet<ContentHash> =
            serde_json::from_value(meta["opaque"].clone()).unwrap();
        let mut fresh = Engine::new();
        let (r, warnings) = fresh.open_recovery_entries(entries, &opaque).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(fresh.snapshot(r).unwrap(), e.snapshot(d).unwrap());
    }

    /// Builds the same recovery the Worker does: checkpoint entries plus stored objects.
    fn recover(
        checkpoint: RecoveryCheckpoint,
        records: &[Vec<u8>],
    ) -> (Engine, DocHandle, Vec<Replayed>) {
        let mut entries = checkpoint.entries;
        let meta: serde_json::Value =
            serde_json::from_slice(&entries.remove("checkpoint.json").unwrap()).unwrap();
        let opaque: std::collections::BTreeSet<ContentHash> =
            serde_json::from_value(meta["opaque"].clone()).unwrap();
        let mut fresh = Engine::new();
        let (r, warnings) = fresh.open_recovery_entries(entries, &opaque).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let replayed = records
            .iter()
            .map(|b| fresh.replay_record(r, b).unwrap())
            .collect();
        (fresh, r, replayed)
    }
    fn history_walk(e: &mut Engine, d: DocHandle) -> Vec<Vec<u8>> {
        let mut out = vec![e.snapshot(d).unwrap()];
        while e.undo(d).unwrap().is_some() {
            out.push(e.snapshot(d).unwrap());
        }
        while e.redo(d).unwrap().is_some() {
            out.push(e.snapshot(d).unwrap());
        }
        out
    }

    #[test]
    fn recovery_keeps_undo_history_across_the_checkpoint() {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(256, 256)).unwrap();
        let layer = Node::raster("A");
        let id = layer.id;
        let root = e.document(d).unwrap().root;
        e.transact(
            d,
            "add",
            vec![Op::InsertNode {
                node: layer,
                parent: root,
                index: 0,
            }],
            0,
        )
        .unwrap();
        let mut tiles = vec![];
        let mut paint = |e: &mut Engine, v: u8| -> Vec<Op> {
            let mut px = vec![v; 262144];
            px[3] = 255;
            let after = e.put_tile(TileData::new(PixelFormat::Rgba8, px).unwrap());
            let before = tiles.last().copied().flatten();
            tiles.push(after);
            let key = TileKey { tx: 0, ty: 0 };
            let ops = vec![Op::SetTiles {
                layer: id,
                changes: vec![TileChange { key, before, after }],
            }];
            let rev = e.document(d).unwrap().revision;
            e.transact(d, "paint", ops.clone(), rev).unwrap();
            ops
        };
        paint(&mut e, 10);
        paint(&mut e, 20);
        // The first tile is only in the undo history now.
        let checkpoint = e.recovery_checkpoint(d, &Default::default()).unwrap();
        let mut records = vec![];
        let ops = paint(&mut e, 30);
        records.push(
            e.journal_record(d, JournalKind::Transact, "paint", &ops)
                .unwrap(),
        );
        let ops = e.undo_operations(d).unwrap();
        e.undo(d).unwrap();
        records.push(
            e.journal_record(d, JournalKind::Undo, "edit.undo", &ops)
                .unwrap(),
        );
        let (mut fresh, r, replayed) = recover(checkpoint, &records);
        assert!(replayed.iter().all(|x| *x == Replayed::Applied));
        assert_eq!(fresh.history_state(r).unwrap(), e.history_state(d).unwrap());
        assert_eq!(history_walk(&mut fresh, r), history_walk(&mut e, d));
    }

    #[test]
    fn a_replayed_step_that_misses_its_recorded_state_is_reverted() {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(64, 64)).unwrap();
        let root = e.document(d).unwrap().root;
        let checkpoint = e.recovery_checkpoint(d, &Default::default()).unwrap();
        let layer = Node::raster("A");
        let ops = vec![Op::InsertNode {
            node: layer,
            parent: root,
            index: 0,
        }];
        e.transact(d, "add", ops.clone(), 0).unwrap();
        let good = e
            .journal_record(d, JournalKind::Transact, "add", &ops)
            .unwrap();
        // Same step, but a recorded state it cannot reach.
        let mut entries = alto_store::read_entries(&good).unwrap();
        let mut tx: serde_json::Value =
            serde_json::from_slice(&entries["transaction.json"]).unwrap();
        tx["after"] = serde_json::json!(ContentHash::of(b"elsewhere"));
        entries.insert("transaction.json".into(), serde_json::to_vec(&tx).unwrap());
        let bad = alto_store::write_entries(&entries).unwrap();
        let (fresh, r, replayed) = recover(checkpoint, &[bad]);
        assert_eq!(replayed, vec![Replayed::Diverged]);
        assert_eq!(
            fresh.document(r).unwrap().nodes.len(),
            1,
            "the step was not reverted"
        );
        assert_eq!(fresh.history_state(r).unwrap(), (0, 0));
    }

    #[test]
    fn journal_record_refuses_an_operation_whose_tile_is_missing() {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(64, 64)).unwrap();
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
            e.journal_record(d, JournalKind::Transact, "paint", &ops),
            Err(EngineError::MissingTile(h)) if h == absent
        ));
    }
}

#[cfg(test)]
mod render_v3_fixture {
    use super::*;
    use crate::unstable_render::{
        UnstableRenderV3Guard, allow_unstable_render_v3, set_unstable_render_v3,
        unstable_render_v3_enabled,
    };
    use alto_core::{RENDER_VERSION, RENDER_VERSION_EXACT, RENDER_VERSION_F32};

    #[test]
    fn render_v3_empty_fixture_opens_at_format_four() {
        let bytes = include_bytes!("../../../tests/fixtures/render-v3-empty.alto");
        let mut engine = Engine::new();
        let (opened, warnings) = engine.open_package(bytes).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let loaded = engine.document(opened).unwrap();
        assert_eq!(loaded.meta.render_version, RENDER_VERSION_EXACT);
        assert_eq!(loaded.meta.format_version, 4);
        assert_eq!((loaded.meta.width, loaded.meta.height), (256, 256));
        assert_eq!(loaded.nodes.len(), 1);
        let err = engine.save_package(opened).unwrap_err();
        assert!(err.to_string().contains("unstable and is off"), "{err}");
        if cfg!(debug_assertions) {
            let _guard = UnstableRenderV3Guard::enable();
            assert!(unstable_render_v3_enabled());
            assert_eq!(engine.save_package(opened).unwrap(), bytes);
        }
    }

    #[test]
    fn default_path_cannot_upgrade_to_render_version_three_or_write_format_four() {
        assert!(
            !allow_unstable_render_v3(false, true, true),
            "a release build ignores the opt-in"
        );
        assert!(
            !allow_unstable_render_v3(true, false, true),
            "the feature is off unless this crate's tests or the feature compiled it in"
        );
        assert!(
            !allow_unstable_render_v3(true, true, false),
            "the request defaults off"
        );
        assert!(allow_unstable_render_v3(true, true, true));
        assert!(!set_unstable_render_v3(false));
        assert!(!unstable_render_v3_enabled());
        if !cfg!(debug_assertions) {
            assert!(!set_unstable_render_v3(true));
            assert!(!unstable_render_v3_enabled());
        }

        let mut engine = Engine::new();
        let mut meta = DocumentMeta::srgb_u8(32, 32);
        meta.render_version = RENDER_VERSION_F32;
        meta.format_version = 1;
        let handle = engine.create_document(meta).unwrap();
        engine.upgrade_render_version(handle).unwrap();
        let upgraded = engine.document(handle).unwrap();
        assert_eq!(upgraded.meta.render_version, RENDER_VERSION);
        assert_eq!(upgraded.meta.format_version, 3);
        let saved = engine.save_package(handle).unwrap();
        let (reopened, warnings) = engine.open_package(&saved).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let resaved = &engine.document(reopened).unwrap().meta;
        assert_eq!(resaved.render_version, RENDER_VERSION);
        assert_eq!(resaved.format_version, 3);

        let err = engine.upgrade_render_version(handle).unwrap_err();
        assert!(err.to_string().contains("already current"), "{err}");
        let err = engine.upgrade_render_version_unstable(handle).unwrap_err();
        assert!(err.to_string().contains("unstable and is off"), "{err}");
        let stayed = engine.document(handle).unwrap();
        assert_eq!(stayed.meta.render_version, RENDER_VERSION);
        assert_eq!(stayed.meta.format_version, 3);

        let bytes = include_bytes!("../../../tests/fixtures/render-v3-empty.alto");
        let (opened, warnings) = engine.open_package(bytes).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let loaded = engine.document(opened).unwrap();
        assert_eq!(loaded.meta.render_version, RENDER_VERSION_EXACT);
        assert_eq!(loaded.meta.format_version, 4);
        assert_eq!(loaded.nodes.len(), 1);
        let err = engine.save_package(opened).unwrap_err();
        assert!(err.to_string().contains("unstable and is off"), "{err}");
        let err = engine.save_package_trimmed(opened, 0).unwrap_err();
        assert!(err.to_string().contains("unstable and is off"), "{err}");
        let err = engine.folder_entries(opened).unwrap_err();
        assert!(err.to_string().contains("unstable and is off"), "{err}");
        assert_eq!(
            engine.document(opened).unwrap().meta.render_version,
            RENDER_VERSION_EXACT
        );
        let text = String::from_utf8(engine.snapshot(opened).unwrap()).unwrap();
        assert!(text.contains("\"render_version\": 3"));
        assert!(text.contains("\"format_version\": 4"));
    }

    /// The empty v3 fixtures are the engine's own save of an empty document:
    /// render version 3 (format 4) at each size, through the debug-only opt-in.
    /// `render-v3-empty-8389120x256.alto` is 8388864 + 256 px wide, past 2^23
    /// px, for the browser's plain-path 2^23 checks (Linus 08:10); it opens
    /// through `open_package` (`engine.open`), which has no canvas limit
    /// (`createDocument`'s 65536 px applies to new documents only).
    /// `ALTO_WRITE_FIXTURES=1` writes missing or changed fixtures.
    #[test]
    fn empty_v3_fixtures_are_the_engine_save() {
        if !cfg!(debug_assertions) {
            return;
        }
        let _guard = UnstableRenderV3Guard::enable();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        for (w, h) in [
            (256, 256),
            (300, 100),
            (512, 256),
            (512, 512),
            (4096, 256),
            (8_389_120, 256),
        ] {
            let mut engine = Engine::new();
            let created = engine.create_document(DocumentMeta::srgb_u8(w, h)).unwrap();
            engine.upgrade_render_version_unstable(created).unwrap();
            // The fixtures name the root node with a fixed id, so the bytes do not
            // depend on the clock; the package (snapshot hash, manifest) is then
            // the engine's save of that snapshot.
            let root = engine.document(created).unwrap().root.0.to_string();
            let snapshot = String::from_utf8(engine.snapshot(created).unwrap())
                .unwrap()
                .replace(&root, "00000000-0000-7000-8000-000000000001");
            let handle = engine.open_snapshot(snapshot.as_bytes()).unwrap();
            let saved = engine.save_package(handle).unwrap();
            let name = if (w, h) == (256, 256) {
                "render-v3-empty.alto".to_string()
            } else {
                format!("render-v3-empty-{w}x{h}.alto")
            };
            let path = dir.join(&name);
            if std::env::var_os("ALTO_WRITE_FIXTURES").is_some()
                && std::fs::read(&path).ok().as_deref() != Some(&saved[..])
            {
                std::fs::write(&path, &saved).unwrap();
            }
            if let Some(dump) = std::env::var_os("ALTO_DUMP_FIXTURES") {
                std::fs::write(std::path::Path::new(&dump).join(&name), &saved).unwrap();
            }
            let committed = std::fs::read(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(committed == saved, "{name} is not the engine's save");
            let (opened, warnings) = engine.open_package(&committed).unwrap();
            assert!(warnings.is_empty(), "{name}: {warnings:?}");
            let meta = &engine.document(opened).unwrap().meta;
            assert_eq!((meta.width, meta.height), (w, h), "{name}");
            assert_eq!(meta.render_version, RENDER_VERSION_EXACT, "{name}");
        }
    }

    #[test]
    fn unstable_opt_in_can_upgrade_to_render_version_three_and_write_format_four() {
        if !cfg!(debug_assertions) {
            assert!(!set_unstable_render_v3(true));
            assert!(!unstable_render_v3_enabled());
            return;
        }
        let _guard = UnstableRenderV3Guard::enable();
        assert!(unstable_render_v3_enabled());
        let mut engine = Engine::new();
        let handle = engine
            .create_document(DocumentMeta::srgb_u8(32, 32))
            .unwrap();
        assert_eq!(
            engine.document(handle).unwrap().meta.render_version,
            RENDER_VERSION
        );
        engine.upgrade_render_version_unstable(handle).unwrap();
        let upgraded = engine.document(handle).unwrap();
        assert_eq!(upgraded.meta.render_version, RENDER_VERSION_EXACT);
        assert_eq!(upgraded.meta.format_version, 4);
        let saved = engine.save_package(handle).unwrap();
        let (opened, warnings) = engine.open_package(&saved).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let loaded = &engine.document(opened).unwrap().meta;
        assert_eq!(loaded.render_version, RENDER_VERSION_EXACT);
        assert_eq!(loaded.format_version, 4);
        let err = engine.upgrade_render_version_unstable(handle).unwrap_err();
        assert!(err.to_string().contains("already current"), "{err}");
        assert_eq!(engine.history_state(handle).unwrap(), (1, 0));
    }
}

#[cfg(test)]
mod m6_tests;

#[cfg(test)]
mod m78_tests;

#[cfg(test)]
mod text_bake_tests;
