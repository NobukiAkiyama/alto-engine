use super::branches::fail;
use super::*;
use alto_store::Manifest;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GcCounts {
    pub kept: usize,
    pub removed: usize,
    pub kept_bytes: u64,
    pub removed_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GcReport {
    pub warnings: Vec<String>,
    pub dry_run: bool,
    pub complete: bool,
    pub baseline: String,
    pub roots: usize,
    pub by_kind: BTreeMap<String, GcCounts>,
}
pub struct GcPlan {
    pub report: GcReport,
    pub entries: BTreeMap<String, Vec<u8>>,
    pub remove: Vec<String>,
}
impl VersionStore {
    pub(super) fn root_commits(&self) -> BTreeSet<ContentHash> {
        let mut roots: BTreeSet<_> = self.refs.all_roots().chain(self.head).collect();
        if let Some(plan) = &self.merge {
            roots.extend(plan.roots());
        }
        roots
    }
    fn reachable_commits(&self) -> Result<BTreeSet<ContentHash>, EngineError> {
        let mut keep = BTreeSet::new();
        for root in self.root_commits() {
            keep.extend(self.ancestors(root)?);
        }
        Ok(keep)
    }
}
impl Engine {
    pub fn prepare_gc(
        &self,
        h: DocHandle,
        stored: &BTreeMap<String, Vec<u8>>,
        dry_run: bool,
    ) -> Result<GcPlan, EngineError> {
        let manifest_bytes = stored
            .get("manifest.json")
            .ok_or_else(|| fail("CORRUPT_DATA", "missing manifest"))?;
        let manifest: Manifest = serde_json::from_slice(manifest_bytes)
            .map_err(|e| fail("CORRUPT_DATA", e.to_string()))?;
        if !manifest.is_full() || !alto_doc::SUPPORTED_FORMATS.contains(&manifest.document_format) {
            return Err(fail("UNSUPPORTED", "GC package format"));
        }
        let baseline = alto_store::sha256_hex(manifest_bytes);
        let s = self.versions(h)?;
        s.validate_graph()?;
        if manifest.head != s.head
            || manifest.refs != s.refs
            || manifest.snapshot != ContentHash::of(&self.snapshot(h)?)
        {
            return Err(fail(
                "CONFLICT",
                "save all working and reference changes before GC",
            ));
        }
        // Validate all candidates too; corruption must never be explained away as unreachable.
        for (name, bytes) in stored {
            if alto_store::is_immutable(name) && !alto_store::bytes_match_name(name, bytes) {
                return Err(fail(
                    "CORRUPT_DATA",
                    format!("GC candidate is corrupt: {name}"),
                ));
            }
        }
        let decoded = alto_store::decode_entries(stored.clone())?;
        if !decoded.warnings.is_empty() {
            return Err(fail("CORRUPT_DATA", "GC package has damaged entries"));
        }
        let mut validator = Engine::new();
        let (_, warnings) = validator.open_entries(stored.clone())?;
        if !warnings.is_empty() {
            return Err(fail(
                "CORRUPT_DATA",
                "GC package has missing or damaged references",
            ));
        }
        let commits = s.reachable_commits()?;
        let mut snapshots = BTreeMap::new();
        let mut hashes = doc_hashes(self.document(h)?);
        for hash in &commits {
            let c = &s.commits[hash];
            let bytes = s
                .snapshots
                .get(&c.snapshot)
                .ok_or_else(|| fail("CORRUPT_DATA", "missing snapshot"))?;
            hashes.extend(doc_hashes(&Document::from_snapshot_bytes(bytes)?));
            snapshots.insert(c.snapshot, bytes.clone());
        }
        let history = self.documents[&h].history();
        let mut direct_objects = BTreeSet::new();
        for tx in history.undo.iter().chain(&history.redo) {
            crate::tx_hashes(tx, &mut direct_objects);
        }
        for (id, stage) in &self.staged_tiles {
            if self.stage_documents.get(id) == Some(&h) {
                direct_objects.extend(stage.iter().copied());
            }
        }
        hashes.extend(direct_objects.iter().copied());
        let mut objects = BTreeMap::new();
        for hash in &hashes {
            // A history object may be only in the folder, which was just read and verified.
            let bytes = match self.encoded(*hash)? {
                Some(bytes) => Some(bytes),
                None if self.external_objects(h)?.contains(hash) => {
                    stored.get(&alto_store::object_path(hash)).cloned()
                }
                None => None,
            };
            objects.insert(*hash, bytes.ok_or(EngineError::MissingTile(*hash))?);
        }
        let mut refs = s.refs.clone();
        if !dry_run {
            refs.generation = refs
                .generation
                .checked_add(1)
                .ok_or_else(|| fail("RESOURCE_LIMIT", "generation overflow"))?;
            refs.gc_generation = refs
                .gc_generation
                .checked_add(1)
                .ok_or_else(|| fail("RESOURCE_LIMIT", "GC generation overflow"))?;
        }
        let parts = PackageParts {
            snapshot: self.snapshot(h)?,
            objects,
            snapshots,
            commits: commits
                .iter()
                .map(|h| (*h, s.commit_bytes[h].clone()))
                .collect(),
            head: s.head,
            refs,
            external: None,
        };
        let entries = alto_store::folder_entries(&parts)?;
        let mut by_kind = BTreeMap::<String, GcCounts>::new();
        let mut remove = vec![];
        for (name, bytes) in stored.iter().filter(|(n, _)| alto_store::is_immutable(n)) {
            let kind = name.split('/').next().unwrap().to_string();
            let c = by_kind.entry(kind).or_default();
            if entries.contains_key(name) {
                c.kept += 1;
                c.kept_bytes += bytes.len() as u64;
            } else {
                c.removed += 1;
                c.removed_bytes += bytes.len() as u64;
                remove.push(name.clone());
            }
        }
        for (name, bytes) in entries
            .iter()
            .filter(|(n, _)| alto_store::is_immutable(n) && !stored.contains_key(*n))
        {
            let c = by_kind
                .entry(name.split('/').next().unwrap().into())
                .or_default();
            c.kept += 1;
            c.kept_bytes += bytes.len() as u64;
        }
        let roots = s.root_commits().len() + 1 + direct_objects.len();
        Ok(GcPlan {
            report: GcReport {
                warnings: vec![],
                dry_run,
                complete: true,
                baseline,
                roots,
                by_kind,
            },
            entries,
            remove,
        })
    }
    pub fn adopt_gc(&mut self, h: DocHandle, manifest_bytes: &[u8]) -> Result<(), EngineError> {
        let manifest: Manifest = serde_json::from_slice(manifest_bytes)
            .map_err(|e| fail("CORRUPT_DATA", e.to_string()))?;
        let s = self.versions(h)?;
        let mut refs = s.refs.clone();
        refs.generation = refs
            .generation
            .checked_add(1)
            .ok_or_else(|| fail("RESOURCE_LIMIT", "generation overflow"))?;
        refs.gc_generation = refs
            .gc_generation
            .checked_add(1)
            .ok_or_else(|| fail("RESOURCE_LIMIT", "generation overflow"))?;
        if !manifest.is_full()
            || !alto_doc::SUPPORTED_FORMATS.contains(&manifest.document_format)
            || manifest.head != s.head
            || manifest.refs != refs
            || manifest.snapshot != ContentHash::of(&self.snapshot(h)?)
        {
            return Err(fail("CONFLICT", "GC state changed"));
        }
        let s = self.versions.entry(h).or_default();
        s.refs = refs;
        s.merge = None;
        s.gc_unreachable();
        self.maintain();
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn gc_folder(
        &mut self,
        h: DocHandle,
        dir: &std::path::Path,
        dry_run: bool,
        expected: Option<&str>,
    ) -> Result<GcReport, EngineError> {
        let session = alto_store::folder::lock(dir)?;
        self.gc_session(h, &session, dry_run, expected)
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn gc_session(
        &mut self,
        h: DocHandle,
        session: &impl GcFolder,
        dry_run: bool,
        expected: Option<&str>,
    ) -> Result<GcReport, EngineError> {
        let stored = session.read()?;
        let plan = self.prepare_gc(h, &stored, dry_run)?;
        if expected.is_some_and(|s| s != plan.report.baseline) {
            return Err(fail("CONFLICT", "GC preview expired"));
        }
        if dry_run {
            return Ok(plan.report);
        }
        let old = stored.get("manifest.json").unwrap();
        // Any record from an interrupted GC is retired only after a fresh complete mark.
        let record = serde_json::json!({"phase":"prepared","baseline":plan.report.baseline,"remove":plan.remove});
        session
            .write_gc_record(&serde_json::to_vec(&record).map_err(|e| invalid(e.to_string()))?)?;
        let next = &plan.entries["manifest.json"];
        // A confirmed publication is adopted before anything else can fail, so the
        // open document never stays a generation behind its folder. A failed write
        // may still have published; readback settles that, and deletion waits for a
        // run whose publication and adoption are both confirmed.
        let publication = match session.write(&plan.entries, Some(ContentHash::of(old))) {
            Ok(publication) => {
                self.adopt_gc(h, next)?;
                publication
            }
            Err(e) => {
                if session
                    .read()
                    .is_ok_and(|now| now.get("manifest.json") == Some(next))
                {
                    self.adopt_gc(h, next)?;
                }
                return Err(e.into());
            }
        };
        for (path_index, path) in plan.remove.iter().enumerate() {
            session.delete_immutable(path).map_err(|e| {
                fail(
                    "STORAGE",
                    format!("GC stopped after {path_index} deletions: {e}"),
                )
            })?;
        }
        let after = session.read()?;
        let mut verify = Engine::new();
        let (_, warnings) = verify.open_entries(after)?;
        if !warnings.is_empty() {
            return Err(fail("CORRUPT_DATA", "GC root validation failed"));
        }
        session.write_gc_record(
            &serde_json::to_vec(
                &serde_json::json!({"phase":"complete","baseline":plan.report.baseline}),
            )
            .map_err(|e| invalid(e.to_string()))?,
        )?;
        let mut report = plan.report;
        report.warnings.extend(publication.warnings);
        Ok(report)
    }
    /// Produces a new archive; replacing an existing file is the target's responsibility.
    pub fn compact_archive(
        &self,
        h: DocHandle,
        bytes: &[u8],
    ) -> Result<(Vec<u8>, GcReport), EngineError> {
        let stored = alto_store::read_entries(bytes)?;
        let plan = self.prepare_gc(h, &stored, false)?;
        let mut entries = plan.entries;
        entries.remove("document.json");
        entries.remove(".gitattributes");
        entries.remove(".gitignore");
        let bytes = alto_store::write_entries(&entries)?;
        let mut verifier = Engine::new();
        let (_, warnings) = verifier.open_package(&bytes)?;
        if !warnings.is_empty() {
            return Err(fail("CORRUPT_DATA", "compacted archive validation failed"));
        }
        let mut report = plan.report;
        report.dry_run = false;
        Ok((bytes, report))
    }
}
/// The locked folder operations a GC uses, so tests can fail one of them.
#[cfg(not(target_arch = "wasm32"))]
trait GcFolder {
    fn read(&self) -> Result<BTreeMap<String, Vec<u8>>, alto_store::StoreError>;
    fn write(
        &self,
        entries: &BTreeMap<String, Vec<u8>>,
        expected: Option<ContentHash>,
    ) -> Result<alto_store::folder::FolderReport, alto_store::StoreError>;
    fn write_gc_record(&self, record: &[u8]) -> Result<(), alto_store::StoreError>;
    fn delete_immutable(&self, path: &str) -> Result<(), alto_store::StoreError>;
}
#[cfg(not(target_arch = "wasm32"))]
impl GcFolder for alto_store::folder::LockedFolder {
    fn read(&self) -> Result<BTreeMap<String, Vec<u8>>, alto_store::StoreError> {
        alto_store::folder::LockedFolder::read(self)
    }
    fn write(
        &self,
        entries: &BTreeMap<String, Vec<u8>>,
        expected: Option<ContentHash>,
    ) -> Result<alto_store::folder::FolderReport, alto_store::StoreError> {
        alto_store::folder::LockedFolder::write(self, entries, expected)
    }
    fn write_gc_record(&self, record: &[u8]) -> Result<(), alto_store::StoreError> {
        alto_store::folder::LockedFolder::write_gc_record(self, record)
    }
    fn delete_immutable(&self, path: &str) -> Result<(), alto_store::StoreError> {
        alto_store::folder::LockedFolder::delete_immutable(self, path)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use alto_doc::{DocumentMeta, Node, Op, PropPatch};
    fn setup() -> (Engine, DocHandle, ContentHash) {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(16, 16)).unwrap();
        let node = Node::raster("n");
        let id = node.id;
        let root = e.document(h).unwrap().root;
        e.transact(
            h,
            "add",
            vec![Op::InsertNode {
                node,
                parent: root,
                index: 0,
            }],
            0,
        )
        .unwrap();
        e.commit(h, "base", 1, None, false).unwrap();
        e.create_branch(h, "discard", None, true).unwrap();
        let rev = e.document(h).unwrap().revision;
        e.transact(
            h,
            "edit",
            vec![Op::SetProps {
                id,
                patch: PropPatch {
                    name: Some("discard".into()),
                    ..Default::default()
                },
            }],
            rev,
        )
        .unwrap();
        let removed = e.commit(h, "discard", 2, None, false).unwrap().hash;
        e.checkout(h, "main").unwrap();
        e.delete_branch(h, "discard", true).unwrap();
        (e, h, removed)
    }
    #[test]
    fn gc_only_marks_stages_owned_by_its_document() {
        let (mut engine, b, _) = setup();
        let stored = engine.folder_entries(b).unwrap();
        let a = engine
            .create_document(DocumentMeta::srgb_u8(16, 16))
            .unwrap();
        let missing = ContentHash::of(b"an external tile belonging to document A");
        let stage_a = engine.begin_tile_stage_for(a);
        engine
            .staged_tiles
            .get_mut(&stage_a)
            .unwrap()
            .insert(missing);
        assert!(engine.prepare_gc(b, &stored, true).is_ok());
        let stage_b = engine.begin_tile_stage_for(b);
        engine
            .staged_tiles
            .get_mut(&stage_b)
            .unwrap()
            .insert(missing);
        assert!(
            matches!(engine.prepare_gc(b, &stored, true), Err(EngineError::MissingTile(hash)) if hash == missing)
        );
        engine.release_staged_tiles(stage_a);
        engine.release_staged_tiles(stage_b);
        assert!(engine.stage_documents.is_empty());
    }
    #[test]
    fn archive_gc_keeps_all_references_and_rejects_corrupt_garbage() {
        let (mut e, h, removed) = setup();
        let stored = e.folder_entries(h).unwrap();
        let plan = e.prepare_gc(h, &stored, true).unwrap();
        assert!(plan.remove.contains(&alto_store::commit_path(&removed)));
        assert!(plan.report.by_kind["commits"].removed > 0);
        assert_eq!(
            plan.report.roots, 2,
            "one commit and one working snapshot; descendants are not roots"
        );
        let bytes = e.save_package(h).unwrap();
        let (compact, report) = e.compact_archive(h, &bytes).unwrap();
        assert!(!report.dry_run);
        let mut f = Engine::new();
        let (d, w) = f.open_package(&compact).unwrap();
        assert!(w.is_empty());
        assert!(f.commit_view(d, removed).is_err());
        assert!(e.commit_view(h, removed).is_ok());
        let mut broken = stored;
        broken
            .get_mut(&alto_store::commit_path(&removed))
            .unwrap()
            .push(b' ');
        assert!(e.prepare_gc(h, &broken, true).is_err());
    }
    #[test]
    fn native_gc_is_exclusive_and_preview_is_stale_after_change() {
        let (mut e, h, removed) = setup();
        let dir = std::env::temp_dir().join(format!("alto-gc-{}", NodeId::new().0));
        alto_store::folder::write(&dir, &e.folder_entries(h).unwrap(), None).unwrap();
        let lease = alto_store::folder::lock(&dir).unwrap();
        assert!(e.gc_folder(h, &dir, false, None).is_err());
        drop(lease);
        let preview = e.gc_folder(h, &dir, true, None).unwrap();
        assert!(e.gc_folder(h, &dir, false, Some("stale")).is_err());
        e.gc_folder(h, &dir, false, Some(&preview.baseline))
            .unwrap();
        assert!(!dir.join(alto_store::commit_path(&removed)).exists());
        let entries = alto_store::folder::read(&dir).unwrap();
        let mut f = Engine::new();
        let (d, w) = f.open_entries(entries).unwrap();
        assert!(w.is_empty());
        assert_eq!(f.version_refs(d).unwrap().gc_generation, 1);
        assert_eq!(
            e.gc_folder(h, &dir, true, None).unwrap().by_kind["commits"].removed,
            0
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    /// A locked folder whose reads fail once a manifest has been written, or whose
    /// writes publish and then report a failure.
    #[cfg(not(target_arch = "wasm32"))]
    struct Flaky {
        folder: alto_store::folder::LockedFolder,
        written: std::cell::Cell<bool>,
        fail_read_after_write: bool,
        fail_write_after_publishing: bool,
    }
    #[cfg(not(target_arch = "wasm32"))]
    impl GcFolder for Flaky {
        fn read(&self) -> Result<BTreeMap<String, Vec<u8>>, alto_store::StoreError> {
            if self.fail_read_after_write && self.written.get() {
                return Err(alto_store::StoreError::Io("read failed".into()));
            }
            self.folder.read()
        }
        fn write(
            &self,
            entries: &BTreeMap<String, Vec<u8>>,
            expected: Option<ContentHash>,
        ) -> Result<alto_store::folder::FolderReport, alto_store::StoreError> {
            let report = self.folder.write(entries, expected)?;
            self.written.set(true);
            if self.fail_write_after_publishing {
                return Err(alto_store::StoreError::Io("write result lost".into()));
            }
            Ok(report)
        }
        fn write_gc_record(&self, record: &[u8]) -> Result<(), alto_store::StoreError> {
            self.folder.write_gc_record(record)
        }
        fn delete_immutable(&self, path: &str) -> Result<(), alto_store::StoreError> {
            self.folder.delete_immutable(path)
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn flaky(dir: &std::path::Path, read: bool, write: bool) -> Flaky {
        Flaky {
            folder: alto_store::folder::lock(dir).unwrap(),
            written: false.into(),
            fail_read_after_write: read,
            fail_write_after_publishing: write,
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_published_gc_is_adopted_even_when_the_readback_fails() {
        let (mut e, h, removed) = setup();
        let dir = std::env::temp_dir().join(format!("alto-gc-{}", NodeId::new().0));
        alto_store::folder::write(&dir, &e.folder_entries(h).unwrap(), None).unwrap();
        let session = flaky(&dir, true, false);
        assert!(e.gc_session(h, &session, false, None).is_err());
        drop(session);
        // The folder holds the new generation, and so does the open document.
        let mut f = Engine::new();
        let (d, _) = f
            .open_entries(alto_store::folder::read(&dir).unwrap())
            .unwrap();
        assert_eq!(f.version_refs(d).unwrap().gc_generation, 1);
        assert_eq!(e.version_refs(h).unwrap().gc_generation, 1);
        // Publication and adoption were confirmed, so the sweep ran; only the final
        // check could not read. A retry passes the manifest comparison.
        assert!(!dir.join(alto_store::commit_path(&removed)).exists());
        let report = e.gc_folder(h, &dir, false, None).unwrap();
        assert_eq!(report.by_kind["commits"].removed, 0);
        assert_eq!(e.version_refs(h).unwrap().gc_generation, 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn an_ambiguous_write_is_reconciled_by_readback_and_deletes_nothing() {
        let (mut e, h, removed) = setup();
        let dir = std::env::temp_dir().join(format!("alto-gc-{}", NodeId::new().0));
        alto_store::folder::write(&dir, &e.folder_entries(h).unwrap(), None).unwrap();
        let session = flaky(&dir, false, true);
        assert!(e.gc_session(h, &session, false, None).is_err());
        drop(session);
        assert_eq!(e.version_refs(h).unwrap().gc_generation, 1);
        assert!(dir.join(alto_store::commit_path(&removed)).exists());
        e.gc_folder(h, &dir, false, None).unwrap();
        assert!(!dir.join(alto_store::commit_path(&removed)).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod interrupted_tests {
    use super::*;
    use alto_doc::{DocumentMeta, Node, Op, PropPatch};
    #[test]
    fn resumes_after_only_an_unreachable_parent_was_deleted() {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(16, 16)).unwrap();
        let node = Node::raster("base");
        let id = node.id;
        let root = e.document(h).unwrap().root;
        e.transact(
            h,
            "add",
            vec![Op::InsertNode {
                node,
                parent: root,
                index: 0,
            }],
            0,
        )
        .unwrap();
        let base = e.commit(h, "base", 1, None, false).unwrap().hash;
        e.create_branch(h, "discard", None, true).unwrap();
        let mut garbage = vec![];
        for index in 2..=3 {
            let rev = e.document(h).unwrap().revision;
            e.transact(
                h,
                "name",
                vec![Op::SetProps {
                    id,
                    patch: PropPatch {
                        name: Some(format!("discard {index}")),
                        ..Default::default()
                    },
                }],
                rev,
            )
            .unwrap();
            garbage.push(e.commit(h, "discard", index, None, false).unwrap().hash);
        }
        e.checkout(h, "main").unwrap();
        e.delete_branch(h, "discard", true).unwrap();
        let dir = std::env::temp_dir().join(format!("alto-interrupted-gc-{}", NodeId::new().0));
        alto_store::folder::write(&dir, &e.folder_entries(h).unwrap(), None).unwrap();
        let session = alto_store::folder::lock(&dir).unwrap();
        let stored = session.read().unwrap();
        let plan = e.prepare_gc(h, &stored, false).unwrap();
        session.write_gc_record(br#"{"phase":"prepared"}"#).unwrap();
        session
            .write(
                &plan.entries,
                Some(ContentHash::of(&stored["manifest.json"])),
            )
            .unwrap();
        session
            .delete_immutable(&alto_store::commit_path(&garbage[0]))
            .unwrap();
        drop(session);
        let mut fresh = Engine::new();
        let (d, w) = fresh
            .open_entries(alto_store::folder::read(&dir).unwrap())
            .unwrap();
        assert!(w.is_empty());
        assert_eq!(fresh.version_status(d).unwrap().head, Some(base));
        fresh.gc_folder(d, &dir, false, None).unwrap();
        for hash in garbage {
            assert!(!dir.join(alto_store::commit_path(&hash)).exists());
        }
        assert!(dir.join(alto_store::commit_path(&base)).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Engine {
    pub fn gc_archive_file(
        &mut self,
        h: DocHandle,
        path: &std::path::Path,
        dry_run: bool,
        expected: Option<&str>,
    ) -> Result<GcReport, EngineError> {
        let target = alto_store::archive::lock(path)?;
        self.gc_archive_session(h, &target, dry_run, expected)
    }
    fn gc_archive_session(
        &mut self,
        h: DocHandle,
        target: &impl GcArchive,
        dry_run: bool,
        expected: Option<&str>,
    ) -> Result<GcReport, EngineError> {
        let original = target.read()?.ok_or_else(|| fail("NOT_FOUND", "archive"))?;
        let stored = alto_store::read_entries(&original)?;
        let plan = self.prepare_gc(h, &stored, dry_run)?;
        if expected.is_some_and(|e| e != plan.report.baseline) {
            return Err(fail("CONFLICT", "GC preview expired"));
        }
        if dry_run {
            return Ok(plan.report);
        }
        let manifest = plan.entries["manifest.json"].clone();
        let mut entries = plan.entries;
        entries.remove("document.json");
        entries.remove(".gitattributes");
        entries.remove(".gitignore");
        let bytes = alto_store::write_entries(&entries)?;
        let mut validator = Engine::new();
        let (_, warnings) = validator.open_package(&bytes)?;
        if !warnings.is_empty() {
            return Err(fail("CORRUPT_DATA", "archive roots failed validation"));
        }
        // As for a folder: adopt a confirmed replacement at once, and let readback
        // settle only a replacement that reported a failure.
        if let Err(e) = target.replace(&bytes, &alto_store::sha256_hex(&original)) {
            if target
                .read()
                .is_ok_and(|now| now.as_deref() == Some(bytes.as_slice()))
            {
                self.adopt_gc(h, &manifest)?;
            }
            return Err(e.into());
        }
        self.adopt_gc(h, &manifest)?;
        Ok(plan.report)
    }
}
/// The locked archive operations a GC uses, so tests can fail one of them.
#[cfg(not(target_arch = "wasm32"))]
trait GcArchive {
    fn read(&self) -> Result<Option<Vec<u8>>, alto_store::StoreError>;
    fn replace(&self, bytes: &[u8], expected: &str) -> Result<(), alto_store::StoreError>;
}
#[cfg(not(target_arch = "wasm32"))]
impl GcArchive for alto_store::archive::LockedArchive {
    fn read(&self) -> Result<Option<Vec<u8>>, alto_store::StoreError> {
        alto_store::archive::LockedArchive::read(self)
    }
    fn replace(&self, bytes: &[u8], expected: &str) -> Result<(), alto_store::StoreError> {
        alto_store::archive::LockedArchive::replace(self, bytes, expected)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod archive_tests {
    use super::*;
    use alto_doc::DocumentMeta;
    #[test]
    fn archive_gc_replaces_atomically_with_reference_generation() {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(16, 16)).unwrap();
        let c = e.commit(h, "first", 1, None, false).unwrap();
        let dir = std::env::temp_dir().join(format!("alto-archive-gc-{}", NodeId::new().0));
        let path = dir.join("image.alto");
        let bytes = e.save_package(h).unwrap();
        {
            let file = alto_store::archive::lock(&path).unwrap();
            file.replace(&bytes, "").unwrap();
        }
        let preview = e.gc_archive_file(h, &path, true, None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        e.gc_archive_file(h, &path, false, Some(&preview.baseline))
            .unwrap();
        let mut f = Engine::new();
        let (d, w) = f.open_package(&std::fs::read(&path).unwrap()).unwrap();
        assert!(w.is_empty());
        assert_eq!(f.version_status(d).unwrap().head, Some(c.hash));
        assert_eq!(f.version_refs(d).unwrap().gc_generation, 1);
        assert_eq!(e.version_refs(h).unwrap().gc_generation, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
    /// A locked archive whose reads fail once it has been replaced.
    struct FlakyArchive {
        archive: alto_store::archive::LockedArchive,
        replaced: std::cell::Cell<bool>,
    }
    impl GcArchive for FlakyArchive {
        fn read(&self) -> Result<Option<Vec<u8>>, alto_store::StoreError> {
            if self.replaced.get() {
                return Err(alto_store::StoreError::Io("read failed".into()));
            }
            self.archive.read()
        }
        fn replace(&self, bytes: &[u8], expected: &str) -> Result<(), alto_store::StoreError> {
            self.archive.replace(bytes, expected)?;
            self.replaced.set(true);
            Ok(())
        }
    }
    #[test]
    fn a_replaced_archive_is_adopted_even_when_the_readback_fails() {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(16, 16)).unwrap();
        e.commit(h, "first", 1, None, false).unwrap();
        let dir = std::env::temp_dir().join(format!("alto-archive-gc-{}", NodeId::new().0));
        let path = dir.join("image.alto");
        let archive = alto_store::archive::lock(&path).unwrap();
        archive.replace(&e.save_package(h).unwrap(), "").unwrap();
        let flaky = FlakyArchive {
            archive,
            replaced: false.into(),
        };
        e.gc_archive_session(h, &flaky, false, None).unwrap();
        drop(flaky);
        let mut f = Engine::new();
        let (d, _) = f.open_package(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(f.version_refs(d).unwrap().gc_generation, 1);
        assert_eq!(e.version_refs(h).unwrap().gc_generation, 1);
        // A retry passes the manifest comparison.
        e.gc_archive_file(h, &path, false, None).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
