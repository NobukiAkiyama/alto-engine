//! Transactions, undo/redo and the intent log.
use super::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedOp {
    pub op: Op,
    pub inverse: Op,
}
/// What an edit was for: an identifier such as `layer.add`, and a label the host may
/// give for display. History and commit summaries keep both; neither is shown text the
/// engine chose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}
impl Intent {
    pub fn labeled(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: Some(label.into()),
        }
    }
}
impl From<&str> for Intent {
    fn from(id: &str) -> Self {
        Self {
            id: id.into(),
            label: None,
        }
    }
}
impl From<String> for Intent {
    fn from(id: String) -> Self {
        Self { id, label: None }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transaction {
    pub intent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub ops: Vec<AppliedOp>,
    pub base_revision: u64,
    #[serde(default)]
    pub seq: u64,
}

/// Undo and redo stacks and the commit intent log, as a recovery checkpoint keeps them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct History {
    pub undo: Vec<Transaction>,
    pub redo: Vec<Transaction>,
    pub seq: u64,
    pub log: Vec<IntentLogEntry>,
}

/// Edits since the last version commit, summarised into the commit's intents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentLogEntry {
    pub seq: u64,
    pub intent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// An undo of the edit, recorded when the edit was in an earlier commit.
    #[serde(default)]
    pub undone: bool,
    pub targets: Vec<NodeId>,
}

#[derive(Clone)]
pub struct Editor {
    pub doc: Document,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    max_undo: usize,
    seq: u64,
    log: Vec<IntentLogEntry>,
    digest: digest::StateDigest,
}
impl Editor {
    pub fn new(doc: Document) -> Self {
        Self {
            digest: digest::StateDigest::of(&doc),
            doc,
            undo: vec![],
            redo: vec![],
            max_undo: 100,
            seq: 0,
            log: vec![],
        }
    }
    /// Identifies the document's content; kept current by every edit, undo and redo.
    pub fn state_digest(&self) -> ContentHash {
        self.digest.value()
    }
    /// Edits recorded since the last call (the last commit).
    pub fn take_intent_log(&mut self) -> Vec<IntentLogEntry> {
        std::mem::take(&mut self.log)
    }
    pub fn intent_log(&self) -> &[IntentLogEntry] {
        &self.log
    }
    fn log_tx(&mut self, tx: &Transaction, undone: bool) {
        let mut targets = vec![];
        for op in &tx.ops {
            if let Some(t) = op.op.target()
                && !targets.contains(&t)
            {
                targets.push(t);
            }
        }
        self.log.push(IntentLogEntry {
            seq: tx.seq,
            intent: tx.intent.clone(),
            label: tx.label.clone(),
            undone,
            targets,
        });
    }
    /// Applies `ops` to the live document. A failed op or a failed graph check
    /// rolls the completed ops back, so the caller does not keep a second copy
    /// of the document just to discard a rejected edit.
    fn replay(
        &mut self,
        ops: Vec<Op>,
        enforce: bool,
    ) -> Result<(Vec<AppliedOp>, ChangeSet), DocError> {
        let revision = self.doc.revision;
        let mut applied = Vec::with_capacity(ops.len());
        let mut changes = ChangeSet::default();
        for op in ops {
            match self.doc.apply(op.clone(), enforce) {
                Ok((inverse, change)) => {
                    applied.push(AppliedOp { op, inverse });
                    changes.merge(change);
                }
                Err(e) => {
                    let revert_err = self.revert(&applied).err();
                    self.doc.revision = revision;
                    return Err(revert_err.unwrap_or(e));
                }
            }
        }
        if let Err(e) = self.doc.validate_graph() {
            let revert_err = self.revert(&applied).err();
            self.doc.revision = revision;
            return Err(revert_err.unwrap_or(e));
        }
        Ok((applied, changes))
    }
    /// Rolls back completed ops. Lock checks are off: the inverse of an op this
    /// transaction applied is part of that transaction. Any other failure is
    /// returned; this does not panic.
    fn revert(&mut self, applied: &[AppliedOp]) -> Result<(), DocError> {
        for item in applied.iter().rev() {
            self.doc
                .apply(item.inverse.clone(), false)
                .map_err(|error| {
                    DocError::Invalid(format!("inverse of an applied edit: {error}"))
                })?;
        }
        Ok(())
    }
    pub fn transact(
        &mut self,
        intent: impl Into<Intent>,
        ops: Vec<Op>,
        base_revision: u64,
    ) -> Result<ChangeSet, DocError> {
        if base_revision != self.doc.revision {
            return Err(DocError::Conflict {
                expected: base_revision,
                actual: self.doc.revision,
            });
        }
        if ops.is_empty() {
            return Ok(ChangeSet::default());
        }
        let (applied, changes) = self.replay(ops, true)?;
        self.doc.revision += 1;
        self.seq += 1;
        let intent = intent.into();
        let tx = Transaction {
            intent: intent.id,
            label: intent.label,
            ops: applied,
            base_revision,
            seq: self.seq,
        };
        self.log_tx(&tx, false);
        self.undo.push(tx);
        if self.undo.len() > self.max_undo {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.digest.update(&self.doc, &changes);
        Ok(changes)
    }
    pub fn undo(&mut self) -> Result<Option<ChangeSet>, DocError> {
        let Some(tx) = self.undo.last().cloned() else {
            return Ok(None);
        };
        let inverses = tx
            .ops
            .iter()
            .rev()
            .map(|item| item.inverse.clone())
            .collect();
        let (_, changes) = self.replay(inverses, false)?;
        self.doc.revision += 1;
        self.undo.pop();
        if self.log.last().is_some_and(|e| e.seq == tx.seq) {
            self.log.pop();
        } else {
            self.log_tx(&tx, true);
        }
        self.redo.push(tx);
        self.digest.update(&self.doc, &changes);
        Ok(Some(changes))
    }
    pub fn redo(&mut self) -> Result<Option<ChangeSet>, DocError> {
        let Some(tx) = self.redo.last().cloned() else {
            return Ok(None);
        };
        let ops = tx.ops.iter().map(|item| item.op.clone()).collect();
        let (_, changes) = self.replay(ops, true)?;
        self.doc.revision += 1;
        self.redo.pop();
        self.log_tx(&tx, false);
        self.undo.push(tx);
        self.digest.update(&self.doc, &changes);
        Ok(Some(changes))
    }
    pub fn drop_oldest_undo(&mut self) -> bool {
        if self.undo.is_empty() {
            false
        } else {
            self.undo.remove(0);
            true
        }
    }
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }
    pub fn undo_ops(&self) -> Vec<Op> {
        self.undo
            .last()
            .map(|t| t.ops.iter().rev().map(|o| o.inverse.clone()).collect())
            .unwrap_or_default()
    }
    pub fn redo_ops(&self) -> Vec<Op> {
        self.redo
            .last()
            .map(|t| t.ops.iter().map(|o| o.op.clone()).collect())
            .unwrap_or_default()
    }
    /// Oldest first.
    pub fn undo_history(&self) -> &[Transaction] {
        &self.undo
    }
    /// The next redo is last.
    pub fn redo_history(&self) -> &[Transaction] {
        &self.redo
    }
    pub fn drop_oldest_undos(&mut self, count: usize) {
        self.undo.drain(..count.min(self.undo.len()));
    }
    pub fn history(&self) -> History {
        History {
            undo: self.undo.clone(),
            redo: self.redo.clone(),
            seq: self.seq,
            log: self.log.clone(),
        }
    }
    /// Reinstates stacks saved with this document's content. The caller guarantees
    /// they were taken from the same document state.
    pub fn restore_history(&mut self, history: History) {
        self.undo = history.undo;
        self.redo = history.redo;
        self.seq = history.seq;
        self.log = history.log;
        if self.undo.len() > self.max_undo {
            let excess = self.undo.len() - self.max_undo;
            self.undo.drain(..excess);
        }
    }
    /// Forgets the most recent redo entry, for an undo that reverted a replayed edit.
    pub fn drop_newest_redo(&mut self) {
        self.redo.pop();
    }
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn editor() -> Editor {
        Editor::new(Document::new(DocumentMeta::srgb_u8(1920, 1080)).unwrap())
    }
    fn content(mut doc: Document) -> Document {
        doc.revision = 0;
        doc
    }
    #[test]
    fn incremental_state_digest_matches_a_full_recompute_after_every_edit() {
        let mut e = editor();
        let root = e.doc.root;
        let check = |e: &Editor, step: &str| {
            assert_eq!(
                e.state_digest(),
                digest::StateDigest::of(&e.doc).value(),
                "{step}"
            );
        };
        let initial = e.state_digest();
        let (a, b, g) = (Node::raster("A"), Node::raster("B"), Node::group("G"));
        let (a, b, g) = {
            let ids = (a.id, b.id, g.id);
            let rev = e.doc.revision;
            e.transact(
                "add",
                vec![
                    Op::InsertNode {
                        node: a,
                        parent: root,
                        index: 0,
                    },
                    Op::InsertNode {
                        node: b,
                        parent: root,
                        index: 1,
                    },
                    Op::InsertNode {
                        node: g,
                        parent: root,
                        index: 2,
                    },
                ],
                rev,
            )
            .unwrap();
            ids
        };
        check(&e, "insert");
        let tile = |seed: &[u8]| TileChange {
            key: TileKey { tx: 0, ty: 0 },
            before: None,
            after: Some(ContentHash::of(seed)),
        };
        let edits: Vec<(&str, Vec<Op>)> = vec![
            (
                "tiles",
                vec![Op::SetTiles {
                    layer: a,
                    changes: vec![tile(b"t")],
                }],
            ),
            (
                "move into group",
                vec![Op::MoveNode {
                    id: b,
                    parent: g,
                    index: 0,
                }],
            ),
            (
                "reorder",
                vec![Op::MoveNode {
                    id: g,
                    parent: root,
                    index: 0,
                }],
            ),
            (
                "props",
                vec![Op::SetProps {
                    id: a,
                    patch: PropPatch {
                        opacity: Some(0.5),
                        name: Some("A2".into()),
                        ..Default::default()
                    },
                }],
            ),
            ("remove group", vec![Op::RemoveNode { id: g }]),
            ("selection", vec![Op::SetSelection { selection: None }]),
            (
                "meta",
                vec![Op::SetMeta {
                    meta: DocumentMeta::srgb_u8(800, 600),
                }],
            ),
        ];
        for (step, ops) in edits {
            let rev = e.doc.revision;
            e.transact(step, ops, rev).unwrap();
            check(&e, step);
        }
        while e.undo().unwrap().is_some() {
            check(&e, "undo");
        }
        assert_eq!(e.state_digest(), initial, "undo back to the start");
        while e.redo().unwrap().is_some() {
            check(&e, "redo");
        }
    }

    #[test]
    fn transaction_undo_redo_and_canonical_snapshot() {
        let mut editor = editor();
        let before = editor.doc.clone();
        let layer = Node::raster("Background");
        let id = layer.id;
        let root = editor.doc.root;
        editor
            .transact(
                "Add layer",
                vec![Op::InsertNode {
                    node: layer,
                    parent: root,
                    index: 0,
                }],
                0,
            )
            .unwrap();
        let changed = editor.doc.clone();
        let bytes = changed.snapshot_bytes().unwrap();
        let reopened = Document::from_snapshot_bytes(&bytes).unwrap();
        assert_eq!(reopened.snapshot_bytes().unwrap(), bytes);
        assert_eq!(content(reopened), content(changed.clone()));
        assert!(editor.undo().unwrap().unwrap().structure_changed);
        assert_eq!(content(editor.doc.clone()), content(before.clone()));
        assert_eq!(
            editor.doc.snapshot_bytes().unwrap(),
            before.snapshot_bytes().unwrap()
        );
        assert!(editor.redo().unwrap().unwrap().nodes.contains(&id));
        assert_eq!(content(editor.doc.clone()), content(changed));
    }

    #[test]
    fn failed_transaction_is_atomic_and_cycle_is_rejected() {
        let mut editor = editor();
        let root = editor.doc.root;
        let group = Node::group("Group");
        let id = group.id;
        let before = editor.doc.clone();
        let result = editor.transact(
            "Invalid",
            vec![
                Op::InsertNode {
                    node: group.clone(),
                    parent: root,
                    index: 0,
                },
                Op::MoveNode {
                    id,
                    parent: id,
                    index: 0,
                },
            ],
            0,
        );
        assert!(matches!(result, Err(DocError::Invalid(_))));
        assert_eq!(editor.doc, before);
        assert_eq!(editor.undo_len(), 0);
    }

    #[test]
    fn tile_hash_changes_are_undone_without_pixel_copies() {
        let mut editor = editor();
        let root = editor.doc.root;
        let layer = Node::raster("Paint");
        let id = layer.id;
        editor
            .transact(
                "Add",
                vec![Op::InsertNode {
                    node: layer,
                    parent: root,
                    index: 0,
                }],
                0,
            )
            .unwrap();
        let base = editor.doc.clone();
        let key = TileKey { tx: -1, ty: 2 };
        let hash = ContentHash::of(b"tile pixels");
        let changed = editor
            .transact(
                "Stroke",
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
        assert_eq!(changed.tiles, vec![(id, key)]);
        assert!(editor.undo().unwrap().is_some());
        assert_eq!(content(editor.doc.clone()), content(base));
        assert!(editor.redo().unwrap().is_some());
        let NodeKind::Raster { tiles, .. } = &editor.doc.node(id).unwrap().kind else {
            panic!("raster expected")
        };
        assert_eq!(tiles.tiles.get(&key), Some(&hash));
    }

    #[test]
    fn lock_rejects_edits_and_a_clearing_patch_unlocks() {
        let mut editor = editor();
        let root = editor.doc.root;
        let layer = Node::raster("Paint");
        let id = layer.id;
        let child = Node::raster("Child");
        let child_id = child.id;
        editor
            .transact(
                "add",
                vec![
                    Op::InsertNode {
                        node: layer,
                        parent: root,
                        index: 0,
                    },
                    Op::InsertNode {
                        node: Node::group("Group"),
                        parent: root,
                        index: 1,
                    },
                ],
                0,
            )
            .unwrap();
        let group = editor.doc.node(root).unwrap().children().unwrap()[1];
        editor
            .transact(
                "child",
                vec![Op::InsertNode {
                    node: child,
                    parent: group,
                    index: 0,
                }],
                1,
            )
            .unwrap();
        let lock = |id| Op::SetProps {
            id,
            patch: PropPatch {
                locked: Some(true),
                ..Default::default()
            },
        };
        editor
            .transact("lock", vec![lock(id), lock(group)], 2)
            .unwrap();
        let rev = editor.doc.revision;
        let stroke = Op::SetTiles {
            layer: id,
            changes: vec![],
        };
        assert!(
            editor
                .transact("stroke", vec![stroke], rev)
                .unwrap_err()
                .to_string()
                .contains("locked")
        );
        assert!(
            editor
                .transact(
                    "opacity",
                    vec![Op::SetProps {
                        id,
                        patch: PropPatch {
                            opacity: Some(0.5),
                            ..Default::default()
                        },
                    }],
                    rev,
                )
                .is_err()
        );
        assert!(
            editor
                .transact("remove", vec![Op::RemoveNode { id }], rev)
                .is_err()
        );
        assert!(
            editor
                .transact(
                    "child stroke",
                    vec![Op::SetTiles {
                        layer: child_id,
                        changes: vec![],
                    }],
                    rev,
                )
                .is_err(),
            "a locked group blocks its children"
        );
        assert!(
            editor
                .transact(
                    "child unlock",
                    vec![Op::SetProps {
                        id: child_id,
                        patch: PropPatch {
                            locked: Some(false),
                            ..Default::default()
                        },
                    }],
                    rev,
                )
                .is_err(),
            "a child cannot unlock itself out of a locked group"
        );
        editor
            .transact(
                "unlock",
                vec![Op::SetProps {
                    id,
                    patch: PropPatch {
                        locked: Some(false),
                        ..Default::default()
                    },
                }],
                rev,
            )
            .unwrap();
        assert!(!editor.doc.node(id).unwrap().props.locked);
        editor.undo().unwrap();
        assert!(editor.doc.node(id).unwrap().props.locked);
        editor.undo().unwrap();
        assert!(!editor.doc.node(id).unwrap().props.locked);
        let rev = editor.doc.revision;
        editor
            .transact(
                "stroke",
                vec![Op::SetTiles {
                    layer: id,
                    changes: vec![],
                }],
                rev,
            )
            .unwrap();
    }

    #[test]
    fn locked_insert_rolls_back_when_a_later_op_fails() {
        let mut editor = editor();
        let root = editor.doc.root;
        let mut layer = Node::raster("Locked");
        layer.props.locked = true;
        let id = layer.id;
        let err = editor
            .transact(
                "insert then edit",
                vec![
                    Op::InsertNode {
                        node: layer,
                        parent: root,
                        index: 0,
                    },
                    Op::SetProps {
                        id,
                        patch: PropPatch {
                            opacity: Some(0.2),
                            ..Default::default()
                        },
                    },
                ],
                0,
            )
            .unwrap_err();
        assert!(err.to_string().contains("locked"), "{err}");
        assert!(editor.doc.node(id).is_err());
        assert_eq!(editor.doc.nodes.len(), 1);
        assert_eq!(editor.doc.revision, 0);
        assert!(editor.undo().unwrap().is_none());
    }

    #[test]
    fn undo_removes_a_node_inserted_already_locked() {
        let mut editor = editor();
        let root = editor.doc.root;
        let mut layer = Node::raster("Locked");
        layer.props.locked = true;
        let id = layer.id;
        editor
            .transact(
                "insert locked",
                vec![Op::InsertNode {
                    node: layer,
                    parent: root,
                    index: 0,
                }],
                0,
            )
            .unwrap();
        assert!(editor.doc.node(id).unwrap().props.locked);
        editor.undo().unwrap();
        assert!(editor.doc.node(id).is_err());
        editor.redo().unwrap();
        assert!(editor.doc.node(id).unwrap().props.locked);
    }

    #[test]
    fn unlock_patch_cannot_carry_other_fields() {
        let mut editor = editor();
        let root = editor.doc.root;
        let layer = Node::raster("Paint");
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
        editor
            .transact(
                "lock",
                vec![Op::SetProps {
                    id,
                    patch: PropPatch {
                        locked: Some(true),
                        ..Default::default()
                    },
                }],
                1,
            )
            .unwrap();
        let err = editor
            .transact(
                "unlock and fade",
                vec![Op::SetProps {
                    id,
                    patch: PropPatch {
                        locked: Some(false),
                        opacity: Some(0.4),
                        ..Default::default()
                    },
                }],
                2,
            )
            .unwrap_err();
        assert!(err.to_string().contains("locked"), "{err}");
        let node = editor.doc.node(id).unwrap();
        assert!(node.props.locked);
        assert_eq!(node.props.opacity, 1.0);
        editor
            .transact(
                "unlock then fade",
                vec![
                    Op::SetProps {
                        id,
                        patch: PropPatch {
                            locked: Some(false),
                            ..Default::default()
                        },
                    },
                    Op::SetProps {
                        id,
                        patch: PropPatch {
                            opacity: Some(0.4),
                            ..Default::default()
                        },
                    },
                ],
                2,
            )
            .unwrap();
        let node = editor.doc.node(id).unwrap();
        assert!(!node.props.locked);
        assert_eq!(node.props.opacity, 0.4);
    }

    #[test]
    fn replace_and_remove_respect_locks() {
        let mut editor = editor();
        let root = editor.doc.root;
        let group = Node::group("Group");
        let group_id = group.id;
        let mut child = Node::raster("Child");
        child.props.locked = true;
        let child_id = child.id;
        let sibling = Node::raster("Sibling");
        let sibling_id = sibling.id;
        editor
            .transact(
                "tree",
                vec![
                    Op::InsertNode {
                        node: group,
                        parent: root,
                        index: 0,
                    },
                    Op::InsertNode {
                        node: child,
                        parent: group_id,
                        index: 0,
                    },
                    Op::InsertNode {
                        node: sibling,
                        parent: root,
                        index: 1,
                    },
                ],
                0,
            )
            .unwrap();
        editor
            .transact(
                "lock sibling",
                vec![Op::SetProps {
                    id: sibling_id,
                    patch: PropPatch {
                        locked: Some(true),
                        ..Default::default()
                    },
                }],
                1,
            )
            .unwrap();
        let mut replacement = editor.doc.node(sibling_id).unwrap().clone();
        replacement.name = "Renamed".into();
        replacement.props.locked = false;
        let err = editor
            .transact(
                "replace",
                vec![Op::ReplaceNode {
                    id: sibling_id,
                    node: replacement,
                }],
                2,
            )
            .unwrap_err();
        assert!(err.to_string().contains("locked"), "{err}");
        assert_eq!(editor.doc.node(sibling_id).unwrap().name, "Sibling");
        assert!(editor.doc.node(sibling_id).unwrap().props.locked);
        let err = editor
            .transact("remove group", vec![Op::RemoveNode { id: group_id }], 2)
            .unwrap_err();
        assert!(err.to_string().contains("locked"), "{err}");
        assert!(editor.doc.node(child_id).is_ok());
        assert!(editor.doc.node(group_id).is_ok());
    }

    #[test]
    fn isolated_groups_stop_at_the_compositor_limit() {
        let mut editor = editor();
        let mut parent = editor.doc.root;
        let mut ops = vec![];
        for i in 0..MAX_ISOLATED_GROUPS {
            let mut node = Node::group(format!("g{i}"));
            node.props.opacity = 0.5;
            let id = node.id;
            ops.push(Op::InsertNode {
                node,
                parent,
                index: 0,
            });
            parent = id;
        }
        editor.transact("stack", ops, 0).unwrap();
        let mut extra = Node::group("one more");
        extra.props.opacity = 0.5;
        let err = editor
            .transact(
                "too deep",
                vec![Op::InsertNode {
                    node: extra,
                    parent,
                    index: 0,
                }],
                editor.doc.revision,
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("31"), "{err}");
        assert_eq!(editor.doc.nodes.len(), MAX_ISOLATED_GROUPS as usize + 1);
    }

    #[test]
    fn a_hidden_stack_can_exceed_the_compositor_until_it_is_shown() {
        let mut editor = editor();
        let mut parent = editor.doc.root;
        let mut ops = vec![];
        let mut top = None;
        for i in 0..40 {
            let mut node = Node::group(format!("h{i}"));
            node.props.opacity = 0.5;
            if i == 0 {
                node.props.visible = false;
                top = Some(node.id);
            }
            let id = node.id;
            ops.push(Op::InsertNode {
                node,
                parent,
                index: 0,
            });
            parent = id;
        }
        editor.transact("hidden", ops, 0).unwrap();
        let err = editor
            .transact(
                "show",
                vec![Op::SetProps {
                    id: top.unwrap(),
                    patch: PropPatch {
                        visible: Some(true),
                        ..Default::default()
                    },
                }],
                editor.doc.revision,
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("31"), "{err}");
        assert!(!editor.doc.node(top.unwrap()).unwrap().props.visible);
    }

    proptest! {
        #[test]
        fn reversible_op_preserves_document(name in "[a-zA-Z0-9]{1,32}", opacity in 0.0f32..1.0f32) {
            let mut editor = editor();
            let root = editor.doc.root;
            let node = Node::raster(name.clone());
            let id = node.id;
            let before = editor.doc.clone();
            editor.transact("insert and change", vec![
                Op::InsertNode { node, parent: root, index: 0 },
                Op::SetProps { id, patch: PropPatch { opacity: Some(opacity), ..Default::default() } },
            ], 0).unwrap();
            editor.undo().unwrap();
            prop_assert_eq!(content(editor.doc), content(before));
        }
    }
}
