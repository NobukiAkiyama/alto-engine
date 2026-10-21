mod edit;
pub mod m5;
pub mod text;
pub mod versions;
use alto_core::ContentHash;
use alto_doc::{BlendMode, ChangeSet, DocError, Document, DocumentMeta, Editor, NodeKind, Op};
use alto_gpu::{RContent, RNode, RenderBlend, RenderScene, RenderTile};
use alto_raster::{MemoryTileStore, TileData, TileStore};
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
pub enum EngineError {
    #[error("document not found: {0:?}")]
    NotFound(DocHandle),
    #[error("content tile not found: {0}")]
    MissingTile(ContentHash),
    #[error("canvas dimensions exceed renderer coordinate range")]
    InvalidDimensions,
    #[error(transparent)]
    Document(#[from] DocError),
    #[error(transparent)]
    Storage(#[from] alto_store::StoreError),
}

pub struct Engine {
    documents: BTreeMap<DocHandle, Editor>,
    next_handle: u64,
    tiles: MemoryTileStore,
    blobs: BTreeMap<ContentHash, Vec<u8>>,
    imported: std::collections::BTreeSet<ContentHash>,
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
    pub fn new() -> Self {
        Self {
            documents: BTreeMap::new(),
            next_handle: 1,
            tiles: MemoryTileStore::default(),
            blobs: BTreeMap::new(),
            imported: std::collections::BTreeSet::new(),
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

    pub fn tile(&self, hash: &ContentHash) -> Option<std::sync::Arc<TileData>> {
        self.tiles.get(hash)
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
                        && self.tiles.get(&hash).is_none()
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
        Ok(changes)
    }
    pub fn undo(&mut self, handle: DocHandle) -> Result<Option<ChangeSet>, EngineError> {
        let c = self
            .documents
            .get_mut(&handle)
            .ok_or(EngineError::NotFound(handle))?
            .undo()?;
        self.maintain();
        Ok(c)
    }
    pub fn redo(&mut self, handle: DocHandle) -> Result<Option<ChangeSet>, EngineError> {
        let c = self
            .documents
            .get_mut(&handle)
            .ok_or(EngineError::NotFound(handle))?
            .redo()?;
        self.maintain();
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
            let transform = node.props.transform.0.map(|value| value as f32);
            let blend = render_blend(node.props.blend);
            nodes.push(RNode {
                key: id,
                version: doc.revision,
                visible: node.props.visible,
                transform,
                opacity: node.props.opacity,
                blend,
                effects: node.props.effects.len(),
                features: alto_gpu::RFeatures {
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
    fn get(&self, h: &ContentHash) -> Option<std::sync::Arc<TileData>> {
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
    fn encoded(&self, h: ContentHash) -> Option<Vec<u8>> {
        if let Some(t) = self.tile(&h) {
            Some(alto_store::encode_object(
                match t.format {
                    alto_core::PixelFormat::Rgba8 => 4,
                    alto_core::PixelFormat::R8 => 1,
                },
                &t.pixels,
            ))
        } else {
            self.blobs.get(&h).map(|b| alto_store::encode_object(0, b))
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
    pub fn take_fault_error(&self) -> Option<String> {
        self.tiles.take_fault()
    }
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
    ) -> Result<(DocHandle, Vec<String>), EngineError> {
        for (h, b) in objects {
            if b[8] == 1 || b[8] == 4 {
                self.tiles.insert_verified(
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
        for h in hashes {
            if let Some(b) = self.encoded(h) {
                entries.insert(format!("objects/{h}"), b);
            }
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

impl Engine {
    pub fn pin_import(&mut self, h: ContentHash) {
        self.imported.insert(h);
    }
    fn maintain(&mut self) {
        for editor in self.documents.values_mut() {
            loop {
                let mut history = std::collections::BTreeSet::new();
                for op in editor.retained_ops() {
                    op_hashes(&op, &mut history);
                }
                let mut current = std::collections::BTreeSet::new();
                for n in editor.doc.nodes.values() {
                    node_hashes(n, &mut current);
                }
                if history.difference(&current).count() <= 2048 || !editor.drop_oldest_undo() {
                    break;
                }
            }
        }
        let mut current = self.imported.clone();
        for editor in self.documents.values() {
            for n in editor.doc.nodes.values() {
                node_hashes(n, &mut current);
            }
            if let Some(t) = &editor.doc.selection {
                current.extend(t.tiles.values().copied());
            }
            for op in editor.retained_ops() {
                op_hashes(&op, &mut current);
            }
        }
        let mut reachable = current.clone();
        for v in self.versions.values() {
            reachable.extend(v.retained.iter().copied());
        }
        if self.backed {
            self.tiles.separate(&current, &reachable);
        } else {
            self.tiles.retain(&reachable);
        }
    }
}
#[cfg(test)]
mod package_tests {
    use super::*;
    use alto_core::{PixelFormat, TileKey};
    use alto_doc::{Node, TileChange};
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
            e.tile(&hash).unwrap().pixels,
            recovered.tile(&hash).unwrap().pixels
        );
        let full = e.save_package(d).unwrap();
        let mut fresh = Engine::new();
        let (f, warnings) = fresh.open_package(&full).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(fresh.snapshot(f).unwrap(), e.snapshot(d).unwrap());
        assert_eq!(fresh.tile(&hash).unwrap().hash(), hash);
    }
}

#[cfg(test)]
mod m6_tests;

#[cfg(test)]
mod m78_tests;
