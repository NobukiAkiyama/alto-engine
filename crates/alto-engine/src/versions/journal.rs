use super::branches::fail;
use super::*;

/// A rollback point whose objects remain rooted while a history command is prepared.
pub struct VersionCheckpoint {
    doc: Document,
    history: alto_doc::History,
    versions: VersionStore,
    stage: u64,
    /// Every object the state before the command references: a replay starts from that
    /// state, so `version_record_since` leaves them out.
    hashes: BTreeSet<ContentHash>,
    pub digest: ContentHash,
}
impl Engine {
    pub fn version_state_digest(&self, h: DocHandle) -> Result<ContentHash, EngineError> {
        let mut b = self.state_digest(h)?.0.to_vec();
        b.extend(self.versions_digest(h)?.0);
        Ok(ContentHash::of(&b))
    }
    pub fn capture_version_state(
        &mut self,
        h: DocHandle,
    ) -> Result<VersionCheckpoint, EngineError> {
        let doc = self.document(h)?.clone();
        let history = self.documents[&h].history();
        let versions = self.versions(h)?.clone();
        let mut hashes = doc_hashes(&doc);
        hashes.extend(versions.retained.iter().copied());
        for tx in history.undo.iter().chain(&history.redo) {
            crate::tx_hashes(tx, &mut hashes);
        }
        let digest = self.version_state_digest(h)?;
        let stage = self.begin_tile_stage();
        self.staged_tiles.insert(stage, hashes.clone());
        Ok(VersionCheckpoint {
            doc,
            history,
            versions,
            stage,
            hashes,
            digest,
        })
    }
    pub fn release_version_state(&mut self, state: VersionCheckpoint) {
        self.release_staged_tiles(state.stage);
    }
    pub fn rollback_version_state(&mut self, h: DocHandle, state: VersionCheckpoint) {
        let mut editor = alto_doc::Editor::new(state.doc);
        editor.set_max_undo(self.max_undo);
        editor.restore_history(state.history);
        self.install_editor(h, editor);
        self.versions.insert(h, state.versions);
        self.release_staged_tiles(state.stage);
    }
    /// A self-contained history state transition. Normal edits still use compact Op records.
    pub fn version_record(
        &self,
        h: DocHandle,
        before: ContentHash,
    ) -> Result<Vec<u8>, EngineError> {
        self.version_record_without(h, before, &BTreeSet::new(), false)
    }
    /// A history state transition without the objects the state before it references.
    /// A replay starts from that state (its digest must match), so it holds them, and
    /// the record stays the size of what the command changed, not of the document.
    pub fn version_record_since(
        &self,
        h: DocHandle,
        before: &VersionCheckpoint,
    ) -> Result<Vec<u8>, EngineError> {
        // A document reading objects from its folder on demand may not hold them when
        // its journal replays, so its records stay complete.
        if self.lazy.contains_key(&h) {
            return self.version_record(h, before.digest);
        }
        let held: BTreeSet<_> = before
            .hashes
            .iter()
            .filter(|hash| self.tiles.contains(hash) || self.blobs.contains_key(hash))
            .copied()
            .collect();
        self.version_record_without(h, before.digest, &held, true)
    }
    fn version_record_without(
        &self,
        h: DocHandle,
        before: ContentHash,
        skip: &BTreeSet<ContentHash>,
        incremental: bool,
    ) -> Result<Vec<u8>, EngineError> {
        let mut checkpoint = self.recovery_checkpoint(h, skip)?.entries;
        checkpoint.insert("version-state.json".into(),serde_json::to_vec(&serde_json::json!({
            "before":before,"after":self.version_state_digest(h)?,"revision":self.document(h)?.revision,
            "incremental":incremental
        })).map_err(|e|invalid(e.to_string()))?);
        Ok(alto_store::write_entries(&checkpoint)?)
    }
    pub(crate) fn replay_version_record(
        &mut self,
        h: DocHandle,
        mut entries: BTreeMap<String, Vec<u8>>,
    ) -> Result<crate::Replayed, EngineError> {
        let header: serde_json::Value = serde_json::from_slice(
            &entries
                .remove("version-state.json")
                .ok_or_else(|| fail("CORRUPT_DATA", "missing version state"))?,
        )
        .map_err(|e| fail("CORRUPT_DATA", e.to_string()))?;
        if header["before"].as_str() != Some(self.version_state_digest(h)?.hex().as_str()) {
            return Ok(crate::Replayed::Diverged);
        }
        let revision = header["revision"]
            .as_u64()
            .ok_or_else(|| fail("CORRUPT_DATA", "missing version revision"))?;
        // An incremental record lists objects it does not carry; the state it starts
        // from holds them. A complete record must carry every one.
        let incremental = header["incremental"].as_bool().unwrap_or(false);
        let listed: serde_json::Value = serde_json::from_slice(
            entries
                .get("checkpoint.json")
                .ok_or_else(|| fail("CORRUPT_DATA", "missing checkpoint metadata"))?,
        )
        .map_err(|e| fail("CORRUPT_DATA", e.to_string()))?;
        let mut filled = BTreeSet::new();
        for path in listed["objects"].as_array().into_iter().flatten() {
            let path = path
                .as_str()
                .ok_or_else(|| fail("CORRUPT_DATA", "object path"))?;
            if entries.contains_key(path) || !incremental {
                continue;
            }
            let hash: ContentHash = path
                .rsplit('/')
                .next()
                .unwrap()
                .parse()
                .map_err(|_| fail("CORRUPT_DATA", "object path"))?;
            if path != alto_store::object_path(&hash) {
                return Err(fail("CORRUPT_DATA", "object path"));
            }
            let bytes = self
                .encoded(hash)?
                .ok_or_else(|| fail("CORRUPT_DATA", "history transition object is missing"))?;
            entries.insert(path.to_string(), bytes);
            filled.insert(path.to_string());
        }
        let objects = entries
            .iter()
            .filter(|(k, _)| k.starts_with("objects/") && !filled.contains(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>();
        alto_store::decode_entries(entries.clone())?;
        let mut temp = Engine::new();
        temp.set_max_undo(self.max_undo)?;
        let (other, w) = temp.open_recovery_entries(entries, &BTreeSet::new())?;
        if !w.is_empty()
            || header["after"].as_str() != Some(temp.version_state_digest(other)?.hex().as_str())
            || temp.document(other)?.root != self.document(h)?.root
        {
            return Err(fail("CORRUPT_DATA", "invalid history transition"));
        }
        // Validate completely in a separate engine before changing the active document.
        for (name, bytes) in objects {
            let hash = name
                .rsplit('/')
                .next()
                .unwrap()
                .parse()
                .map_err(|_| fail("CORRUPT_DATA", "object path"))?;
            self.install_object(hash, &bytes)?;
        }
        let mut editor = temp.documents.remove(&other).unwrap();
        editor.doc.revision = revision;
        let versions = temp.versions.remove(&other).unwrap();
        self.install_editor(h, editor);
        self.versions.insert(h, versions);
        self.maintain();
        Ok(crate::Replayed::Applied)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use alto_doc::{DocumentMeta, Op};
    #[test]
    fn history_transition_replays_and_rejects_wrong_base() {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(16, 16)).unwrap();
        let initial = e.save_package(h).unwrap();
        let before = e.version_state_digest(h).unwrap();
        let c = e.commit(h, "first", 1, None, false).unwrap();
        let commit = e.version_record(h, before).unwrap();
        let before = e.version_state_digest(h).unwrap();
        e.create_branch(h, "feature", None, true).unwrap();
        let branch = e.version_record(h, before).unwrap();
        let mut f = Engine::new();
        let (d, _) = f.open_package(&initial).unwrap();
        assert_eq!(
            f.replay_record(d, &branch).unwrap(),
            crate::Replayed::Diverged
        );
        assert_eq!(
            f.replay_record(d, &commit).unwrap(),
            crate::Replayed::Applied
        );
        assert_eq!(f.version_status(d).unwrap().head, Some(c.hash));
        assert_eq!(
            f.replay_record(d, &branch).unwrap(),
            crate::Replayed::Applied
        );
        assert_eq!(
            f.version_state_digest(d).unwrap(),
            e.version_state_digest(h).unwrap()
        );
        assert_eq!(
            f.version_refs(d).unwrap().current_branch.as_deref(),
            Some("feature")
        );
    }

    /// A document with one painted raster layer: a commit's record would carry the tile.
    fn painted() -> (Engine, DocHandle, ContentHash) {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(256, 256)).unwrap();
        let node = alto_doc::Node::raster("layer");
        let layer = node.id;
        let parent = e.document(h).unwrap().root;
        e.transact(
            h,
            "layer",
            vec![Op::InsertNode {
                node,
                parent,
                index: 0,
            }],
            0,
        )
        .unwrap();
        // Noise, so the tile does not compress and its absence shows in the size.
        let mut x = 0x9e37_79b9u32;
        let mut pixels = vec![0u8; 262144];
        for b in pixels.iter_mut() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8 | 1;
        }
        let tile = e
            .put_tile(alto_raster::TileData::new(alto_core::PixelFormat::Rgba8, pixels).unwrap())
            .unwrap();
        let revision = e.document(h).unwrap().revision;
        e.transact(
            h,
            "paint",
            vec![Op::SetTiles {
                layer,
                changes: vec![alto_doc::TileChange {
                    key: alto_core::TileKey { tx: 0, ty: 0 },
                    before: None,
                    after: Some(tile),
                }],
            }],
            revision,
        )
        .unwrap();
        (e, h, tile)
    }

    #[test]
    fn an_incremental_commit_record_leaves_out_what_the_state_before_holds_and_replays() {
        let (mut e, h, tile) = painted();
        let initial = e.save_package(h).unwrap();
        let before = e.capture_version_state(h).unwrap();
        let c = e.commit(h, "first", 1, None, false).unwrap();
        let full = e.version_record(h, before.digest).unwrap();
        let incremental = e.version_record_since(h, &before).unwrap();
        e.release_version_state(before);
        let path = alto_store::object_path(&tile);
        assert!(alto_store::read_entries(&full).unwrap().contains_key(&path));
        assert!(
            !alto_store::read_entries(&incremental)
                .unwrap()
                .contains_key(&path)
        );
        assert!(
            incremental.len() * 10 < full.len(),
            "{} vs {}",
            incremental.len(),
            full.len()
        );

        let mut f = Engine::new();
        let (d, _) = f.open_package(&initial).unwrap();
        assert_eq!(
            f.replay_record(d, &incremental).unwrap(),
            crate::Replayed::Applied
        );
        assert_eq!(f.version_status(d).unwrap().head, Some(c.hash));
        assert_eq!(
            f.version_state_digest(d).unwrap(),
            e.version_state_digest(h).unwrap()
        );
        // The commit retains the tile, so it survives on the replaying side too.
        assert!(f.encoded(tile).unwrap().is_some());
    }

    #[test]
    fn an_incremental_record_is_refused_where_a_listed_object_is_missing() {
        let (mut e, h, _) = painted();
        let initial = e.save_package(h).unwrap();
        let before = e.capture_version_state(h).unwrap();
        e.commit(h, "first", 1, None, false).unwrap();
        let record = e.version_record_since(h, &before).unwrap();
        e.release_version_state(before);
        // List an object that neither the record nor the replaying engine holds.
        let mut entries = alto_store::read_entries(&record).unwrap();
        let mut meta: serde_json::Value =
            serde_json::from_slice(&entries["checkpoint.json"]).unwrap();
        let absent = ContentHash::of(b"not stored anywhere");
        meta["objects"]
            .as_array_mut()
            .unwrap()
            .push(alto_store::object_path(&absent).into());
        entries.insert("checkpoint.json".into(), serde_json::to_vec(&meta).unwrap());
        let tampered = alto_store::write_entries(&entries).unwrap();
        let mut f = Engine::new();
        let (d, _) = f.open_package(&initial).unwrap();
        let digest = f.version_state_digest(d).unwrap();
        assert!(f.replay_record(d, &tampered).is_err());
        assert_eq!(
            f.version_state_digest(d).unwrap(),
            digest,
            "a refused record changed the state"
        );
    }

    #[test]
    fn an_incremental_record_replays_onto_a_package_that_holds_an_earlier_version() {
        let (mut e, h, _) = painted();
        e.commit(h, "first", 1, None, false).unwrap();
        // Paint over the tile: the first version alone keeps the old one.
        let layer = e
            .document(h)
            .unwrap()
            .nodes
            .values()
            .find(|n| matches!(n.kind, alto_doc::NodeKind::Raster { .. }))
            .unwrap()
            .id;
        let old = match &e.document(h).unwrap().node(layer).unwrap().kind {
            alto_doc::NodeKind::Raster { tiles, .. } => tiles.tiles.values().next().copied(),
            _ => unreachable!(),
        };
        let tile = e
            .put_tile(
                alto_raster::TileData::new(alto_core::PixelFormat::Rgba8, vec![9u8; 262144])
                    .unwrap(),
            )
            .unwrap();
        let revision = e.document(h).unwrap().revision;
        e.transact(
            h,
            "repaint",
            vec![Op::SetTiles {
                layer,
                changes: vec![alto_doc::TileChange {
                    key: alto_core::TileKey { tx: 0, ty: 0 },
                    before: old,
                    after: Some(tile),
                }],
            }],
            revision,
        )
        .unwrap();
        let saved = e.save_package(h).unwrap();
        let before = e.capture_version_state(h).unwrap();
        let c = e.commit(h, "second", 2, None, false).unwrap();
        let record = e.version_record_since(h, &before).unwrap();
        e.release_version_state(before);
        let entries = alto_store::read_entries(&record).unwrap();
        assert!(!entries.contains_key(&alto_store::object_path(&old.unwrap())));
        assert!(!entries.contains_key(&alto_store::object_path(&tile)));

        let mut f = Engine::new();
        let (d, _) = f.open_package(&saved).unwrap();
        assert_eq!(
            f.replay_record(d, &record).unwrap(),
            crate::Replayed::Applied
        );
        assert_eq!(f.version_status(d).unwrap().head, Some(c.hash));
        assert_eq!(
            f.version_state_digest(d).unwrap(),
            e.version_state_digest(h).unwrap()
        );
        assert!(f.encoded(old.unwrap()).unwrap().is_some());
    }

    #[test]
    fn a_complete_record_missing_an_object_is_not_filled_in() {
        let (mut e, h, tile) = painted();
        let initial = e.save_package(h).unwrap();
        let before = e.version_state_digest(h).unwrap();
        e.commit(h, "first", 1, None, false).unwrap();
        let mut entries = alto_store::read_entries(&e.version_record(h, before).unwrap()).unwrap();
        entries.remove(&alto_store::object_path(&tile));
        let truncated = alto_store::write_entries(&entries).unwrap();
        let mut f = Engine::new();
        let (d, _) = f.open_package(&initial).unwrap();
        assert!(f.replay_record(d, &truncated).is_err());
    }
}
