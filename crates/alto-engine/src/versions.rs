mod gc;
pub use gc::{GcCounts, GcPlan, GcReport};
mod journal;
mod lazy;
pub use journal::VersionCheckpoint;
pub(crate) use lazy::LazyObjects;
pub use lazy::ObjectTarget;
mod merge;
pub use merge::{MergeConflict, MergePlan, MergeResolution, MergeResult, MergeValue};
mod branches;
pub use branches::BranchView;
// M8: version commits, history, semantic diff, restore, and the package parts that
// carry history in both the single-file and folder forms.
use crate::{DocHandle, Engine, EngineError, Warning, node_hashes};
use alto_core::{ContentHash, NodeId};
use alto_doc::{Commit, DiffItem, DocError, Document, version::summarize};
use alto_raster::TileStore;
use alto_store::PackageParts;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Clone)]
pub struct VersionStore {
    merge: Option<merge::MergeSession>,
    pub(crate) commits: BTreeMap<ContentHash, Commit>,
    pub(crate) commit_bytes: BTreeMap<ContentHash, Vec<u8>>,
    pub(crate) snapshots: BTreeMap<ContentHash, Vec<u8>>,
    pub(crate) head: Option<ContentHash>,
    pub(crate) refs: alto_store::VersionRefs,
    /// Tiles and blobs referenced by committed snapshots (GC roots).
    pub(crate) retained: BTreeSet<ContentHash>,
    /// The head commit's document, parsed once. Version status diffs against it
    /// after every edit; parsing the snapshot each time cost ten times the diff.
    head_digest: Option<ContentHash>,
    head_doc: Option<(ContentHash, std::sync::Arc<Document>)>,
}
impl VersionStore {
    fn set_head(&mut self, head: Option<ContentHash>) {
        self.head = head;
        self.head_doc = head.and_then(|c| {
            let commit = self.commits.get(&c)?;
            let doc = Document::from_snapshot_bytes(self.snapshots.get(&commit.snapshot)?).ok()?;
            Some((c, std::sync::Arc::new(doc)))
        });
        self.head_digest = self
            .head_doc
            .as_ref()
            .map(|(_, doc)| alto_doc::digest::StateDigest::of(doc).value());
    }
}

pub struct RecoveryCheckpoint {
    pub entries: BTreeMap<String, Vec<u8>>,
    /// Objects whose bytes `entries` carries.
    pub written: Vec<ContentHash>,
    /// Objects left in the document's folder. The recovery store may drop them, so they
    /// have to be written again if they come back.
    pub external: BTreeSet<ContentHash>,
}

/// Which side of a diff: a commit or the working document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionRef {
    Working,
    Commit(ContentHash),
}

#[derive(Clone, Debug, Serialize)]
pub struct IntentView {
    pub intent: String,
    pub label: Option<String>,
    pub undone: bool,
    pub count: u32,
    pub targets: Vec<NodeId>,
    /// `None` for a node that no longer exists.
    pub target_names: Vec<Option<String>>,
}
#[derive(Clone, Debug, Serialize)]
pub struct CommitView {
    pub hash: ContentHash,
    pub parents: Vec<ContentHash>,
    pub snapshot: ContentHash,
    pub message: String,
    pub created_at: i64,
    pub author: Option<String>,
    pub intents: Vec<IntentView>,
}
#[derive(Clone, Debug, Serialize)]
pub struct VersionStatus {
    pub head: Option<ContentHash>,
    pub refs: alto_store::VersionRefs,
    pub commits: usize,
    /// Whether the working document differs from the head commit.
    pub changed: bool,
    pub changes: usize,
    pub pending_intents: usize,
}

pub(crate) fn doc_hashes(doc: &Document) -> BTreeSet<ContentHash> {
    let mut h = BTreeSet::new();
    for n in doc.nodes.values() {
        node_hashes(n, &mut h);
    }
    if let Some(s) = &doc.selection {
        h.extend(s.tiles.values().copied());
    }
    h.extend(doc.assets.values().map(|a| a.hash));
    h
}

impl VersionStore {
    fn insert(
        &mut self,
        hash: ContentHash,
        commit: Commit,
        bytes: Vec<u8>,
        snapshot: Vec<u8>,
    ) -> Result<(), DocError> {
        let doc = Document::from_snapshot_bytes(&snapshot)?;
        self.retained.extend(doc_hashes(&doc));
        self.snapshots.insert(commit.snapshot, snapshot);
        self.commits.insert(hash, commit);
        self.commit_bytes.insert(hash, bytes);
        Ok(())
    }

    /// Drops commits that cannot be reached from the head, then rebuilds the
    /// retained object set from the snapshots those commits still point at.
    /// Only `Engine::prune_versions` calls this. Commit and open keep every version.
    pub(crate) fn gc_unreachable(&mut self) {
        let mut keep = BTreeSet::new();
        let mut stack: Vec<ContentHash> = self.root_commits().into_iter().collect();
        while let Some(hash) = stack.pop() {
            if !keep.insert(hash) {
                continue;
            }
            if let Some(commit) = self.commits.get(&hash) {
                stack.extend(commit.parents.iter().copied());
            }
        }
        self.commits.retain(|hash, _| keep.contains(hash));
        self.commit_bytes.retain(|hash, _| keep.contains(hash));
        let snapshots: BTreeSet<_> = self.commits.values().map(|c| c.snapshot).collect();
        self.snapshots.retain(|hash, _| snapshots.contains(hash));
        self.retained.clear();
        for bytes in self.snapshots.values() {
            if let Ok(doc) = Document::from_snapshot_bytes(bytes) {
                self.retained.extend(doc_hashes(&doc));
            }
        }
    }
}

fn invalid(s: impl Into<String>) -> EngineError {
    DocError::Invalid(s.into()).into()
}

impl Engine {
    pub(crate) fn invalidate_merge(&mut self, h: DocHandle) {
        if let Some(s) = self.versions.get_mut(&h) {
            s.merge = None;
        }
    }
    pub(crate) fn versions(&self, h: DocHandle) -> Result<&VersionStore, EngineError> {
        self.document(h)?;
        Ok(self.versions.get(&h).unwrap_or(&self.empty_versions))
    }

    /// Records the working document as a new version on top of the head.
    pub fn commit(
        &mut self,
        h: DocHandle,
        message: &str,
        created_at: i64,
        author: Option<String>,
        allow_empty: bool,
    ) -> Result<CommitView, EngineError> {
        self.document(h)?;
        if self.versions(h)?.head.is_some() && self.versions(h)?.refs.current_branch.is_none() {
            return Err(branches::fail(
                "CONFLICT",
                "create a branch before committing in detached state",
            ));
        }
        let snapshot = self.snapshot(h)?;
        let sh = ContentHash::of(&snapshot);
        let store = self.versions.entry(h).or_default();
        let head = store.head;
        if !allow_empty
            && head
                .and_then(|c| store.commits.get(&c))
                .is_some_and(|c| c.snapshot == sh)
        {
            return Err(invalid("nothing changed since the last version"));
        }
        let editor = self.documents.get(&h).ok_or(EngineError::NotFound(h))?;
        let commit = Commit {
            format: 2,
            merge_base: None,
            parents: head.into_iter().collect(),
            snapshot: sh,
            message: message.trim_end().to_string(),
            intents: summarize(editor.intent_log()),
            created_at,
            author,
        };
        commit.validate().map_err(|e| invalid(e.to_string()))?;
        let bytes = commit.bytes();
        let hash = ContentHash::of(&bytes);
        let store = self.versions.entry(h).or_default();
        let generation = store
            .refs
            .generation
            .checked_add(1)
            .ok_or_else(|| branches::fail("RESOURCE_LIMIT", "generation overflow"))?;
        store.insert(hash, commit, bytes, snapshot)?;
        store.refs.generation = generation;
        store.merge = None;
        let name = store
            .refs
            .current_branch
            .clone()
            .unwrap_or_else(|| "main".into());
        store
            .refs
            .branches
            .entry(name.clone())
            .and_modify(|b| b.head = hash)
            .or_insert(alto_store::Branch {
                head: hash,
                protected: true,
            });
        store.refs.current_branch = Some(name);
        store.set_head(Some(hash));
        if let Some(editor) = self.documents.get_mut(&h) {
            editor.take_intent_log();
        }
        self.commit_view(h, hash)
    }

    fn snapshot_doc(&self, h: DocHandle, snapshot: &ContentHash) -> Result<Document, EngineError> {
        let bytes = self
            .versions(h)?
            .snapshots
            .get(snapshot)
            .ok_or_else(|| invalid(format!("NOT_FOUND: snapshot {snapshot}")))?;
        Ok(Document::from_snapshot_bytes(bytes)?)
    }

    pub fn commit_view(&self, h: DocHandle, hash: ContentHash) -> Result<CommitView, EngineError> {
        let c = self
            .versions(h)?
            .commits
            .get(&hash)
            .ok_or_else(|| invalid(format!("NOT_FOUND: commit {hash}")))?;
        let doc = self.snapshot_doc(h, &c.snapshot).ok();
        let current = self.document(h)?;
        let name = |id: &NodeId| {
            doc.as_ref()
                .and_then(|d| d.nodes.get(id))
                .or_else(|| current.nodes.get(id))
                .map(|n| n.name.clone())
        };
        Ok(CommitView {
            hash,
            parents: c.parents.clone(),
            snapshot: c.snapshot,
            message: c.message.clone(),
            created_at: c.created_at,
            author: c.author.clone(),
            intents: c
                .intents
                .iter()
                .map(|i| IntentView {
                    intent: i.intent.clone(),
                    label: i.label.clone(),
                    undone: i.undone,
                    count: i.count,
                    targets: i.targets.clone(),
                    target_names: i.targets.iter().map(name).collect(),
                })
                .collect(),
        })
    }

    /// History from the head along first parents, newest first.
    pub fn log(&self, h: DocHandle) -> Result<Vec<CommitView>, EngineError> {
        let store = self.versions(h)?;
        let mut out = vec![];
        let mut next = store.head;
        let mut seen = BTreeSet::new();
        while let Some(c) = next {
            if !seen.insert(c) {
                break;
            }
            let Some(commit) = store.commits.get(&c) else {
                break;
            };
            out.push(self.commit_view(h, c)?);
            next = commit.parents.first().copied();
        }
        Ok(out)
    }

    /// Reverse topological history: every child precedes its parents, with stable timestamp/hash ties.
    pub fn log_dag(
        &self,
        h: DocHandle,
        all_branches: bool,
    ) -> Result<Vec<CommitView>, EngineError> {
        let s = self.versions(h)?;
        s.validate_graph()?;
        let roots: Vec<_> = if all_branches {
            s.refs.roots().chain(s.head).collect()
        } else {
            s.head.into_iter().collect()
        };
        let mut keep = BTreeSet::new();
        for root in roots {
            keep.extend(s.ancestors(root)?);
        }
        let mut children: BTreeMap<_, usize> = keep.iter().map(|h| (*h, 0)).collect();
        for h in &keep {
            for p in &s.commits[h].parents {
                *children.get_mut(p).unwrap() += 1;
            }
        }
        let mut ready: BTreeSet<_> = children
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(h, _)| (std::cmp::Reverse(s.commits[h].created_at), *h))
            .collect();
        let mut out = vec![];
        while let Some((_, hash)) = ready.pop_first() {
            out.push(self.commit_view(h, hash)?);
            for p in &s.commits[&hash].parents {
                let n = children.get_mut(p).unwrap();
                *n -= 1;
                if *n == 0 {
                    ready.insert((std::cmp::Reverse(s.commits[p].created_at), *p));
                }
            }
        }
        Ok(out)
    }

    fn resolve(&self, h: DocHandle, r: VersionRef) -> Result<Document, EngineError> {
        match r {
            VersionRef::Working => Ok(self.document(h)?.clone()),
            VersionRef::Commit(c) => {
                let snapshot = self
                    .versions(h)?
                    .commits
                    .get(&c)
                    .ok_or_else(|| invalid(format!("NOT_FOUND: commit {c}")))?
                    .snapshot;
                self.snapshot_doc(h, &snapshot)
            }
        }
    }

    pub fn diff_versions(
        &self,
        h: DocHandle,
        from: VersionRef,
        to: VersionRef,
    ) -> Result<Vec<DiffItem>, EngineError> {
        Ok(alto_doc::diff(
            &self.resolve(h, from)?,
            &self.resolve(h, to)?,
        ))
    }

    /// Node names on both sides of a diff, for display.
    pub fn diff_names(
        &self,
        h: DocHandle,
        from: VersionRef,
        to: VersionRef,
    ) -> Result<BTreeMap<NodeId, String>, EngineError> {
        let mut names = BTreeMap::new();
        for r in [from, to] {
            for (id, n) in self.resolve(h, r)?.nodes {
                names.insert(id, n.name);
            }
        }
        Ok(names)
    }

    pub fn version_status(&self, h: DocHandle) -> Result<VersionStatus, EngineError> {
        let store = self.versions(h)?;
        let changes = match (store.head, &store.head_doc) {
            (Some(c), Some((cached, doc))) if *cached == c => {
                alto_doc::diff(doc, self.document(h)?).len()
            }
            (Some(c), _) => self
                .diff_versions(h, VersionRef::Commit(c), VersionRef::Working)?
                .len(),
            (None, _) => 1,
        };
        Ok(VersionStatus {
            head: store.head,
            refs: store.refs.clone(),
            commits: store.commits.len(),
            changed: store
                .head_digest
                .map(|d| self.state_digest(h).map(|current| d != current))
                .transpose()?
                .unwrap_or(true),
            changes,
            pending_intents: self.documents.get(&h).map_or(0, |e| e.intent_log().len()),
        })
    }

    /// Drops versions that cannot be reached from the head, then drops objects
    /// only those versions referenced. Commit and open do not call this.
    pub fn prune_versions(&mut self, h: DocHandle) -> Result<usize, EngineError> {
        self.document(h)?;
        let Some(store) = self.versions.get_mut(&h) else {
            return Ok(0);
        };
        store.validate_graph()?;
        let before = store.commits.len();
        store.gc_unreachable();
        let removed = before - store.commits.len();
        self.maintain();
        Ok(removed)
    }

    /// Restores a version as one undoable transaction. History is not rewritten.
    /// Returns the ops and the intent, which a journal record of the edit repeats.
    pub fn restore_version(
        &mut self,
        h: DocHandle,
        commit: ContentHash,
    ) -> Result<(Vec<alto_doc::Op>, alto_doc::Intent), EngineError> {
        let target = self.resolve(h, VersionRef::Commit(commit))?;
        self.require_objects(h, &ObjectTarget::Commit { hash: commit })?;
        let message = self.versions(h)?.commits[&commit].message.clone();
        let current = self.document(h)?;
        let ops = alto_doc::restore_ops(current, &target).map_err(|e| invalid(e.to_string()))?;
        let revision = current.revision;
        let label: String = message
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(40)
            .collect();
        let intent = alto_doc::Intent::labeled("version.restore", label);
        self.transact(h, intent.clone(), ops.clone(), revision)?;
        Ok((ops, intent))
    }

    /// Package contents including every committed snapshot and its objects.
    pub fn package_parts(&self, h: DocHandle) -> Result<PackageParts, EngineError> {
        Ok(self
            .package_parts_except(h, &BTreeSet::new(), BTreeSet::new())?
            .0)
    }

    /// Package parts without the bytes of objects in `stored`, plus every object the
    /// package references. A recovery checkpoint writes only the objects it lacks.
    pub fn package_parts_except(
        &self,
        h: DocHandle,
        stored: &BTreeSet<ContentHash>,
        extra: BTreeSet<ContentHash>,
    ) -> Result<(PackageParts, BTreeSet<ContentHash>), EngineError> {
        let doc = self.document(h)?;
        let store = self.versions(h)?;
        let mut hashes = extra;
        hashes.extend(doc_hashes(doc));
        hashes.extend(store.retained.iter().copied());
        let mut objects = BTreeMap::new();
        for &hash in hashes.difference(stored) {
            let bytes = self.encoded(hash)?.ok_or(EngineError::MissingTile(hash))?;
            objects.insert(hash, bytes);
        }
        let parts = PackageParts {
            snapshot: doc.snapshot_bytes()?,
            objects,
            snapshots: store.snapshots.clone(),
            commits: store.commit_bytes.clone(),
            head: store.head,
            refs: store.refs.clone(),
        };
        Ok((parts, hashes))
    }

    /// A recovery checkpoint as package entries without the objects in `stored`,
    /// plus `checkpoint.json` naming every object path it references and the
    /// opaque objects.
    pub fn recovery_checkpoint(
        &self,
        h: DocHandle,
        stored: &BTreeSet<ContentHash>,
    ) -> Result<RecoveryCheckpoint, EngineError> {
        // The undo history travels with a recovery checkpoint, and so do the objects
        // only it references. A saved .alto file has neither.
        let editor = self.documents.get(&h).ok_or(EngineError::NotFound(h))?;
        let history = editor.history();
        let mut history_objects = BTreeSet::new();
        for tx in history.undo.iter().chain(&history.redo) {
            crate::tx_hashes(tx, &mut history_objects);
        }
        let external = self.external_objects(h)?;
        let skip: BTreeSet<_> = stored.union(&external).copied().collect();
        let (parts, mut refs) = self.package_parts_except(h, &skip, history_objects)?;
        refs.retain(|hash| !external.contains(hash));
        let mut entries = alto_store::package_entries(&parts)?;
        if let Some(lazy) = self.lazy.get(&h) {
            entries.insert(
                "lazy-objects.json".into(),
                serde_json::to_vec(&lazy.known)
                    .map_err(|e| DocError::Serialization(e.to_string()))?,
            );
        }
        entries.insert(
            "history.json".into(),
            serde_json::to_vec(&history).map_err(|e| DocError::Serialization(e.to_string()))?,
        );
        let meta = serde_json::json!({
            "objects": refs.iter().map(alto_store::object_path).collect::<Vec<_>>(),
            "opaque": self.recovery_opaque_hashes(h)?,
        });
        entries.insert(
            "checkpoint.json".into(),
            serde_json::to_vec(&meta).map_err(|e| DocError::Serialization(e.to_string()))?,
        );
        Ok(RecoveryCheckpoint {
            entries,
            written: parts.objects.into_keys().collect(),
            external,
        })
    }

    pub fn recovery_opaque_hashes(&self, h: DocHandle) -> Result<Vec<ContentHash>, EngineError> {
        let doc = self.document(h)?;
        let mut hashes = doc_hashes(doc);
        hashes.extend(self.versions(h)?.retained.iter().copied());
        Ok(hashes
            .into_iter()
            .filter(|hash| self.tiles.is_opaque(hash))
            .collect())
    }

    pub fn folder_entries(&self, h: DocHandle) -> Result<BTreeMap<String, Vec<u8>>, EngineError> {
        Ok(alto_store::folder_entries(&self.package_parts(h)?)?)
    }

    /// A new package whose history roots are the head and `ancestors` parents.
    /// The open document's commits are left unchanged.
    pub fn save_package_trimmed(
        &self,
        h: DocHandle,
        ancestors: u32,
    ) -> Result<Vec<u8>, EngineError> {
        let _ = (h, ancestors);
        Err(branches::fail(
            "UNSUPPORTED",
            "history truncation is not supported; use reference-based GC",
        ))
    }

    /// Object paths a folder needs, given its manifest, snapshots and commits.
    pub fn folder_plan(entries: BTreeMap<String, Vec<u8>>) -> Result<Vec<String>, EngineError> {
        let p = alto_store::decode_entries(entries)?;
        let mut hashes = doc_hashes(&Document::from_snapshot_bytes(&p.snapshot)?);
        for s in p.snapshots.values() {
            if let Ok(d) = Document::from_snapshot_bytes(s) {
                hashes.extend(doc_hashes(&d));
            }
        }
        Ok(hashes.iter().map(alto_store::object_path).collect())
    }

    /// Opens either package form from its logical entries.
    pub fn open_entries(
        &mut self,
        entries: BTreeMap<String, Vec<u8>>,
    ) -> Result<(DocHandle, Vec<Warning>), EngineError> {
        let p = alto_store::decode_entries(entries)?;
        self.open_decoded_package(p, false, None, None)
    }

    /// Journal-only fast path. The caller must verify the entire checkpoint's
    /// digest before using the persisted opaque metadata.
    pub fn open_recovery_package(
        &mut self,
        bytes: &[u8],
        opaque: &std::collections::BTreeSet<ContentHash>,
    ) -> Result<(DocHandle, Vec<Warning>), EngineError> {
        self.open_recovery_entries(alto_store::read_entries(bytes)?, opaque)
    }

    /// Journal-only: entries of a checkpoint whose metadata digest the caller verified.
    /// Objects are named by their hash and checked against it on first use.
    pub fn open_recovery_entries(
        &mut self,
        mut entries: BTreeMap<String, Vec<u8>>,
        opaque: &std::collections::BTreeSet<ContentHash>,
    ) -> Result<(DocHandle, Vec<Warning>), EngineError> {
        let history = match entries.remove("history.json") {
            Some(bytes) => Some(
                serde_json::from_slice::<alto_doc::History>(&bytes)
                    .map_err(|e| alto_store::StoreError::Corrupt(format!("history: {e}")))?,
            ),
            None => None,
        };
        let lazy = match entries.remove("lazy-objects.json") {
            Some(bytes) => Some(
                serde_json::from_slice::<BTreeSet<ContentHash>>(&bytes)
                    .map_err(|e| alto_store::StoreError::Corrupt(format!("lazy objects: {e}")))?,
            ),
            None => None,
        };
        let p = alto_store::decode_recovery_entries(entries, opaque)?;
        self.open_decoded_package(p, true, history, lazy)
    }

    fn open_decoded_package(
        &mut self,
        p: alto_store::Package,
        deferred_verify: bool,
        history: Option<alto_doc::History>,
        lazy: Option<BTreeSet<ContentHash>>,
    ) -> Result<(DocHandle, Vec<Warning>), EngineError> {
        let current_doc = Document::from_snapshot_bytes(&p.snapshot)?;
        let current = ContentHash::of(&p.snapshot);
        let mut parsed = BTreeMap::new();
        for (hash, bytes) in &p.commits {
            parsed.insert(
                *hash,
                Commit::parse(bytes).map_err(|e| branches::fail("CORRUPT_DATA", e))?,
            );
        }
        let mut required = BTreeSet::new();
        let mut stack: Vec<_> = p.refs.roots().chain(p.head).collect();
        while let Some(hash) = stack.pop() {
            if required.insert(hash) {
                let c = parsed.get(&hash).ok_or_else(|| {
                    branches::fail("CORRUPT_DATA", "referenced commit is missing")
                })?;
                stack.extend(c.parents.iter().copied());
            }
        }
        let mut store = VersionStore::default();
        for (hash, commit) in parsed {
            let snapshot = if commit.snapshot == current {
                Some(&p.snapshot)
            } else {
                p.snapshots.get(&commit.snapshot)
            };
            let Some(snapshot) = snapshot else {
                if required.contains(&hash) {
                    return Err(branches::fail(
                        "CORRUPT_DATA",
                        "referenced commit snapshot is missing",
                    ));
                }
                continue; // An interrupted GC can leave unreferenced fragments.
            };
            let doc = Document::from_snapshot_bytes(snapshot)?;
            if doc.root != current_doc.root {
                return Err(branches::fail(
                    "CORRUPT_DATA",
                    "history belongs to another document",
                ));
            }
            store.insert(hash, commit, p.commits[&hash].clone(), snapshot.clone())?;
        }
        loop {
            let incomplete: Vec<_> = store
                .commits
                .iter()
                .filter(|(_, c)| c.parents.iter().any(|p| !store.commits.contains_key(p)))
                .map(|(h, _)| *h)
                .collect();
            if incomplete.is_empty() {
                break;
            }
            for hash in incomplete {
                if required.contains(&hash) {
                    return Err(branches::fail(
                        "CORRUPT_DATA",
                        "referenced parent is missing",
                    ));
                }
                store.commits.remove(&hash);
                store.commit_bytes.remove(&hash);
            }
        }
        let needed: BTreeSet<_> = store.commits.values().map(|c| c.snapshot).collect();
        store.snapshots.retain(|h, _| needed.contains(h));
        store.retained.clear();
        for bytes in store.snapshots.values() {
            store
                .retained
                .extend(doc_hashes(&Document::from_snapshot_bytes(bytes)?));
        }
        store.refs = p.refs;
        store.set_head(p.head);
        store.validate_graph()?;
        // A recovery replays history records that may need what the checkpoint carries.
        let carried: BTreeSet<ContentHash> = if deferred_verify {
            p.objects.keys().copied().collect()
        } else {
            BTreeSet::new()
        };
        let (handle, mut warnings) =
            self.install_package(p.objects, p.opaque, &p.snapshot, vec![], deferred_verify)?;
        for hash in &store.retained {
            if !self.tiles.contains(hash)
                && !self.blobs.contains_key(hash)
                && !lazy.as_ref().is_some_and(|known| known.contains(hash))
            {
                self.documents.remove(&handle);
                return Err(branches::fail(
                    "CORRUPT_DATA",
                    format!("history object missing: {hash}"),
                ));
            }
        }
        {
            let validation = (|| -> Result<(), EngineError> {
                if let Some(selection) = &self.document(handle)?.selection {
                    self.validate_mask_plane_impl(selection, deferred_verify)?;
                }
                for node in self.document(handle)?.nodes.values() {
                    self.validate_node_content_impl(node, deferred_verify)?;
                }
                // With objects left in the folder, a version is checked when they are read.
                let versions = store.snapshots.values().filter(|_| lazy.is_none());
                for bytes in versions {
                    let doc = Document::from_snapshot_bytes(bytes)?;
                    if let Some(selection) = &doc.selection {
                        self.validate_mask_plane_impl(selection, deferred_verify)?;
                    }
                    for node in doc.nodes.values() {
                        self.validate_node_content_impl(node, deferred_verify)?;
                    }
                }
                Ok(())
            })();
            if let Err(error) = validation {
                self.documents.remove(&handle);
                return Err(error);
            }
        }
        warnings.dedup();
        self.versions.insert(handle, store);
        if let Some(known) = lazy {
            self.lazy.insert(
                handle,
                LazyObjects {
                    known,
                    pinned: carried,
                },
            );
        }
        // Before maintain(): the objects only the history references are kept for it.
        if let Some(history) = history
            && let Some(editor) = self.documents.get_mut(&handle)
        {
            editor.restore_history(history);
        }
        self.backed = true;
        self.maintain();
        Ok((handle, warnings))
    }
}
