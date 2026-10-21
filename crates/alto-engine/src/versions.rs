//! M8: version commits, history, semantic diff, restore, and the package parts that
//! carry history in both the single-file and folder forms.
use crate::{DocHandle, Engine, EngineError, node_hashes};
use alto_core::{ContentHash, NodeId};
use alto_doc::{Commit, DiffItem, DocError, Document, version::summarize};
use alto_store::PackageParts;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Clone)]
pub struct VersionStore {
    pub(crate) commits: BTreeMap<ContentHash, Commit>,
    pub(crate) commit_bytes: BTreeMap<ContentHash, Vec<u8>>,
    pub(crate) snapshots: BTreeMap<ContentHash, Vec<u8>>,
    pub(crate) head: Option<ContentHash>,
    /// Tiles and blobs referenced by committed snapshots (GC roots).
    pub(crate) retained: BTreeSet<ContentHash>,
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
    pub count: u32,
    pub targets: Vec<NodeId>,
    pub target_names: Vec<String>,
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
    /// A linear history does not shrink. Commits left behind by a rewritten
    /// parent link do.
    pub(crate) fn gc_unreachable(&mut self) {
        let mut keep = BTreeSet::new();
        let mut stack: Vec<ContentHash> = self.head.into_iter().collect();
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

fn kept_commits(store: &VersionStore, ancestors: u32) -> BTreeSet<ContentHash> {
    let Some(mut cursor) = store.head else {
        return BTreeSet::new();
    };
    let mut kept = BTreeSet::new();
    kept.insert(cursor);
    for _ in 0..ancestors {
        let Some(parent) = store
            .commits
            .get(&cursor)
            .and_then(|c| c.parents.first())
            .copied()
        else {
            break;
        };
        if !kept.insert(parent) {
            break;
        }
        cursor = parent;
    }
    kept
}

fn invalid(s: impl Into<String>) -> EngineError {
    DocError::Invalid(s.into()).into()
}

impl Engine {
    fn versions(&self, h: DocHandle) -> Result<&VersionStore, EngineError> {
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
        let snapshot = self.snapshot(h)?;
        let sh = ContentHash::of(&snapshot);
        let store = self.versions.entry(h).or_default();
        let head = store.head;
        if !allow_empty
            && head
                .and_then(|c| store.commits.get(&c))
                .is_some_and(|c| c.snapshot == sh)
        {
            return Err(invalid("INVALID_ARGUMENT: 前の版から変更がありません"));
        }
        let editor = self.documents.get(&h).ok_or(EngineError::NotFound(h))?;
        let commit = Commit {
            parents: head.into_iter().collect(),
            snapshot: sh,
            message: message.trim_end().to_string(),
            intents: summarize(editor.intent_log()),
            created_at,
            author,
        };
        commit
            .validate()
            .map_err(|e| invalid(format!("INVALID_ARGUMENT: {e}")))?;
        let bytes = commit.bytes();
        let hash = ContentHash::of(&bytes);
        let store = self.versions.entry(h).or_default();
        store.insert(hash, commit, bytes, snapshot)?;
        store.head = Some(hash);
        store.gc_unreachable();
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
                .map_or_else(|| "（削除済み）".to_string(), |n| n.name.clone())
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
        let changes = match store.head {
            Some(c) => self
                .diff_versions(h, VersionRef::Commit(c), VersionRef::Working)?
                .len(),
            None => usize::from(self.document(h)?.nodes.len() > 1),
        };
        Ok(VersionStatus {
            head: store.head,
            commits: store.commits.len(),
            changed: changes > 0,
            changes,
            pending_intents: self.documents.get(&h).map_or(0, |e| e.intent_log().len()),
        })
    }

    /// Restores a version as one undoable transaction. History is not rewritten.
    pub fn restore_version(
        &mut self,
        h: DocHandle,
        commit: ContentHash,
    ) -> Result<Vec<alto_doc::Op>, EngineError> {
        let target = self.resolve(h, VersionRef::Commit(commit))?;
        let message = self.versions(h)?.commits[&commit].message.clone();
        let current = self.document(h)?;
        let ops = alto_doc::restore_ops(current, &target)
            .map_err(|e| invalid(format!("INVALID_ARGUMENT: {e}")))?;
        let revision = current.revision;
        let label: String = message
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(40)
            .collect();
        self.transact(h, format!("版を復元: {label}"), ops.clone(), revision)?;
        Ok(ops)
    }

    /// Package contents including every committed snapshot and its objects.
    pub fn package_parts(&self, h: DocHandle) -> Result<PackageParts, EngineError> {
        let doc = self.document(h)?;
        let store = self.versions(h)?;
        let mut hashes = doc_hashes(doc);
        hashes.extend(store.retained.iter().copied());
        let objects = hashes
            .into_iter()
            .filter_map(|hash| self.encoded(hash).map(|b| (hash, b)))
            .collect();
        Ok(PackageParts {
            snapshot: doc.snapshot_bytes()?,
            objects,
            snapshots: store.snapshots.clone(),
            commits: store.commit_bytes.clone(),
            head: store.head,
        })
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
        let doc = self.document(h)?;
        let current = doc.snapshot_bytes()?;
        let current_hash = ContentHash::of(&current);
        let store = self.versions(h)?;
        let kept = kept_commits(store, ancestors);
        let mut hashes = doc_hashes(doc);
        let mut snapshots = BTreeMap::new();
        let mut commits = BTreeMap::new();
        for hash in &kept {
            let Some(commit) = store.commits.get(hash) else {
                continue;
            };
            if let Some(bytes) = store.snapshots.get(&commit.snapshot) {
                hashes.extend(doc_hashes(&Document::from_snapshot_bytes(bytes)?));
                if commit.snapshot != current_hash {
                    snapshots.insert(commit.snapshot, bytes.clone());
                }
            }
            if let Some(bytes) = store.commit_bytes.get(hash) {
                commits.insert(*hash, bytes.clone());
            }
        }
        let objects = hashes
            .into_iter()
            .filter_map(|hash| self.encoded(hash).map(|b| (hash, b)))
            .collect();
        Ok(alto_store::encode_parts(&PackageParts {
            snapshot: current,
            objects,
            snapshots,
            commits,
            head: store.head.filter(|h| kept.contains(h)),
        })?)
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
    ) -> Result<(DocHandle, Vec<String>), EngineError> {
        let p = alto_store::decode_entries(entries)?;
        let (handle, mut warnings) =
            self.install_package(p.objects, p.opaque, &p.snapshot, p.warnings)?;
        let current = ContentHash::of(&p.snapshot);
        let mut store = VersionStore::default();
        for (hash, bytes) in &p.commits {
            let commit = match Commit::parse_history(bytes) {
                Ok((c, warning)) => {
                    if let Some(w) = warning {
                        warnings.push(format!("commit {hash}: {w}"));
                    }
                    c
                }
                Err(e) => {
                    warnings.push(format!("damaged commit {hash}: {e}"));
                    continue;
                }
            };
            let snapshot = if commit.snapshot == current {
                Some(p.snapshot.clone())
            } else {
                p.snapshots.get(&commit.snapshot).cloned()
            };
            match snapshot {
                Some(s) => {
                    if let Err(e) = store.insert(*hash, commit, bytes.clone(), s) {
                        warnings.push(format!("damaged version {hash}: {e}"));
                    }
                }
                None => warnings.push(format!("snapshot missing for commit {hash}")),
            }
        }
        store.head = p.head.filter(|h| store.commits.contains_key(h));
        store.gc_unreachable();
        for hash in &store.retained {
            if !self.tiles.contains(hash) && !self.blobs.contains_key(hash) {
                warnings.push(format!("missing object {hash} in version history"));
            }
        }
        warnings.dedup();
        self.versions.insert(handle, store);
        self.backed = true;
        self.maintain();
        Ok((handle, warnings))
    }
}
