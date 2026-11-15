//! History objects read from the folder when an operation needs them.
//! See doc/design/ON-DEMAND-HISTORY-OBJECTS.md.
use super::branches::fail;
use super::*;
use serde::Deserialize;

/// Objects a document may leave in its folder.
#[derive(Default)]
pub(crate) struct LazyObjects {
    /// Objects known to be in the folder.
    pub(crate) known: BTreeSet<ContentHash>,
    /// Kept in memory for the operation in progress.
    pub(crate) pinned: BTreeSet<ContentHash>,
    /// Opened from a thin .alto: its versions' objects belong in a history store.
    pub(crate) external_history: bool,
}

/// Why a history store cannot serve the document it was found for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreMismatch {
    /// The store lacks objects the versions need, or cannot be read.
    Missing,
    /// The store belongs to a different document.
    OtherDocument,
}

impl StoreMismatch {
    pub fn reason(&self) -> &'static str {
        match self {
            StoreMismatch::Missing => "missing",
            StoreMismatch::OtherDocument => "other-document",
        }
    }
}

/// What an operation is about to read.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObjectTarget {
    Commit { hash: ContentHash },
    Branch { name: String },
    Merge { name: String },
    MergePlan,
    All,
}

impl Engine {
    fn target_docs(
        &self,
        h: DocHandle,
        target: &ObjectTarget,
    ) -> Result<Vec<Document>, EngineError> {
        let s = self.versions(h)?;
        let mut commits = BTreeSet::new();
        match target {
            ObjectTarget::Commit { hash } => {
                commits.insert(*hash);
            }
            ObjectTarget::Branch { name } => {
                commits.extend(s.refs.branches.get(name).map(|b| b.head))
            }
            ObjectTarget::Merge { name } => {
                let Some(theirs) = s.refs.branches.get(name).map(|b| b.head) else {
                    return Ok(vec![]);
                };
                commits.insert(theirs);
                if let Some(ours) = s.head {
                    commits.insert(ours);
                    let (a, b) = (s.ancestors(ours)?, s.ancestors(theirs)?);
                    let common: BTreeSet<_> = a.intersection(&b).copied().collect();
                    let mut best = common.clone();
                    for c in &common {
                        for parent in &s.commits[c].parents {
                            best.remove(parent);
                        }
                    }
                    commits.extend(best);
                }
            }
            ObjectTarget::MergePlan => {
                if let Some(plan) = &s.merge {
                    commits.extend(plan.roots());
                }
            }
            ObjectTarget::All => {
                let mut docs = vec![];
                for bytes in s.snapshots.values() {
                    docs.push(Document::from_snapshot_bytes(bytes)?);
                }
                return Ok(docs);
            }
        }
        // An unknown commit is the operation's own error to report.
        Ok(commits
            .into_iter()
            .filter_map(|c| self.resolve(h, VersionRef::Commit(c)).ok())
            .collect())
    }
    fn present(&self, hash: &ContentHash) -> bool {
        self.tiles.reachable(hash) || self.blobs.contains_key(hash)
    }
    /// Object paths the target needs that are only in the folder.
    pub fn missing_objects(
        &self,
        h: DocHandle,
        target: &ObjectTarget,
    ) -> Result<Vec<String>, EngineError> {
        let Some(lazy) = self.lazy.get(&h) else {
            return Ok(vec![]);
        };
        let mut hashes = BTreeSet::new();
        for doc in self.target_docs(h, target)? {
            hashes.extend(doc_hashes(&doc));
        }
        hashes.retain(|hash| !self.present(hash));
        if let Some(hash) = hashes.iter().find(|hash| !lazy.known.contains(hash)) {
            return Err(fail(
                "CORRUPT_DATA",
                format!("history object missing: {hash}"),
            ));
        }
        Ok(hashes.iter().map(alto_store::object_path).collect())
    }
    /// Refuses before anything changes when an operation would land on pixels not in memory.
    pub(super) fn require_objects(
        &self,
        h: DocHandle,
        target: &ObjectTarget,
    ) -> Result<(), EngineError> {
        if self.missing_objects(h, target)?.is_empty() {
            Ok(())
        } else {
            Err(fail(
                "STORAGE",
                "objects-unavailable: the version's objects have not been read from its folder",
            ))
        }
    }
    /// Takes objects read from the folder and keeps them until `release_objects`. Every
    /// object is verified before any is taken, and the versions that use them are checked.
    pub fn provide_objects(
        &mut self,
        h: DocHandle,
        entries: BTreeMap<String, Vec<u8>>,
        target: &ObjectTarget,
    ) -> Result<(), EngineError> {
        let lazy = self.lazy.get(&h).ok_or_else(|| {
            fail(
                "INVALID_ARGUMENT",
                "the document reads no objects on demand",
            )
        })?;
        let mut verified = Vec::with_capacity(entries.len());
        for (path, bytes) in entries {
            let hash = path
                .rsplit('/')
                .next()
                .and_then(|name| name.parse::<ContentHash>().ok())
                .filter(|hash| alto_store::object_path(hash) == path && lazy.known.contains(hash))
                .ok_or_else(|| fail("CORRUPT_DATA", format!("unexpected object {path}")))?;
            let (tag, data) = alto_store::decode_object(&bytes)?;
            if alto_store::object_hash(tag, &data) != hash {
                return Err(fail("CORRUPT_DATA", format!("object damaged: {path}")));
            }
            verified.push((hash, bytes));
        }
        let checked = (|| -> Result<(), EngineError> {
            for (hash, bytes) in verified {
                self.install_object(hash, &bytes)?;
                if let Some(lazy) = self.lazy.get_mut(&h) {
                    lazy.pinned.insert(hash);
                }
            }
            for doc in self.target_docs(h, target)? {
                if let Some(selection) = &doc.selection {
                    self.validate_mask_plane(selection)?;
                }
                for node in doc.nodes.values() {
                    self.validate_node_content(node)?;
                }
            }
            Ok(())
        })();
        if checked.is_err() {
            self.release_objects(h);
        }
        checked
    }
    pub fn release_objects(&mut self, h: DocHandle) {
        if let Some(lazy) = self.lazy.get_mut(&h) {
            lazy.pinned.clear();
        }
        self.maintain();
    }
    /// Everything the document and its versions reference is now in the folder last saved.
    pub fn mark_folder_saved(&mut self, h: DocHandle) -> Result<(), EngineError> {
        if !self.lazy.contains_key(&h) {
            return Ok(());
        }
        let mut known = doc_hashes(self.document(h)?);
        known.extend(self.versions(h)?.retained.iter().copied());
        if let Some(lazy) = self.lazy.get_mut(&h) {
            lazy.known = known;
        }
        self.maintain();
        Ok(())
    }
    /// After a thin save: the history store just written holds everything the document and
    /// its versions reference, so the versions' objects need not stay in memory.
    /// A document that held everything until now starts reading them from the store.
    pub fn mark_store_saved(&mut self, h: DocHandle) -> Result<(), EngineError> {
        self.document(h)?;
        self.lazy.entry(h).or_insert_with(|| LazyObjects {
            external_history: true,
            ..Default::default()
        });
        self.mark_folder_saved(h)
    }
    /// Objects the document references that are only in its folder.
    pub fn external_objects(&self, h: DocHandle) -> Result<BTreeSet<ContentHash>, EngineError> {
        let Some(lazy) = self.lazy.get(&h) else {
            return Ok(BTreeSet::new());
        };
        let mut hashes = doc_hashes(self.document(h)?);
        hashes.extend(self.versions(h)?.retained.iter().copied());
        hashes.retain(|hash| !self.present(hash) && lazy.known.contains(hash));
        Ok(hashes)
    }
    /// Whether the document was opened from a thin .alto, even one with nothing outside.
    pub fn history_is_external(&self, h: DocHandle) -> bool {
        self.lazy.get(&h).is_some_and(|lazy| lazy.external_history)
    }
    /// Folder entries for the folder the document came from: objects still there are not read.
    pub fn folder_entries_keeping_external(
        &self,
        h: DocHandle,
    ) -> Result<BTreeMap<String, Vec<u8>>, EngineError> {
        let external = self.external_objects(h)?;
        let (parts, _) = self.package_parts_except(h, &external, BTreeSet::new(), true)?;
        Ok(alto_store::folder_entries(&parts)?)
    }
    /// A package of the working document alone, for when the history cannot be read.
    /// The caller asked for this explicitly: a normal save fails instead.
    pub fn save_without_history(&self, h: DocHandle) -> Result<Vec<u8>, EngineError> {
        let doc = self.document(h)?;
        let parts = alto_store::PackageParts {
            snapshot: doc.snapshot_bytes()?,
            ..Default::default()
        };
        self.encode_fetching(parts, &doc_hashes(doc))
    }
    /// Object paths to read when only the working document is loaded.
    pub fn folder_plan_on_demand(
        entries: BTreeMap<String, Vec<u8>>,
    ) -> Result<Vec<String>, EngineError> {
        let p = decode_folder(entries)?;
        let hashes = doc_hashes(&Document::from_snapshot_bytes(&p.snapshot)?);
        Ok(hashes.iter().map(alto_store::object_path).collect())
    }
    /// Opens a folder with only the working document's objects. `known` names every
    /// object in the folder, so a reference to anything else is still corruption.
    pub fn open_entries_on_demand(
        &mut self,
        entries: BTreeMap<String, Vec<u8>>,
        known: BTreeSet<ContentHash>,
    ) -> Result<(DocHandle, Vec<Warning>), EngineError> {
        let p = decode_folder(entries)?;
        self.open_decoded_package(p, false, None, Some(known))
    }
    /// Objects only versions reference: what a thin .alto leaves outside.
    pub fn version_only_objects(&self, h: DocHandle) -> Result<BTreeSet<ContentHash>, EngineError> {
        let working = doc_hashes(self.document(h)?);
        Ok(self
            .versions(h)?
            .retained
            .iter()
            .filter(|hash| !working.contains(hash))
            .copied()
            .collect())
    }
    /// Whether a history store can serve the document. `manifest_and_snapshot` holds the
    /// store's `manifest.json` and the snapshot it names; `present` lists the objects
    /// the store has. An unreadable store is `Missing`, not an error, so opening succeeds.
    pub fn check_history_store(
        &self,
        h: DocHandle,
        manifest_and_snapshot: BTreeMap<String, Vec<u8>>,
        present: &BTreeSet<ContentHash>,
    ) -> Result<Option<StoreMismatch>, EngineError> {
        let root = self.document(h)?.root;
        let store_root = manifest_and_snapshot
            .get("manifest.json")
            .and_then(|b| serde_json::from_slice::<alto_store::Manifest>(b).ok())
            .and_then(|m| manifest_and_snapshot.get(&alto_store::snapshot_path(&m.snapshot)))
            .and_then(|b| Document::from_snapshot_bytes(b).ok())
            .map(|d| d.root);
        match store_root {
            None => return Ok(Some(StoreMismatch::Missing)),
            Some(r) if r != root => return Ok(Some(StoreMismatch::OtherDocument)),
            Some(_) => {}
        }
        if self
            .external_objects(h)?
            .iter()
            .all(|o| present.contains(o))
        {
            Ok(None)
        } else {
            Ok(Some(StoreMismatch::Missing))
        }
    }
    /// Whether every root of a store's `manifest.json` (head, branch heads, tags, pins) is
    /// a version this document has. If not, the store moved on past this document: saving
    /// its refs there would drop versions. A manifest that cannot be read is not contained.
    pub fn store_roots_contained(
        &self,
        h: DocHandle,
        manifest: &[u8],
    ) -> Result<bool, EngineError> {
        let commits = &self.versions(h)?.commits;
        let Ok(m) = serde_json::from_slice::<alto_store::Manifest>(manifest) else {
            return Ok(false);
        };
        Ok(m.head
            .into_iter()
            .chain(m.refs.roots())
            .all(|root| commits.contains_key(&root)))
    }
    /// A thin package's parts, without object bytes, and the objects it holds: the
    /// working document's alone.
    pub(crate) fn thin_package_parts(
        &self,
        h: DocHandle,
    ) -> Result<(alto_store::PackageParts, BTreeSet<ContentHash>), EngineError> {
        let doc = self.document(h)?;
        let (mut parts, _) =
            self.package_parts_except(h, &BTreeSet::new(), BTreeSet::new(), false)?;
        parts.external = Some(alto_store::ExternalObjects {
            document: doc.root,
            objects: self.version_only_objects(h)?,
        });
        Ok((parts, doc_hashes(doc)))
    }
    /// A .alto with every version but only the working document's objects. The
    /// versions' objects are listed in `external-objects.json` and kept elsewhere.
    pub fn save_thin_package(&mut self, h: DocHandle) -> Result<Vec<u8>, EngineError> {
        let (parts, hashes) = self.thin_package_parts(h)?;
        let bytes = self.encode_fetching(parts, &hashes)?;
        self.backed = true;
        self.maintain();
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alto_core::{PixelFormat, TileKey};
    use alto_doc::{DocumentMeta, Node, Op, TileChange};
    use alto_raster::TileData;

    struct Fixture {
        entries: BTreeMap<String, Vec<u8>>,
        layer: NodeId,
        old: ContentHash,
        old_tile: ContentHash,
        head_tile: ContentHash,
    }
    fn paint(e: &mut Engine, d: DocHandle, layer: NodeId, v: u8) -> ContentHash {
        let hash = e
            .put_tile(TileData::new(PixelFormat::Rgba8, vec![v; 262144]).unwrap())
            .unwrap();
        let doc = e.document(d).unwrap();
        let before = match &doc.node(layer).unwrap().kind {
            alto_doc::NodeKind::Raster { tiles, .. } => tiles.tiles.values().next().copied(),
            _ => panic!(),
        };
        let revision = doc.revision;
        e.transact(
            d,
            "paint",
            vec![Op::SetTiles {
                layer,
                changes: vec![TileChange {
                    key: TileKey { tx: 0, ty: 0 },
                    before,
                    after: Some(hash),
                }],
            }],
            revision,
        )
        .unwrap();
        hash
    }
    /// Two versions on main, as a saved folder's entries.
    fn fixture() -> Fixture {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(256, 256)).unwrap();
        let node = Node::raster("layer");
        let layer = node.id;
        let parent = e.document(d).unwrap().root;
        e.transact(
            d,
            "add",
            vec![Op::InsertNode {
                node,
                parent,
                index: 0,
            }],
            0,
        )
        .unwrap();
        let old_tile = paint(&mut e, d, layer, 10);
        let old = e.commit(d, "old", 1, None, false).unwrap().hash;
        let head_tile = paint(&mut e, d, layer, 20);
        e.commit(d, "new", 2, None, false).unwrap();
        Fixture {
            entries: e.folder_entries(d).unwrap(),
            layer,
            old,
            old_tile,
            head_tile,
        }
    }
    fn known(entries: &BTreeMap<String, Vec<u8>>) -> BTreeSet<ContentHash> {
        entries
            .keys()
            .filter(|n| n.starts_with("objects/"))
            .filter_map(|n| n.rsplit('/').next()?.parse().ok())
            .collect()
    }
    fn open(f: &Fixture) -> (Engine, DocHandle) {
        let mut plan_input = f.entries.clone();
        plan_input.retain(|n, _| !n.starts_with("objects/"));
        let wanted = Engine::folder_plan_on_demand(plan_input.clone()).unwrap();
        for path in wanted {
            plan_input.insert(path.clone(), f.entries[&path].clone());
        }
        let mut e = Engine::new();
        let (d, warnings) = e
            .open_entries_on_demand(plan_input, known(&f.entries))
            .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        (e, d)
    }
    fn provide(e: &mut Engine, d: DocHandle, f: &Fixture, target: &ObjectTarget) {
        let mut entries = BTreeMap::new();
        for path in e.missing_objects(d, target).unwrap() {
            entries.insert(path.clone(), f.entries[&path].clone());
        }
        e.provide_objects(d, entries, target).unwrap();
    }

    #[test]
    fn only_the_working_document_is_read_and_history_still_lists() {
        let f = fixture();
        let (e, d) = open(&f);
        assert!(e.tile_resident(&f.head_tile));
        assert!(
            !e.tiles.reachable(&f.old_tile),
            "the old tile stays in the folder"
        );
        assert_eq!(e.external_objects(d).unwrap().len(), 1);
        // An unread object's size is not known, so neither is the package's.
        assert_eq!(e.package_bytes(d).unwrap(), None);
        assert_eq!(e.log(d).unwrap().len(), 2);
        assert!(
            !e.diff_versions(d, VersionRef::Commit(f.old), VersionRef::Working)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_history_object_outside_the_folder_is_still_corruption() {
        let f = fixture();
        let mut entries = f.entries.clone();
        entries.remove(&alto_store::object_path(&f.old_tile));
        let mut listed = known(&entries);
        listed.remove(&f.old_tile);
        entries.retain(|n, _| !n.starts_with("objects/") || n.contains(&f.head_tile.hex()));
        let error = Engine::new()
            .open_entries_on_demand(entries, listed)
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("history object missing"),
            "{error}"
        );
    }

    #[test]
    fn checkout_needs_the_objects_and_fails_without_changing_anything() {
        let f = fixture();
        let (mut e, d) = open(&f);
        let before = e.snapshot(d).unwrap();
        assert!(e.detach(d, f.old).is_err());
        assert_eq!(e.snapshot(d).unwrap(), before);
        let target = ObjectTarget::Commit { hash: f.old };
        provide(&mut e, d, &f, &target);
        e.detach(d, f.old).unwrap();
        e.release_objects(d);
        assert_eq!(e.pick_color(d, f.layer, [0.0, 0.0]).unwrap().bytes, [10; 4]);
        // The tile of the version left behind goes back to being external.
        assert!(!e.tiles.reachable(&f.head_tile));
        assert_eq!(
            e.external_objects(d).unwrap(),
            BTreeSet::from([f.head_tile])
        );
    }

    #[test]
    fn restored_tiles_stay_for_undo_after_the_pin_ends() {
        let f = fixture();
        let (mut e, d) = open(&f);
        let target = ObjectTarget::Commit { hash: f.old };
        provide(&mut e, d, &f, &target);
        e.restore_version(d, f.old).unwrap();
        e.release_objects(d);
        assert_eq!(e.pick_color(d, f.layer, [0.0, 0.0]).unwrap().bytes, [10; 4]);
        e.undo(d).unwrap();
        assert_eq!(e.pick_color(d, f.layer, [0.0, 0.0]).unwrap().bytes, [20; 4]);
        e.redo(d).unwrap();
        assert_eq!(e.pick_color(d, f.layer, [0.0, 0.0]).unwrap().bytes, [10; 4]);
    }

    #[test]
    fn damaged_or_unexpected_objects_are_refused_whole() {
        let f = fixture();
        let (mut e, d) = open(&f);
        let target = ObjectTarget::Commit { hash: f.old };
        let path = alto_store::object_path(&f.old_tile);
        let swapped = f.entries[&alto_store::object_path(&f.head_tile)].clone();
        let damaged = BTreeMap::from([(path, swapped)]);
        assert!(e.provide_objects(d, damaged, &target).is_err());
        assert!(!e.tiles.reachable(&f.old_tile));
        let other = BTreeMap::from([("objects/zz/nonsense".to_string(), vec![1, 2, 3])]);
        assert!(e.provide_objects(d, other, &target).is_err());
        assert_eq!(e.missing_objects(d, &target).unwrap().len(), 1);
    }

    #[test]
    fn saving_needs_every_object_except_to_the_same_folder() {
        let f = fixture();
        let (mut e, d) = open(&f);
        assert!(
            e.save_package(d).is_err(),
            "a normal save never drops history"
        );
        let same = e.folder_entries_keeping_external(d).unwrap();
        assert!(!same.contains_key(&alto_store::object_path(&f.old_tile)));
        assert!(same.contains_key(&alto_store::object_path(&f.head_tile)));
        assert_eq!(same["manifest.json"], f.entries["manifest.json"]);

        let working = e.save_without_history(d).unwrap();
        let (w, _) = e.open_package(&working).unwrap();
        assert_eq!(e.log(w).unwrap().len(), 0, "explicitly without history");
        e.close_document(w).unwrap();

        provide(&mut e, d, &f, &ObjectTarget::All);
        let payload = e.package_bytes(d).unwrap().expect("every object is read");
        let whole = e.save_package(d).unwrap();
        assert!(payload > 0 && payload < whole.len() as u64);
        e.release_objects(d);
        let (_, warnings) = Engine::new().open_package(&whole).unwrap();
        assert!(warnings.is_empty());
        assert!(
            !e.tiles.reachable(&f.old_tile),
            "read for the save, then released"
        );
    }

    #[test]
    fn unsaved_commits_stay_in_memory_until_saved() {
        let f = fixture();
        let (mut e, d) = open(&f);
        let tile = paint(&mut e, d, f.layer, 30);
        e.commit(d, "unsaved", 3, None, false).unwrap();
        paint(&mut e, d, f.layer, 40);
        e.commit(d, "later", 4, None, false).unwrap();
        // Not in the folder yet, so it stays even though it is not the current picture.
        assert!(e.tiles.reachable(&tile));
        e.mark_folder_saved(d).unwrap();
        assert!(e.external_objects(d).unwrap().contains(&f.old_tile));
    }

    #[test]
    fn recovery_keeps_the_external_list_and_rereads_objects() {
        let f = fixture();
        let (mut e, d) = open(&f);
        paint(&mut e, d, f.layer, 50);
        let checkpoint = e.recovery_checkpoint(d, &BTreeSet::new()).unwrap();
        assert_eq!(checkpoint.external.len(), 1);
        let meta: serde_json::Value =
            serde_json::from_slice(&checkpoint.entries["checkpoint.json"]).unwrap();
        let listed = meta["objects"].to_string();
        assert!(
            !listed.contains(&f.old_tile.hex()),
            "an external object is not expected on disk"
        );
        let mut entries = checkpoint.entries;
        entries.remove("checkpoint.json");
        let mut fresh = Engine::new();
        let (r, _) = fresh
            .open_recovery_entries(entries, &BTreeSet::new())
            .unwrap();
        assert_eq!(
            fresh.pick_color(r, f.layer, [0.0, 0.0]).unwrap().bytes,
            [50; 4]
        );
        fresh.undo(r).unwrap();
        assert_eq!(
            fresh.pick_color(r, f.layer, [0.0, 0.0]).unwrap().bytes,
            [20; 4]
        );
        assert!(
            fresh.save_package(r).is_err(),
            "history is still in the folder"
        );
        let target = ObjectTarget::All;
        provide(&mut fresh, r, &f, &target);
        assert!(fresh.save_package(r).is_ok());
    }

    #[test]
    fn an_object_only_another_documents_folder_holds_stays_in_memory() {
        let f = fixture();
        let (mut e, d) = open(&f);
        // A second document uses the old tile but knows nothing of this folder.
        let other = e.create_document(DocumentMeta::srgb_u8(256, 256)).unwrap();
        let target = ObjectTarget::Commit { hash: f.old };
        provide(&mut e, d, &f, &target);
        let node = Node::raster("other");
        let layer = node.id;
        let parent = e.document(other).unwrap().root;
        e.transact(
            other,
            "add",
            vec![Op::InsertNode {
                node,
                parent,
                index: 0,
            }],
            0,
        )
        .unwrap();
        paint(&mut e, other, layer, 10);
        e.commit(other, "uses the same tile", 1, None, false)
            .unwrap();
        paint(&mut e, other, layer, 11);
        e.commit(other, "moves on", 2, None, false).unwrap();
        e.release_objects(d);
        assert!(
            e.tiles.reachable(&f.old_tile),
            "the other document still needs it"
        );
    }

    #[test]
    fn a_checkout_after_the_checkpoint_replays_from_what_the_checkpoint_carries() {
        let f = fixture();
        let (mut e, d) = open(&f);
        let target = ObjectTarget::Commit { hash: f.old };
        provide(&mut e, d, &f, &target);
        // The order the Worker uses: checkpoint with the objects, then the transition.
        let checkpoint = e.recovery_checkpoint(d, &BTreeSet::new()).unwrap();
        assert!(checkpoint.written.contains(&f.old_tile));
        let before = e.capture_version_state(d).unwrap();
        e.detach(d, f.old).unwrap();
        let record = e.version_record(d, before.digest).unwrap();
        e.release_version_state(before);
        e.release_objects(d);

        let mut entries = checkpoint.entries;
        entries.remove("checkpoint.json");
        let mut fresh = Engine::new();
        let (r, _) = fresh
            .open_recovery_entries(entries, &BTreeSet::new())
            .unwrap();
        fresh.replay_record(r, &record).unwrap();
        fresh.release_objects(r);
        assert_eq!(
            fresh.pick_color(r, f.layer, [0.0, 0.0]).unwrap().bytes,
            [10; 4]
        );
    }

    /// The fixture opened with every object in memory.
    fn whole(f: &Fixture) -> (Engine, DocHandle) {
        let mut e = Engine::new();
        let (d, warnings) = e.open_entries(f.entries.clone()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        (e, d)
    }
    fn thin(f: &Fixture) -> Vec<u8> {
        let (mut e, d) = whole(f);
        e.save_thin_package(d).unwrap()
    }
    fn code(error: EngineError) -> String {
        error.to_string().split(':').next().unwrap().to_string()
    }
    /// A thin package of the fixture whose external list is `objects`.
    fn thin_listing(f: &Fixture, document: NodeId, objects: BTreeSet<ContentHash>) -> Vec<u8> {
        let (e, d) = whole(f);
        let mut parts = e.package_parts(d).unwrap();
        parts.objects.retain(|hash, _| *hash == f.head_tile);
        parts.external = Some(alto_store::ExternalObjects { document, objects });
        alto_store::write_entries(&alto_store::package_entries(&parts).unwrap()).unwrap()
    }

    #[test]
    fn thin_package_holds_only_the_working_objects() {
        let f = fixture();
        let (mut e, d) = whole(&f);
        assert_eq!(
            e.version_only_objects(d).unwrap(),
            BTreeSet::from([f.old_tile])
        );
        let estimate = e.thin_package_bytes(d).unwrap();
        let bytes = e.save_thin_package(d).unwrap();
        let entries = alto_store::read_entries(&bytes).unwrap();
        let objects: Vec<_> = entries
            .keys()
            .filter(|n| n.starts_with("objects/"))
            .cloned()
            .collect();
        assert_eq!(objects, vec![alto_store::object_path(&f.head_tile)]);
        assert_eq!(
            entries.keys().filter(|n| n.starts_with("commits/")).count(),
            2
        );
        let p = alto_store::decode_entries(entries.clone()).unwrap();
        let external = p
            .external
            .expect("a thin package lists its external objects");
        assert_eq!(external.objects, BTreeSet::from([f.old_tile]));
        assert_eq!(external.document, e.document(d).unwrap().root);
        // Counted like package_bytes: the entries' data, not the ZIP.
        let payload: u64 = entries.values().map(|b| b.len() as u64).sum();
        assert_eq!(estimate, payload);
        assert!(estimate < bytes.len() as u64);
        assert!(estimate < e.package_bytes(d).unwrap().unwrap());
    }

    #[test]
    fn a_thin_save_lets_the_versions_objects_leave_memory() {
        let f = fixture();
        let (mut e, d) = whole(&f);
        assert!(e.tiles.reachable(&f.old_tile));
        e.mark_store_saved(d).unwrap();
        assert_eq!(e.external_objects(d).unwrap(), BTreeSet::from([f.old_tile]));
        assert!(!e.tiles.reachable(&f.old_tile), "the version's tile stayed");
        assert!(e.tiles.reachable(&f.head_tile));
        assert!(e.history_is_external(d));
        // The next save to the same store reads nothing back.
        let entries = e.folder_entries_keeping_external(d).unwrap();
        assert!(!entries.contains_key(&alto_store::object_path(&f.old_tile)));
        // A save to a format-2 folder still needs every object.
        assert!(e.folder_entries(d).is_err());
    }

    #[test]
    fn thin_package_opens_without_the_store() {
        let f = fixture();
        let bytes = thin(&f);
        let mut e = Engine::new();
        let (d, warnings) = e.open_package(&bytes).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(e.log(d).unwrap().len(), 2);
        assert!(!e.version_status(d).unwrap().changed);
        assert!(
            !e.diff_versions(d, VersionRef::Working, VersionRef::Commit(f.old))
                .unwrap()
                .is_empty()
        );
        let target = ObjectTarget::Commit { hash: f.old };
        assert_eq!(
            e.missing_objects(d, &target).unwrap(),
            vec![alto_store::object_path(&f.old_tile)]
        );
        let before = e.snapshot(d).unwrap();
        let error = e.restore_version(d, f.old).err().unwrap();
        assert!(
            error
                .to_string()
                .starts_with("STORAGE: objects-unavailable"),
            "{error}"
        );
        assert_eq!(e.snapshot(d).unwrap(), before);
        assert_eq!(e.external_objects(d).unwrap(), BTreeSet::from([f.old_tile]));
        assert_eq!(e.pick_color(d, f.layer, [0.0, 0.0]).unwrap().bytes, [20; 4]);
    }

    #[test]
    fn history_store_check() {
        let f = fixture();
        let bytes = thin(&f);
        let mut e = Engine::new();
        let (d, _) = e.open_package(&bytes).unwrap();
        let store = |entries: &BTreeMap<String, Vec<u8>>| {
            let manifest = entries["manifest.json"].clone();
            let m: alto_store::Manifest = serde_json::from_slice(&manifest).unwrap();
            let path = alto_store::snapshot_path(&m.snapshot);
            BTreeMap::from([
                ("manifest.json".to_string(), manifest),
                (path.clone(), entries[&path].clone()),
            ])
        };
        let present = known(&f.entries);
        let ours = store(&f.entries);
        assert_eq!(
            e.check_history_store(d, ours.clone(), &present).unwrap(),
            None
        );
        let mut without = present.clone();
        without.remove(&f.old_tile);
        assert_eq!(
            e.check_history_store(d, ours.clone(), &without).unwrap(),
            Some(StoreMismatch::Missing)
        );
        let other = store(&fixture().entries);
        assert_eq!(
            e.check_history_store(d, other, &present).unwrap(),
            Some(StoreMismatch::OtherDocument)
        );
        let mut no_manifest = ours.clone();
        no_manifest.remove("manifest.json");
        let mut broken = ours.clone();
        broken.insert("manifest.json".into(), b"{".to_vec());
        let mut no_snapshot = ours.clone();
        no_snapshot.retain(|n, _| n == "manifest.json");
        let mut bad_snapshot = ours.clone();
        for (n, b) in bad_snapshot.iter_mut() {
            if n.starts_with("snapshots/") {
                *b = b"not json".to_vec();
            }
        }
        for damaged in [no_manifest, broken, no_snapshot, bad_snapshot] {
            assert_eq!(
                e.check_history_store(d, damaged, &present).unwrap(),
                Some(StoreMismatch::Missing)
            );
        }
        assert_eq!(StoreMismatch::Missing.reason(), "missing");
        assert_eq!(StoreMismatch::OtherDocument.reason(), "other-document");
    }

    #[test]
    fn a_store_that_moved_on_is_not_contained() {
        let f = fixture();
        let mut e = Engine::new();
        let (d, _) = e.open_package(&thin(&f)).unwrap();
        assert!(
            e.store_roots_contained(d, &f.entries["manifest.json"])
                .unwrap()
        );
        // The same document, saved again after one more version.
        let (mut later, l) = whole(&f);
        paint(&mut later, l, f.layer, 30);
        later.commit(l, "later", 3, None, false).unwrap();
        let ahead = later.folder_entries(l).unwrap();
        assert!(!e.store_roots_contained(d, &ahead["manifest.json"]).unwrap());
        // A tag or a pin on a version this document lacks counts too.
        let mut tagged: alto_store::Manifest =
            serde_json::from_slice(&f.entries["manifest.json"]).unwrap();
        let unknown = ContentHash::of(b"elsewhere");
        tagged.refs.tags.insert("t".into(), unknown);
        let bytes = serde_json::to_vec(&tagged).unwrap();
        assert!(!e.store_roots_contained(d, &bytes).unwrap());
        tagged.refs.tags.clear();
        tagged.refs.pinned.insert(unknown);
        let bytes = serde_json::to_vec(&tagged).unwrap();
        assert!(!e.store_roots_contained(d, &bytes).unwrap());
        assert!(!e.store_roots_contained(d, b"{").unwrap());
    }

    #[test]
    fn thin_package_external_list_must_match_versions() {
        let f = fixture();
        let (e, d) = whole(&f);
        let document = e.document(d).unwrap().root;
        // Listed correctly, the package opens.
        let right = thin_listing(&f, document, BTreeSet::from([f.old_tile]));
        Engine::new().open_package(&right).unwrap();

        let unrelated = ContentHash::of(b"no version uses this");
        let extra = thin_listing(&f, document, BTreeSet::from([f.old_tile, unrelated]));
        let mut opener = Engine::new();
        let error = opener.open_package(&extra).err().unwrap();
        assert_eq!(code(error), "CORRUPT_DATA");
        assert!(opener.versions.is_empty() && opener.documents.is_empty());

        let short = thin_listing(&f, document, BTreeSet::new());
        let error = Engine::new().open_package(&short).err().unwrap();
        assert!(
            error
                .to_string()
                .starts_with("CORRUPT_DATA: history object missing"),
            "{error}"
        );

        let other = Node::raster("another document").id;
        let elsewhere = thin_listing(&f, other, BTreeSet::from([f.old_tile]));
        let error = Engine::new().open_package(&elsewhere).err().unwrap();
        assert_eq!(code(error), "CORRUPT_DATA");
    }

    #[test]
    fn thin_package_of_a_document_without_versions() {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(256, 256)).unwrap();
        let node = Node::raster("layer");
        let layer = node.id;
        let parent = e.document(d).unwrap().root;
        e.transact(
            d,
            "add",
            vec![Op::InsertNode {
                node,
                parent,
                index: 0,
            }],
            0,
        )
        .unwrap();
        let tile = paint(&mut e, d, layer, 7);
        assert!(e.version_only_objects(d).unwrap().is_empty());
        let bytes = e.save_thin_package(d).unwrap();
        let entries = alto_store::read_entries(&bytes).unwrap();
        assert!(entries.contains_key(&alto_store::object_path(&tile)));
        let p = alto_store::decode_entries(entries).unwrap();
        assert!(p.external.unwrap().objects.is_empty());
        let mut fresh = Engine::new();
        let (r, warnings) = fresh.open_package(&bytes).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(fresh.external_objects(r).unwrap().is_empty());
        assert!(
            fresh.history_is_external(r),
            "thin even with nothing outside"
        );
        let (n, _) = fresh.open_package(&e.save_package(d).unwrap()).unwrap();
        assert!(!fresh.history_is_external(n));
        assert!(!e.history_is_external(d));
        assert_eq!(
            fresh.pick_color(r, layer, [0.0, 0.0]).unwrap().bytes,
            [7; 4]
        );
    }

    #[test]
    fn thin_package_restores_after_objects_are_provided() {
        let f = fixture();
        let bytes = thin(&f);
        let mut e = Engine::new();
        let (d, _) = e.open_package(&bytes).unwrap();
        let target = ObjectTarget::Commit { hash: f.old };
        provide(&mut e, d, &f, &target);
        e.restore_version(d, f.old).unwrap();
        e.release_objects(d);

        let (mut source, s) = whole(&f);
        let mut normal = Engine::new();
        let (n, _) = normal
            .open_package(&source.save_package(s).unwrap())
            .unwrap();
        normal.restore_version(n, f.old).unwrap();
        assert_eq!(e.state_digest(d).unwrap(), normal.state_digest(n).unwrap());
        assert_eq!(e.pick_color(d, f.layer, [0.0, 0.0]).unwrap().bytes, [10; 4]);
    }

    #[test]
    fn a_folder_never_lists_external_objects() {
        let f = fixture();
        let entries = alto_store::read_entries(&thin(&f)).unwrap();
        let refused = |result: Result<(), EngineError>| {
            let error = result.expect_err("a folder holds every object");
            assert!(error.to_string().starts_with("UNSUPPORTED"), "{error}");
        };
        refused(Engine::new().open_entries(entries.clone()).map(|_| ()));
        refused(
            Engine::new()
                .open_entries_on_demand(entries.clone(), known(&f.entries))
                .map(|_| ()),
        );
        refused(Engine::folder_plan(entries.clone()).map(|_| ()));
        refused(Engine::folder_plan_on_demand(entries).map(|_| ()));
    }

    #[test]
    fn merge_targets_cover_both_heads_and_the_base() {
        let mut e = Engine::new();
        let d = e.create_document(DocumentMeta::srgb_u8(512, 256)).unwrap();
        let node = Node::raster("layer");
        let layer = node.id;
        let parent = e.document(d).unwrap().root;
        e.transact(
            d,
            "add",
            vec![Op::InsertNode {
                node,
                parent,
                index: 0,
            }],
            0,
        )
        .unwrap();
        paint(&mut e, d, layer, 1);
        e.commit(d, "base", 1, None, false).unwrap();
        e.create_branch(d, "feature", None, true).unwrap();
        paint(&mut e, d, layer, 2);
        e.commit(d, "theirs", 2, None, false).unwrap();
        e.checkout(d, "main").unwrap();
        let revision = e.document(d).unwrap().revision;
        e.transact(
            d,
            "rename",
            vec![Op::SetProps {
                id: layer,
                patch: alto_doc::PropPatch {
                    name: Some("ours".into()),
                    ..Default::default()
                },
            }],
            revision,
        )
        .unwrap();
        e.commit(d, "ours", 3, None, false).unwrap();
        let entries = e.folder_entries(d).unwrap();
        let mut partial = entries.clone();
        partial.retain(|n, _| !n.starts_with("objects/"));
        for path in Engine::folder_plan_on_demand(partial.clone()).unwrap() {
            partial.insert(path.clone(), entries[&path].clone());
        }
        let mut lazy = Engine::new();
        let (h, _) = lazy
            .open_entries_on_demand(partial, known(&entries))
            .unwrap();
        let target = ObjectTarget::Merge {
            name: "feature".into(),
        };
        let missing = lazy.missing_objects(h, &target).unwrap();
        assert_eq!(
            missing.len(),
            1,
            "base shares the working tile, theirs does not"
        );
        let provided: BTreeMap<_, _> = missing
            .into_iter()
            .map(|p| {
                let bytes = entries[&p].clone();
                (p, bytes)
            })
            .collect();
        lazy.provide_objects(h, provided, &target).unwrap();
        let plan = lazy.prepare_merge(h, "feature").unwrap();
        assert_eq!(plan.kind, "three_way");
        let finish = ObjectTarget::MergePlan;
        assert!(lazy.missing_objects(h, &finish).unwrap().is_empty());
        lazy.finish_merge(h, &plan.token, vec![], "merge", 4, None)
            .unwrap();
        lazy.release_objects(h);
        assert_eq!(lazy.pick_color(h, layer, [0.0, 0.0]).unwrap().bytes, [2; 4]);
    }
}
