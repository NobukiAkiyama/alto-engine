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
        for h in self.refs.roots().chain(self.head) {
            self.ancestors(h)?;
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
        self.documents.insert(h, alto_doc::Editor::new(doc));
        self.maintain();
        Ok(())
    }
    pub fn checkout(&mut self, h: DocHandle, name: &str) -> Result<(), EngineError> {
        self.require_objects(
            h,
            &super::lazy::ObjectTarget::Branch {
                name: name.to_string(),
            },
        )?;
        self.clean_head(h)?;
        let name = ref_name(name)?;
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
