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
        let mut roots: BTreeSet<_> = self.refs.roots().chain(self.head).collect();
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
        if manifest.format != 2 || manifest.document_format != 5 {
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
        for stage in self.staged_tiles.values() {
            direct_objects.extend(stage.iter().copied());
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
        if manifest.format != 2
            || manifest.document_format != 5
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
        let publication = session.write(&plan.entries, Some(ContentHash::of(old)));
        let next = &plan.entries["manifest.json"];
        if session.read()?.get("manifest.json") == Some(next) {
            self.adopt_gc(h, next)?;
        }
        let publication = publication?;
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
        let publication = target.replace(&bytes, &alto_store::sha256_hex(&original));
        if target.read()?.as_deref() == Some(bytes.as_slice()) {
            self.adopt_gc(h, &manifest)?;
        }
        publication?;
        Ok(plan.report)
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
}
