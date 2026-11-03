use super::branches::fail;
use super::*;

/// A rollback point whose objects remain rooted while a history command is prepared.
pub struct VersionCheckpoint {
    doc: Document,
    history: alto_doc::History,
    versions: VersionStore,
    stage: u64,
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
        self.staged_tiles.insert(stage, hashes);
        Ok(VersionCheckpoint {
            doc,
            history,
            versions,
            stage,
            digest,
        })
    }
    pub fn release_version_state(&mut self, state: VersionCheckpoint) {
        self.release_staged_tiles(state.stage);
    }
    pub fn rollback_version_state(&mut self, h: DocHandle, state: VersionCheckpoint) {
        let mut editor = alto_doc::Editor::new(state.doc);
        editor.restore_history(state.history);
        self.documents.insert(h, editor);
        self.versions.insert(h, state.versions);
        self.release_staged_tiles(state.stage);
    }
    /// A self-contained history state transition. Normal edits still use compact Op records.
    pub fn version_record(
        &self,
        h: DocHandle,
        before: ContentHash,
    ) -> Result<Vec<u8>, EngineError> {
        let mut checkpoint = self.recovery_checkpoint(h, &BTreeSet::new())?.entries;
        checkpoint.insert("version-state.json".into(),serde_json::to_vec(&serde_json::json!({
            "before":before,"after":self.version_state_digest(h)?,"revision":self.document(h)?.revision
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
        let objects = entries
            .iter()
            .filter(|(k, _)| k.starts_with("objects/"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>();
        alto_store::decode_entries(entries.clone())?;
        let mut temp = Engine::new();
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
        self.documents.insert(h, editor);
        self.versions.insert(h, versions);
        self.maintain();
        Ok(crate::Replayed::Applied)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use alto_doc::DocumentMeta;
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
}
