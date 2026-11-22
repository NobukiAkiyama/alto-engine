use super::*;
use alto_store::Branch;
use unicode_normalization::UnicodeNormalization;

pub(super) fn fail(code: &'static str, message: impl Into<String>) -> EngineError {
    EngineError::Operation {
        code,
        message: message.into(),
    }
}
pub(super) fn ref_name(name: &str) -> Result<String, EngineError> {
    let name: String = name.nfc().collect();
    if name.is_empty()
        || name.len() > 128
        || name.starts_with('-')
        || name.ends_with('.')
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("@{")
        || name
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "/\\~^:?*[".contains(c))
    {
        return Err(fail("INVALID_ARGUMENT", "invalid reference name"));
    }
    Ok(name)
}
#[derive(Clone, Debug, Serialize)]
pub struct BranchView {
    pub name: String,
    pub head: ContentHash,
    pub current: bool,
    pub protected: bool,
    pub ahead: usize,
    pub behind: usize,
}
impl VersionStore {
    pub(super) fn ancestors(
        &self,
        head: ContentHash,
    ) -> Result<BTreeSet<ContentHash>, EngineError> {
        let mut keep = BTreeSet::new();
        let mut stack = vec![head];
        while let Some(h) = stack.pop() {
            if keep.insert(h) {
                let c = self
                    .commits
                    .get(&h)
                    .ok_or_else(|| fail("CORRUPT_DATA", format!("missing commit {h}")))?;
                stack.extend(c.parents.iter().copied());
            }
        }
        Ok(keep)
    }
    pub(super) fn bump(&mut self) -> Result<(), EngineError> {
        self.refs.generation = self
            .refs
            .generation
            .checked_add(1)
            .ok_or_else(|| fail("RESOURCE_LIMIT", "reference generation overflow"))?;
        self.merge = None;
        Ok(())
    }
    pub(super) fn validate_graph(&self) -> Result<(), EngineError> {
        self.refs.validate(self.head)?;
        // Kahn's algorithm validates every stored commit, including unreachable ones.
        let mut indegree: BTreeMap<_, usize> = self
            .commits
            .iter()
            .map(|(h, c)| (*h, c.parents.len()))
            .collect();
        let mut children: BTreeMap<ContentHash, Vec<ContentHash>> = BTreeMap::new();
        for (h, c) in &self.commits {
            for p in &c.parents {
                if !self.commits.contains_key(p) {
                    return Err(fail("CORRUPT_DATA", "missing commit parent"));
                }
                children.entry(*p).or_default().push(*h);
            }
        }
        let mut stack: Vec<_> = indegree
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(h, _)| *h)
            .collect();
        let mut visited = 0;
        while let Some(h) = stack.pop() {
            visited += 1;
            for child in children.get(&h).into_iter().flatten() {
                let n = indegree.get_mut(child).unwrap();
                *n -= 1;
                if *n == 0 {
                    stack.push(*child);
                }
            }
        }
        if visited != self.commits.len() {
            return Err(fail("CORRUPT_DATA", "cyclic commit history"));
        }
        for h in self.refs.all_roots().chain(self.head) {
            self.ancestors(h)?;
        }
        for entry in &self.refs.stashes {
            let commit = &self.commits[&entry.commit];
            if commit.parents.len() != 1
                || commit.message != entry.message
                || commit.created_at != entry.created_at
            {
                return Err(fail(
                    "CORRUPT_DATA",
                    "stash entry does not match its commit",
                ));
            }
        }
        for c in self.commits.values() {
            if let Some(base) = c.merge_base
                && (!self.ancestors(c.parents[0])?.contains(&base)
                    || !self.ancestors(c.parents[1])?.contains(&base))
            {
                return Err(fail("CORRUPT_DATA", "invalid merge base"));
            }
        }
        for (name, _) in self
            .refs
            .branches
            .iter()
            .map(|(k, v)| (k, v.head))
            .chain(self.refs.tags.iter().map(|(k, v)| (k, *v)))
        {
            if ref_name(name)? != *name {
                return Err(fail("CORRUPT_DATA", "noncanonical reference name"));
            }
        }
        Ok(())
    }
}
impl Engine {
    pub fn version_refs(&self, h: DocHandle) -> Result<&alto_store::VersionRefs, EngineError> {
        Ok(&self.versions(h)?.refs)
    }
    pub fn versions_digest(&self, h: DocHandle) -> Result<ContentHash, EngineError> {
        let s = self.versions(h)?;
        Ok(ContentHash::of(
            &serde_json::to_vec(&(s.head, &s.refs)).map_err(|e| invalid(e.to_string()))?,
        ))
    }
    pub(super) fn clean_head(&self, h: DocHandle) -> Result<ContentHash, EngineError> {
        let store = self.versions(h)?;
        let head = store
            .head
            .ok_or_else(|| fail("NO_COMMIT", "record the first version"))?;
        if store.head_digest != Some(self.state_digest(h)?) {
            return Err(fail("CONFLICT", "uncommitted working changes"));
        }
        Ok(head)
    }
    pub fn create_branch(
        &mut self,
        h: DocHandle,
        name: &str,
        from: Option<ContentHash>,
        checkout: bool,
    ) -> Result<(), EngineError> {
        let name = ref_name(name)?;
        let store = self.versions(h)?;
        if store.refs.branches.contains_key(&name) {
            return Err(fail("CONFLICT", "branch already exists"));
        }
        let from = from
            .or(store.head)
            .ok_or_else(|| fail("NO_COMMIT", "record the first version"))?;
        let target = self.resolve(h, VersionRef::Commit(from))?;
        if checkout {
            self.clean_head(h)?;
            self.require_objects(h, &super::lazy::ObjectTarget::Commit { hash: from })?;
        }
        let revision = self.document(h)?.revision;
        if checkout {
            revision
                .checked_add(1)
                .ok_or_else(|| fail("RESOURCE_LIMIT", "revision overflow"))?;
        }
        let store = self.versions.entry(h).or_default();
        store.bump()?;
        store.refs.branches.insert(
            name.clone(),
            Branch {
                head: from,
                protected: false,
            },
        );
        if checkout {
            store.refs.current_branch = Some(name);
            store.set_head(Some(from));
            self.install_version_document(h, target, revision)?;
        }
        Ok(())
    }
    pub(super) fn install_version_document(
        &mut self,
        h: DocHandle,
        mut doc: Document,
        revision: u64,
    ) -> Result<(), EngineError> {
        doc.revision = revision
            .checked_add(1)
            .ok_or_else(|| fail("RESOURCE_LIMIT", "revision overflow"))?;
        self.install_editor(h, alto_doc::Editor::new(doc));
        self.maintain();
        Ok(())
    }
    pub fn checkout(&mut self, h: DocHandle, name: &str) -> Result<(), EngineError> {
        // Both the object check and the lookup below use the stored spelling.
        let name = ref_name(name)?;
        self.require_objects(h, &super::lazy::ObjectTarget::Branch { name: name.clone() })?;
        self.clean_head(h)?;
        let store = self.versions(h)?;
        let head = store
            .refs
            .branches
            .get(&name)
            .ok_or_else(|| fail("NOT_FOUND", "branch"))?
            .head;
        if store.refs.current_branch.as_ref() == Some(&name) {
            return Ok(());
        }
        self.switch_version(h, head, Some(name))
    }
    pub fn detach(&mut self, h: DocHandle, head: ContentHash) -> Result<(), EngineError> {
        self.require_objects(h, &super::lazy::ObjectTarget::Commit { hash: head })?;
        self.clean_head(h)?;
        self.switch_version(h, head, None)
    }
    fn switch_version(
        &mut self,
        h: DocHandle,
        head: ContentHash,
        branch: Option<String>,
    ) -> Result<(), EngineError> {
        let target = self.resolve(h, VersionRef::Commit(head))?;
        let revision = self.document(h)?.revision;
        revision
            .checked_add(1)
            .ok_or_else(|| fail("RESOURCE_LIMIT", "revision overflow"))?;
        let store = self.versions.entry(h).or_default();
        store.bump()?;
        store.refs.current_branch = branch;
        store.set_head(Some(head));
        self.install_version_document(h, target, revision)
    }
    pub fn delete_branch(
        &mut self,
        h: DocHandle,
        name: &str,
        force: bool,
    ) -> Result<(), EngineError> {
        let name = ref_name(name)?;
        let s = self.versions(h)?;
        let branch = s
            .refs
            .branches
            .get(&name)
            .ok_or_else(|| fail("NOT_FOUND", "branch"))?;
        if branch.protected
            || s.refs.current_branch.as_ref() == Some(&name)
            || s.refs.branches.len() == 1
        {
            return Err(fail("CONFLICT", "current, last, or protected branch"));
        }
        if !force
            && (s.refs.current_branch.is_none()
                || !s.ancestors(s.head.unwrap())?.contains(&branch.head))
        {
            return Err(fail("BRANCH_NOT_MERGED", "branch has unmerged versions"));
        }
        let s = self.versions.get_mut(&h).unwrap();
        s.bump()?;
        s.refs.branches.remove(&name);
        Ok(())
    }
    pub fn protect_branch(
        &mut self,
        h: DocHandle,
        name: &str,
        protected: bool,
    ) -> Result<(), EngineError> {
        let name = ref_name(name)?;
        self.versions(h)?
            .refs
            .branches
            .get(&name)
            .ok_or_else(|| fail("NOT_FOUND", "branch"))?;
        let s = self.versions.get_mut(&h).unwrap();
        s.bump()?;
        s.refs.branches.get_mut(&name).unwrap().protected = protected;
        Ok(())
    }
    pub fn set_tag(
        &mut self,
        h: DocHandle,
        name: &str,
        target: Option<ContentHash>,
    ) -> Result<(), EngineError> {
        let name = ref_name(name)?;
        let s = self.versions(h)?;
        if let Some(hash) = target {
            if !s.commits.contains_key(&hash) {
                return Err(fail("NOT_FOUND", "commit"));
            }
            if s.refs.tags.contains_key(&name) {
                return Err(fail("CONFLICT", "tag exists"));
            }
        } else if !s.refs.tags.contains_key(&name) {
            return Err(fail("NOT_FOUND", "tag"));
        }
        let s = self.versions.get_mut(&h).unwrap();
        s.bump()?;
        if let Some(hash) = target {
            s.refs.tags.insert(name, hash);
        } else {
            s.refs.tags.remove(&name);
        }
        Ok(())
    }
    pub fn pin_version(
        &mut self,
        h: DocHandle,
        hash: ContentHash,
        pin: bool,
    ) -> Result<(), EngineError> {
        if !self.versions(h)?.commits.contains_key(&hash) {
            return Err(fail("NOT_FOUND", "commit"));
        }
        let s = self.versions.get_mut(&h).unwrap();
        s.bump()?;
        if pin {
            s.refs.pinned.insert(hash);
        } else {
            s.refs.pinned.remove(&hash);
        }
        Ok(())
    }
    /// Puts the working document aside as a private commit and returns the head
    /// to its committed contents. Works in detached state: no branch moves.
    pub fn stash(
        &mut self,
        h: DocHandle,
        message: &str,
        created_at: i64,
        author: Option<String>,
    ) -> Result<alto_store::StashEntry, EngineError> {
        self.document(h)?;
        let head = self
            .versions(h)?
            .head
            .ok_or_else(|| fail("NO_COMMIT", "record the first version"))?;
        let snapshot = self.snapshot(h)?;
        let snapshot_hash = ContentHash::of(&snapshot);
        if self.versions(h)?.commits[&head].snapshot == snapshot_hash {
            return Err(fail("INVALID_ARGUMENT", "nothing to stash"));
        }
        let editor = self.documents.get(&h).ok_or(EngineError::NotFound(h))?;
        let commit = Commit {
            format: 2,
            merge_base: None,
            parents: vec![head],
            snapshot: snapshot_hash,
            message: message.trim_end().to_string(),
            intents: summarize(editor.intent_log()),
            created_at,
            author,
        };
        commit.validate().map_err(|e| invalid(e.to_string()))?;
        let bytes = commit.bytes();
        let hash = ContentHash::of(&bytes);
        let stashes = &self.versions(h)?.refs.stashes;
        if stashes.len() >= MAX_STASHES && !stashes.iter().any(|entry| entry.commit == hash) {
            return Err(fail("RESOURCE_LIMIT", "too many stashes"));
        }
        self.require_objects(h, &super::lazy::ObjectTarget::Commit { hash: head })?;
        let target = self.resolve(h, VersionRef::Commit(head))?;
        let revision = self.document(h)?.revision;
        revision
            .checked_add(1)
            .ok_or_else(|| fail("RESOURCE_LIMIT", "revision overflow"))?;
        let entry = alto_store::StashEntry {
            commit: hash,
            message: commit.message.clone(),
            created_at,
        };
        let store = self.versions.entry(h).or_default();
        let generation = store
            .refs
            .generation
            .checked_add(1)
            .ok_or_else(|| fail("RESOURCE_LIMIT", "generation overflow"))?;
        store.insert(hash, commit, bytes, snapshot)?;
        store.refs.generation = generation;
        store.refs.stashes.retain(|other| other.commit != hash);
        store.refs.stashes.insert(0, entry.clone());
        store.merge = None;
        self.install_version_document(h, target, revision)?;
        Ok(entry)
    }
    /// Forgets a stash. Its commit and objects stay until the next GC.
    pub fn stash_drop(&mut self, h: DocHandle, commit: ContentHash) -> Result<(), EngineError> {
        self.document(h)?;
        if !self
            .versions(h)?
            .refs
            .stashes
            .iter()
            .any(|entry| entry.commit == commit)
        {
            return Err(fail("NOT_FOUND", "stash"));
        }
        let s = self.versions.get_mut(&h).unwrap();
        s.bump()?;
        s.refs.stashes.retain(|entry| entry.commit != commit);
        Ok(())
    }
    pub fn list_branches(&self, h: DocHandle) -> Result<Vec<BranchView>, EngineError> {
        let s = self.versions(h)?;
        let ours = s
            .head
            .map(|h| s.ancestors(h))
            .transpose()?
            .unwrap_or_default();
        s.refs
            .branches
            .iter()
            .map(|(name, b)| {
                let theirs = s.ancestors(b.head)?;
                Ok(BranchView {
                    name: name.clone(),
                    head: b.head,
                    current: s.refs.current_branch.as_ref() == Some(name),
                    protected: b.protected,
                    ahead: theirs.difference(&ours).count(),
                    behind: ours.difference(&theirs).count(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alto_doc::{DocumentMeta, Node, Op, PropPatch};
    fn setup() -> (Engine, DocHandle, NodeId, ContentHash) {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(32, 32)).unwrap();
        let n = Node::raster("layer");
        let id = n.id;
        let root = e.document(h).unwrap().root;
        e.transact(
            h,
            "add",
            vec![Op::InsertNode {
                node: n,
                parent: root,
                index: 0,
            }],
            0,
        )
        .unwrap();
        let first = e.commit(h, "base", 1, None, false).unwrap().hash;
        (e, h, id, first)
    }
    fn change(e: &mut Engine, h: DocHandle, id: NodeId, patch: PropPatch) {
        let r = e.document(h).unwrap().revision;
        e.transact(h, "edit", vec![Op::SetProps { id, patch }], r)
            .unwrap();
    }
    #[test]
    fn branch_switch_dirty_protection_and_archive_roundtrip() {
        let (mut e, h, id, first) = setup();
        e.create_branch(h, "案A", None, true).unwrap();
        change(
            &mut e,
            h,
            id,
            PropPatch {
                name: Some("A".into()),
                ..Default::default()
            },
        );
        assert!(
            e.checkout(h, "main")
                .unwrap_err()
                .to_string()
                .starts_with("CONFLICT:")
        );
        let tip = e.commit(h, "A", 2, None, false).unwrap().hash;
        e.checkout(h, "main").unwrap();
        assert_eq!(e.document(h).unwrap().node(id).unwrap().name, "layer");
        assert_eq!(e.history_state(h).unwrap(), (0, 0));
        assert!(
            e.delete_branch(h, "案A", false)
                .unwrap_err()
                .to_string()
                .starts_with("BRANCH_NOT_MERGED:")
        );
        assert!(e.delete_branch(h, "main", true).is_err());
        e.set_tag(h, "kept", Some(tip)).unwrap();
        e.delete_branch(h, "案A", true).unwrap();
        assert_eq!(e.prune_versions(h).unwrap(), 0);
        let data = e.save_package(h).unwrap();
        let mut reopened = Engine::new();
        let (d, w) = reopened.open_package(&data).unwrap();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(reopened.version_status(d).unwrap().head, Some(first));
        assert_eq!(reopened.version_refs(d).unwrap().tags["kept"], tip);
        reopened.set_tag(d, "kept", None).unwrap();
        assert_eq!(reopened.prune_versions(d).unwrap(), 1);
    }
    #[test]
    fn merge_disjoint_properties_conflicts_stale_and_two_parents() {
        let (mut e, h, id, base) = setup();
        e.create_branch(h, "feature", None, true).unwrap();
        change(
            &mut e,
            h,
            id,
            PropPatch {
                name: Some("feature".into()),
                ..Default::default()
            },
        );
        let theirs = e.commit(h, "feature", 2, None, false).unwrap().hash;
        e.checkout(h, "main").unwrap();
        change(
            &mut e,
            h,
            id,
            PropPatch {
                opacity: Some(0.5),
                ..Default::default()
            },
        );
        let ours = e.commit(h, "main", 3, None, false).unwrap().hash;
        let plan = e.prepare_merge(h, "feature").unwrap();
        assert!(plan.conflicts.is_empty());
        assert_eq!(plan.base, Some(base));
        let result = e
            .finish_merge(h, &plan.token, vec![], "merge", 4, None)
            .unwrap();
        assert_eq!(
            e.commit_view(h, result.head).unwrap().parents,
            vec![ours, theirs]
        );
        let n = e.document(h).unwrap().node(id).unwrap();
        assert_eq!(n.name, "feature");
        assert_eq!(n.props.opacity, 0.5);
        let bytes = e.save_package(h).unwrap();
        let (d, _) = e.open_package(&bytes).unwrap();
        assert_eq!(
            e.commit_view(d, result.head).unwrap().parents,
            vec![ours, theirs]
        );
        let plan = e.prepare_merge(h, "feature").unwrap();
        change(
            &mut e,
            h,
            id,
            PropPatch {
                name: Some("later".into()),
                ..Default::default()
            },
        );
        assert!(
            e.finish_merge(h, &plan.token, vec![], "stale", 5, None)
                .unwrap_err()
                .to_string()
                .starts_with("STALE_MERGE:")
        );
    }
    #[test]
    fn merge_of_format_5_and_6_writes_6() {
        let (mut e, h, id, _) = setup();
        e.create_branch(h, "hanging", None, true).unwrap();
        let node = Node::text(
            "punctuation",
            alto_text::TextContent {
                text: "あい。".into(),
                hanging: true,
                ..Default::default()
            },
        );
        let text = node.id;
        let doc = e.document(h).unwrap();
        e.transact(
            h,
            "layer.add",
            vec![Op::InsertNode {
                node,
                parent: doc.root,
                index: 0,
            }],
            doc.revision,
        )
        .unwrap();
        let theirs = e.commit(h, "format 6", 2, None, false).unwrap().hash;
        assert_eq!(e.document(h).unwrap().meta.format_version, 6);

        e.checkout(h, "main").unwrap();
        change(
            &mut e,
            h,
            id,
            PropPatch {
                opacity: Some(0.5),
                ..Default::default()
            },
        );
        let ours = e.commit(h, "format 5", 3, None, false).unwrap().hash;
        assert_eq!(e.document(h).unwrap().meta.format_version, 5);

        let plan = e.prepare_merge(h, "hanging").unwrap();
        assert!(plan.conflicts.is_empty());
        let result = e
            .finish_merge(h, &plan.token, vec![], "merge", 4, None)
            .unwrap();
        assert_eq!(
            e.commit_view(h, result.head).unwrap().parents,
            vec![ours, theirs]
        );
        assert!(!e.version_status(h).unwrap().changed);
        let bytes = e.save_package(h).unwrap();
        let entries = alto_store::read_entries(&bytes).unwrap();
        let manifest: alto_store::Manifest =
            serde_json::from_slice(&entries["manifest.json"]).unwrap();
        assert_eq!(manifest.document_format, 6);
        let (reopened, warnings) = e.open_package(&bytes).unwrap();
        assert!(warnings.is_empty());
        let doc = e.document(reopened).unwrap();
        assert_eq!(doc.meta.format_version, 6);
        assert_eq!(doc.node(id).unwrap().props.opacity, 0.5);
        assert!(
            matches!(&doc.node(text).unwrap().kind, alto_doc::NodeKind::Text { content, .. } if content.hanging)
        );
    }

    #[test]
    fn merge_conflict_is_read_only_until_all_resolved() {
        let (mut e, h, id, _) = setup();
        e.create_branch(h, "feature", None, true).unwrap();
        change(
            &mut e,
            h,
            id,
            PropPatch {
                name: Some("theirs".into()),
                ..Default::default()
            },
        );
        e.commit(h, "f", 2, None, false).unwrap();
        e.checkout(h, "main").unwrap();
        change(
            &mut e,
            h,
            id,
            PropPatch {
                name: Some("ours".into()),
                ..Default::default()
            },
        );
        e.commit(h, "m", 3, None, false).unwrap();
        let before = e.snapshot(h).unwrap();
        let plan = e.prepare_merge(h, "feature").unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert!(
            e.finish_merge(h, &plan.token, vec![], "bad", 4, None)
                .is_err()
        );
        assert_eq!(e.snapshot(h).unwrap(), before);
        let r = super::super::MergeResolution {
            conflict_id: plan.conflicts[0].id.clone(),
            choice: "theirs".into(),
            value: None,
        };
        e.finish_merge(h, &plan.token, vec![r], "merge", 4, None)
            .unwrap();
        assert_eq!(e.document(h).unwrap().node(id).unwrap().name, "theirs");
    }
    #[test]
    fn normalized_names_and_detached_commit() {
        let (mut e, h, _, base) = setup();
        e.create_branch(h, "か\u{3099}", None, false).unwrap();
        assert!(e.create_branch(h, "が", None, false).is_err());
        for name in ["..", "a/b", "-bad", "a.lock", "space name", "a\\b"] {
            assert!(e.create_branch(h, name, None, false).is_err());
        }
        e.detach(h, base).unwrap();
        assert!(e.commit(h, "detached", 2, None, true).is_err());
    }
}

#[cfg(test)]
mod pixel_tests {
    use super::*;
    use alto_core::{PixelFormat, TileKey};
    use alto_doc::{DocumentMeta, ImageBounds, Node, Op, TileChange};
    use alto_raster::{TileData, TileMap};
    fn setup() -> (Engine, DocHandle, NodeId) {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(512, 256)).unwrap();
        let mut n = Node::raster("pixels");
        let id = n.id;
        n.kind = alto_doc::NodeKind::Raster {
            bounds: ImageBounds {
                x: 0,
                y: 0,
                width: 512,
                height: 256,
            },
            tiles: TileMap::new(PixelFormat::Rgba8),
        };
        let parent = e.document(h).unwrap().root;
        e.transact(
            h,
            "add",
            vec![Op::InsertNode {
                node: n,
                parent,
                index: 0,
            }],
            0,
        )
        .unwrap();
        e.commit(h, "base", 1, None, false).unwrap();
        (e, h, id)
    }
    fn paint(e: &mut Engine, h: DocHandle, id: NodeId, x: i32, byte: u8) {
        let key = TileKey { tx: x, ty: 0 };
        let hash = e
            .put_tile(TileData::new(PixelFormat::Rgba8, vec![byte; 262144]).unwrap())
            .unwrap();
        let r = e.document(h).unwrap().revision;
        e.transact(
            h,
            "paint",
            vec![Op::SetTiles {
                layer: id,
                changes: vec![TileChange {
                    key,
                    before: None,
                    after: Some(hash),
                }],
            }],
            r,
        )
        .unwrap();
    }
    #[test]
    fn different_tiles_merge_and_same_tile_conflicts() {
        for same in [false, true] {
            let (mut e, h, id) = setup();
            e.create_branch(h, "feature", None, true).unwrap();
            paint(&mut e, h, id, 0, 100);
            e.commit(h, "feature", 2, None, false).unwrap();
            e.checkout(h, "main").unwrap();
            paint(&mut e, h, id, if same { 0 } else { 1 }, 200);
            e.commit(h, "main", 3, None, false).unwrap();
            let p = e.prepare_merge(h, "feature").unwrap();
            if same {
                assert_eq!(p.conflicts.len(), 1);
                assert_eq!(p.conflicts[0].kind, "pixels");
            } else {
                assert!(p.conflicts.is_empty(), "{:?}", p.conflicts);
                e.finish_merge(h, &p.token, vec![], "merge", 4, None)
                    .unwrap();
                let alto_doc::NodeKind::Raster { tiles, .. } =
                    &e.document(h).unwrap().node(id).unwrap().kind
                else {
                    panic!()
                };
                assert_eq!(tiles.tiles.len(), 2);
            }
        }
    }
}

#[cfg(test)]
mod dag_tests {
    use super::*;
    use alto_doc::{DocumentMeta, Op, PropPatch};
    #[test]
    fn crisscross_merge_bases_are_refused_and_dag_log_is_topological() {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(16, 16)).unwrap();
        let root = e.document(h).unwrap().root;
        e.commit(h, "base", 1, None, false).unwrap();
        e.create_branch(h, "left", None, true).unwrap();
        let r = e.document(h).unwrap().revision;
        e.transact(
            h,
            "name",
            vec![Op::SetProps {
                id: root,
                patch: PropPatch {
                    name: Some("left".into()),
                    ..Default::default()
                },
            }],
            r,
        )
        .unwrap();
        e.commit(h, "left", 2, None, false).unwrap();
        e.create_branch(h, "l1", None, false).unwrap();
        e.checkout(h, "main").unwrap();
        e.create_branch(h, "right", None, true).unwrap();
        let r = e.document(h).unwrap().revision;
        e.transact(
            h,
            "opacity",
            vec![Op::SetProps {
                id: root,
                patch: PropPatch {
                    opacity: Some(0.5),
                    ..Default::default()
                },
            }],
            r,
        )
        .unwrap();
        e.commit(h, "right", 3, None, false).unwrap();
        e.create_branch(h, "r1", None, false).unwrap();
        e.checkout(h, "left").unwrap();
        let p = e.prepare_merge(h, "r1").unwrap();
        e.finish_merge(h, &p.token, vec![], "left merge", 4, None)
            .unwrap();
        e.checkout(h, "right").unwrap();
        let p = e.prepare_merge(h, "l1").unwrap();
        e.finish_merge(h, &p.token, vec![], "right merge", 5, None)
            .unwrap();
        let before = e.version_state_digest(h).unwrap();
        assert!(
            e.prepare_merge(h, "left")
                .unwrap_err()
                .to_string()
                .starts_with("MULTIPLE_MERGE_BASES:")
        );
        assert_eq!(e.version_state_digest(h).unwrap(), before);
        let all = e.log_dag(h, true).unwrap();
        assert_eq!(all.len(), 5);
        let order: BTreeMap<_, _> = all.iter().enumerate().map(|(i, c)| (c.hash, i)).collect();
        for c in all {
            for p in c.parents {
                assert!(order[&p] > order[&c.hash]);
            }
        }
    }
}

#[cfg(test)]
mod stash_tests {
    use super::*;
    use alto_core::{PixelFormat, TileKey};
    use alto_doc::{DocumentMeta, Node, NodeKind, Op, PropPatch, TileChange};
    use alto_raster::TileData;

    fn setup() -> (Engine, DocHandle, NodeId, ContentHash) {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(32, 32)).unwrap();
        let n = Node::raster("layer");
        let id = n.id;
        let root = e.document(h).unwrap().root;
        e.transact(
            h,
            "add",
            vec![Op::InsertNode {
                node: n,
                parent: root,
                index: 0,
            }],
            0,
        )
        .unwrap();
        let first = e.commit(h, "base", 1, None, false).unwrap().hash;
        (e, h, id, first)
    }
    fn rename(e: &mut Engine, h: DocHandle, id: NodeId, name: &str) {
        let revision = e.document(h).unwrap().revision;
        e.transact(
            h,
            "edit",
            vec![Op::SetProps {
                id,
                patch: PropPatch {
                    name: Some(name.into()),
                    ..Default::default()
                },
            }],
            revision,
        )
        .unwrap();
    }
    fn paint(e: &mut Engine, h: DocHandle, id: NodeId, v: u8) -> ContentHash {
        let hash = e
            .put_tile(TileData::new(PixelFormat::Rgba8, vec![v; 262144]).unwrap())
            .unwrap();
        let doc = e.document(h).unwrap();
        let NodeKind::Raster { tiles, .. } = &doc.node(id).unwrap().kind else {
            panic!()
        };
        let before = tiles.tiles.get(&TileKey { tx: 0, ty: 0 }).copied();
        let revision = doc.revision;
        e.transact(
            h,
            "paint",
            vec![Op::SetTiles {
                layer: id,
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
    fn stash_names(e: &Engine, h: DocHandle) -> Vec<String> {
        e.version_refs(h)
            .unwrap()
            .stashes
            .iter()
            .map(|s| s.message.clone())
            .collect()
    }

    #[test]
    fn stash_restores_head_and_keeps_the_work() {
        let (mut e, h, id, base) = setup();
        rename(&mut e, h, id, "wip");
        let entry = e
            .stash(h, "work in progress\nsecond line", 2, Some("mimi".into()))
            .unwrap();
        assert_eq!(entry.created_at, 2);
        assert_eq!(entry.message, "work in progress\nsecond line");
        assert_ne!(entry.commit, base);
        assert_eq!(e.commit_view(h, entry.commit).unwrap().parents, vec![base]);
        assert_eq!(
            e.commit_view(h, entry.commit).unwrap().author.as_deref(),
            Some("mimi")
        );
        assert!(
            e.commit_view(h, entry.commit)
                .unwrap()
                .intents
                .iter()
                .any(|i| i.intent == "edit")
        );
        assert_eq!(e.version_refs(h).unwrap().stashes, vec![entry.clone()]);
        assert_eq!(e.version_status(h).unwrap().head, Some(base));
        assert!(!e.version_status(h).unwrap().changed);
        assert_eq!(e.document(h).unwrap().node(id).unwrap().name, "layer");
        assert_eq!(e.log(h).unwrap().len(), 1, "a stash is not on the branch");

        // Applying returns the work as one undoable edit.
        assert!(e.stash_apply(h, entry.commit, false).unwrap().is_none());
        assert_eq!(e.document(h).unwrap().node(id).unwrap().name, "wip");
        assert_eq!(e.version_refs(h).unwrap().stashes.len(), 1);
        e.undo(h).unwrap();
        assert_eq!(e.document(h).unwrap().node(id).unwrap().name, "layer");
        e.redo(h).unwrap();
        assert_eq!(e.document(h).unwrap().node(id).unwrap().name, "wip");
        e.undo(h).unwrap();

        // Pop drops the stash only after applying it.
        assert!(e.stash_apply(h, entry.commit, true).unwrap().is_none());
        assert_eq!(e.document(h).unwrap().node(id).unwrap().name, "wip");
        assert!(e.version_refs(h).unwrap().stashes.is_empty());
        assert_eq!(e.version_status(h).unwrap().head, Some(base));
        e.undo(h).unwrap();
        assert_eq!(e.document(h).unwrap().node(id).unwrap().name, "layer");
    }

    #[test]
    fn stash_with_nothing_to_save_is_refused() {
        let (mut e, h, _, _) = setup();
        let error = e.stash(h, "wip", 2, None).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("INVALID_ARGUMENT: nothing to stash"),
            "{error}"
        );
        assert!(e.version_refs(h).unwrap().stashes.is_empty());

        let mut fresh = Engine::new();
        let d = fresh.create_document(DocumentMeta::srgb_u8(8, 8)).unwrap();
        let error = fresh.stash(d, "wip", 1, None).unwrap_err();
        assert!(error.to_string().starts_with("NO_COMMIT"), "{error}");
    }

    #[test]
    fn stash_in_detached_state_returns_to_the_detached_head() {
        let (mut e, h, id, base) = setup();
        e.detach(h, base).unwrap();
        rename(&mut e, h, id, "detached work");
        let entry = e.stash(h, "detached", 2, None).unwrap();
        assert!(e.version_refs(h).unwrap().current_branch.is_none());
        assert_eq!(e.version_status(h).unwrap().head, Some(base));
        assert_eq!(e.document(h).unwrap().node(id).unwrap().name, "layer");
        assert!(e.stash_apply(h, entry.commit, true).unwrap().is_none());
        assert_eq!(
            e.document(h).unwrap().node(id).unwrap().name,
            "detached work"
        );
        assert!(e.version_refs(h).unwrap().current_branch.is_none());
        assert_eq!(e.version_status(h).unwrap().head, Some(base));
    }

    #[test]
    fn too_many_stashes_is_refused() {
        let (mut e, h, id, _) = setup();
        for n in 0..MAX_STASHES {
            rename(&mut e, h, id, &format!("wip {n}"));
            e.stash(h, &format!("stash {n}"), n as i64 + 2, None)
                .unwrap();
        }
        assert_eq!(e.version_refs(h).unwrap().stashes.len(), MAX_STASHES);
        assert_eq!(
            stash_names(&e, h)[0],
            format!("stash {}", MAX_STASHES - 1),
            "newest first"
        );
        rename(&mut e, h, id, "one too many");
        let error = e.stash(h, "over", 100, None).unwrap_err();
        assert!(error.to_string().starts_with("RESOURCE_LIMIT"), "{error}");
        assert_eq!(
            e.document(h).unwrap().node(id).unwrap().name,
            "one too many",
            "a refused stash leaves the work in place"
        );
        assert_eq!(e.version_refs(h).unwrap().stashes.len(), MAX_STASHES);
    }

    #[test]
    fn repeating_an_identical_stash_keeps_one_entry() {
        let (mut engine, doc, layer, _) = setup();
        rename(&mut engine, doc, layer, "same edit");
        let first = engine.stash(doc, "WIP", 2, None).unwrap();
        rename(&mut engine, doc, layer, "same edit");
        let repeated = engine.stash(doc, "WIP", 2, None).unwrap();
        assert_eq!(first, repeated);
        assert_eq!(engine.version_refs(doc).unwrap().stashes, [first]);
    }

    #[test]
    fn stash_references_must_match_their_commit() {
        let (mut engine, doc, layer, _) = setup();
        rename(&mut engine, doc, layer, "working");
        let entry = engine.stash(doc, "WIP", 2, None).unwrap();
        let store = engine.versions.get(&doc).unwrap();
        let mut wrong = store.clone();
        wrong.refs.stashes[0].message = "different message".into();
        assert!(wrong.validate_graph().is_err());
        let mut wrong = store.clone();
        wrong
            .commits
            .get_mut(&entry.commit)
            .unwrap()
            .parents
            .clear();
        assert!(wrong.validate_graph().is_err());
    }

    #[test]
    fn stash_drop_forgets_only_the_reference() {
        let (mut e, h, id, _) = setup();
        rename(&mut e, h, id, "wip");
        let entry = e.stash(h, "wip", 2, None).unwrap();
        e.stash_drop(h, entry.commit).unwrap();
        assert!(e.version_refs(h).unwrap().stashes.is_empty());
        assert!(e.commit_view(h, entry.commit).is_ok());
        assert_eq!(e.prune_versions(h).unwrap(), 1);
        assert!(e.commit_view(h, entry.commit).is_err());
        let error = e.stash_drop(h, entry.commit).unwrap_err();
        assert!(error.to_string().starts_with("NOT_FOUND"), "{error}");
    }

    #[test]
    fn gc_keeps_stashed_objects_and_prune_keeps_stashes() {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(256, 256)).unwrap();
        let node = Node::raster("layer");
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
        let base_tile = paint(&mut e, h, id, 10);
        let base = e.commit(h, "base", 1, None, false).unwrap().hash;
        let stashed_tile = paint(&mut e, h, id, 20);
        let entry = e.stash(h, "wip", 2, None).unwrap();

        assert!(e.tile(&stashed_tile).unwrap().is_some());
        assert_eq!(e.prune_versions(h).unwrap(), 0, "a stash is a root");
        assert!(e.tile(&stashed_tile).unwrap().is_some());
        let entries = e.folder_entries(h).unwrap();
        assert!(entries.contains_key(&alto_store::commit_path(&entry.commit)));
        assert!(entries.contains_key(&alto_store::object_path(&stashed_tile)));
        assert!(entries.contains_key(&alto_store::object_path(&base_tile)));
        let plan = e.prepare_gc(h, &entries, true).unwrap();
        assert!(
            !plan
                .remove
                .contains(&alto_store::commit_path(&entry.commit))
        );
        assert!(
            !plan
                .remove
                .contains(&alto_store::object_path(&stashed_tile))
        );

        // Forgetting the stash makes its commit and objects garbage.
        e.stash_drop(h, entry.commit).unwrap();
        assert_eq!(e.prune_versions(h).unwrap(), 1);
        assert!(!e.versions[&h].commits.contains_key(&entry.commit));
        assert_eq!(e.version_status(h).unwrap().head, Some(base));
        let entries = e.folder_entries(h).unwrap();
        assert!(!entries.contains_key(&alto_store::object_path(&stashed_tile)));
        assert!(entries.contains_key(&alto_store::object_path(&base_tile)));
    }

    #[test]
    fn stash_survives_save_reopen_and_journal_replay() {
        let (mut e, h, id, _) = setup();
        let initial = e.save_package(h).unwrap();
        let before = e.capture_version_state(h).unwrap();
        rename(&mut e, h, id, "wip");
        let entry = e.stash(h, "wip", 2, None).unwrap();
        let record = e.version_record(h, before.digest).unwrap();
        let after = e.version_state_digest(h).unwrap();
        e.release_version_state(before);

        let bytes = e.save_package(h).unwrap();
        let mut fresh = Engine::new();
        let (d, warnings) = fresh.open_package(&bytes).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(fresh.version_refs(d).unwrap().stashes, vec![entry.clone()]);
        assert!(fresh.stash_apply(d, entry.commit, true).unwrap().is_none());
        assert_eq!(fresh.document(d).unwrap().node(id).unwrap().name, "wip");

        let mut replay = Engine::new();
        let (r, warnings) = replay.open_package(&initial).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            replay.replay_record(r, &record).unwrap(),
            crate::Replayed::Applied
        );
        assert_eq!(replay.version_state_digest(r).unwrap(), after);
        assert_eq!(replay.version_refs(r).unwrap().stashes, vec![entry.clone()]);
        assert!(replay.stash_apply(r, entry.commit, true).unwrap().is_none());
        assert_eq!(replay.document(r).unwrap().node(id).unwrap().name, "wip");
    }

    #[test]
    fn conflicting_pop_keeps_the_stash_until_finish() {
        let (mut e, h, id, base) = setup();
        rename(&mut e, h, id, "theirs");
        let entry = e.stash(h, "theirs", 2, None).unwrap();
        rename(&mut e, h, id, "ours");
        let ours = e.commit(h, "ours", 3, None, false).unwrap().hash;

        let plan = e.stash_apply(h, entry.commit, true).unwrap().unwrap();
        assert_eq!(plan.kind, "stash");
        assert_eq!(plan.ours, ours);
        assert_eq!(plan.theirs, entry.commit);
        assert_eq!(plan.base, Some(base));
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].kind, "property");
        assert_eq!(e.document(h).unwrap().node(id).unwrap().name, "ours");
        assert_eq!(e.version_status(h).unwrap().head, Some(ours));
        assert!(
            e.version_refs(h)
                .unwrap()
                .stashes
                .iter()
                .any(|s| s.commit == entry.commit),
            "the stash stays until the finish succeeds"
        );

        let resolution = super::super::MergeResolution {
            conflict_id: plan.conflicts[0].id.clone(),
            choice: "theirs".into(),
            value: None,
        };
        let result = e
            .finish_merge(h, &plan.token, vec![resolution], "ignored", 4, None)
            .unwrap();
        assert_eq!(result.kind, "stash");
        assert_eq!(result.head, ours, "the head does not move");
        assert_eq!(e.version_status(h).unwrap().head, Some(ours));
        assert_eq!(e.log(h).unwrap().len(), 2, "no merge commit is written");
        assert!(e.version_refs(h).unwrap().stashes.is_empty());
        assert_eq!(e.document(h).unwrap().node(id).unwrap().name, "theirs");
        e.undo(h).unwrap();
        assert_eq!(e.document(h).unwrap().node(id).unwrap().name, "ours");
    }

    #[test]
    fn resolving_a_stash_conflict_to_the_current_value_consumes_the_plan() {
        let (mut engine, doc, layer, _) = setup();
        rename(&mut engine, doc, layer, "stashed");
        let entry = engine.stash(doc, "WIP", 2, None).unwrap();
        rename(&mut engine, doc, layer, "current");
        engine.commit(doc, "current", 3, None, false).unwrap();

        let plan = engine
            .stash_apply(doc, entry.commit, false)
            .unwrap()
            .unwrap();
        let resolution = super::super::MergeResolution {
            conflict_id: plan.conflicts[0].id.clone(),
            choice: "ours".into(),
            value: None,
        };
        let revision = engine.document(doc).unwrap().revision;
        engine
            .finish_merge(doc, &plan.token, vec![resolution.clone()], "", 4, None)
            .unwrap();
        assert_eq!(engine.document(doc).unwrap().revision, revision);
        assert_eq!(engine.version_refs(doc).unwrap().stashes, [entry]);
        let error = engine
            .finish_merge(doc, &plan.token, vec![resolution], "", 5, None)
            .unwrap_err();
        assert!(error.to_string().contains("STALE_MERGE"), "{error}");
    }

    #[test]
    fn conflicting_pop_finishes_while_detached() {
        let (mut e, h, id, _) = setup();
        rename(&mut e, h, id, "theirs");
        let entry = e.stash(h, "theirs", 2, None).unwrap();
        rename(&mut e, h, id, "ours");
        let ours = e.commit(h, "ours", 3, None, false).unwrap().hash;
        e.detach(h, ours).unwrap();
        let main = e.version_refs(h).unwrap().branches["main"].head;

        let plan = e.stash_apply(h, entry.commit, true).unwrap().unwrap();
        assert_eq!(plan.kind, "stash");
        let resolution = super::super::MergeResolution {
            conflict_id: plan.conflicts[0].id.clone(),
            choice: "ours".into(),
            value: None,
        };
        let result = e
            .finish_merge(h, &plan.token, vec![resolution], "ignored", 4, None)
            .unwrap();
        assert_eq!(result.kind, "stash");
        assert_eq!(result.head, ours);
        assert!(e.version_refs(h).unwrap().current_branch.is_none());
        assert_eq!(e.version_refs(h).unwrap().branches["main"].head, main);
        assert_eq!(e.version_status(h).unwrap().head, Some(ours));
        assert_eq!(e.document(h).unwrap().node(id).unwrap().name, "ours");
    }

    #[test]
    fn stash_keeps_the_documents_required_format() {
        let (mut e, h, id, _) = setup();
        let node = Node::text(
            "punctuation",
            alto_text::TextContent {
                text: "あい。".into(),
                hanging: true,
                ..Default::default()
            },
        );
        let doc = e.document(h).unwrap();
        e.transact(
            h,
            "layer.add",
            vec![Op::InsertNode {
                node,
                parent: doc.root,
                index: 0,
            }],
            doc.revision,
        )
        .unwrap();
        e.commit(h, "format 6", 2, None, false).unwrap();
        assert_eq!(e.document(h).unwrap().meta.format_version, 6);

        rename(&mut e, h, id, "stashed");
        let entry = e.stash(h, "wip", 3, None).unwrap();
        let revision = e.document(h).unwrap().revision;
        e.transact(
            h,
            "opacity",
            vec![Op::SetProps {
                id,
                patch: PropPatch {
                    opacity: Some(0.5),
                    ..Default::default()
                },
            }],
            revision,
        )
        .unwrap();
        e.commit(h, "head", 4, None, false).unwrap();
        assert!(e.stash_apply(h, entry.commit, true).unwrap().is_none());
        assert_eq!(e.document(h).unwrap().meta.format_version, 6);
        let node = e.document(h).unwrap().node(id).unwrap();
        assert_eq!(node.name, "stashed");
        assert_eq!(node.props.opacity, 0.5);
    }

    #[test]
    fn a_manifest_without_stashes_keeps_its_bytes() {
        let (mut e, h, id, _) = setup();
        let entries = e.folder_entries(h).unwrap();
        let manifest: alto_store::Manifest =
            serde_json::from_slice(&entries["manifest.json"]).unwrap();
        assert_eq!(manifest.format, 2);
        assert!(manifest.refs.stashes.is_empty());
        assert!(!String::from_utf8_lossy(&entries["manifest.json"]).contains("stashes"));

        rename(&mut e, h, id, "wip");
        e.stash(h, "wip", 2, None).unwrap();
        let entries = e.folder_entries(h).unwrap();
        let manifest: alto_store::Manifest =
            serde_json::from_slice(&entries["manifest.json"]).unwrap();
        assert_eq!(manifest.format, 4, "a stash raises the manifest format");
        assert_eq!(manifest.refs.stashes.len(), 1);
    }
}
