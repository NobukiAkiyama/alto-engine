//! Versions, folder packages and recovery checkpoints at the WASM boundary.
use crate::{Runtime, clock, error};
use alto_core::ContentHash;
use alto_engine::versions::ObjectTarget;
use alto_engine::{Engine, JournalKind, VersionRef};
use serde_json::json;
use std::collections::BTreeMap;
use wasm_bindgen::prelude::*;

fn version(r: &str) -> Result<VersionRef, JsValue> {
    if r == "working" {
        Ok(VersionRef::Working)
    } else {
        Ok(VersionRef::Commit(r.parse::<ContentHash>().map_err(
            |_| error("INVALID_ARGUMENT: version must be \"working\" or a commit hash"),
        )?))
    }
}

/// Entries framed as u32 count, then (u32 name length, name, u32 data length, data).
pub(crate) fn frame(entries: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut out = (entries.len() as u32).to_le_bytes().to_vec();
    for (k, v) in entries {
        out.extend((k.len() as u32).to_le_bytes());
        out.extend(k.as_bytes());
        out.extend((v.len() as u32).to_le_bytes());
        out.extend(v);
    }
    out
}
pub(crate) fn unframe(b: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, JsValue> {
    let bad = || error("CORRUPT_DATA: framed entries");
    let mut p = 0usize;
    let u32_at = |p: &mut usize| -> Result<usize, JsValue> {
        let v = b.get(*p..*p + 4).ok_or_else(bad)?;
        *p += 4;
        Ok(u32::from_le_bytes(v.try_into().map_err(|_| bad())?) as usize)
    };
    let n = u32_at(&mut p)?;
    if n > 2_000_000 {
        return Err(bad());
    }
    let mut out = BTreeMap::new();
    for _ in 0..n {
        let l = u32_at(&mut p)?;
        let name = std::str::from_utf8(b.get(p..p + l).ok_or_else(bad)?)
            .map_err(|_| bad())?
            .to_string();
        p += l;
        let l = u32_at(&mut p)?;
        let data = b.get(p..p + l).ok_or_else(bad)?.to_vec();
        p += l;
        if name.contains("..") || name.starts_with('/') {
            return Err(bad());
        }
        out.insert(name, data);
    }
    Ok(out)
}

#[wasm_bindgen]
impl Runtime {
    pub fn commit(
        &mut self,
        message: &str,
        created_at: f64,
        author: &str,
        allow_empty: bool,
    ) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("CONFLICT: finish active tool first"));
        }
        if !created_at.is_finite() {
            return Err(error("INVALID_ARGUMENT: time"));
        }
        let author = (!author.is_empty()).then(|| author.to_string());
        let checkpoint = self.engine.capture_version_state(self.doc).map_err(error)?;
        let outcome = (|| {
            let c = self
                .engine
                .commit(self.doc, message, created_at as i64, author, allow_empty)
                .map_err(error)?;
            let result = serde_json::to_string(&c).map_err(error)?;
            self.journal.push(
                self.engine
                    .version_record_since(self.doc, &checkpoint)
                    .map_err(error)?,
            );
            Ok(result)
        })();
        match outcome {
            Ok(v) => {
                self.engine.release_version_state(checkpoint);
                Ok(v)
            }
            Err(e) => {
                self.engine.rollback_version_state(self.doc, checkpoint);
                Err(e)
            }
        }
    }
    #[wasm_bindgen(js_name=versionLog)]
    pub fn version_log(&self, dag: bool, all_branches: bool) -> Result<String, JsValue> {
        if all_branches && !dag {
            return Err(error("INVALID_ARGUMENT: allBranches requires DAG mode"));
        }
        let log = if dag {
            self.engine.log_dag(self.doc, all_branches)
        } else {
            self.engine.log(self.doc)
        }
        .map_err(error)?;
        serde_json::to_string(&log).map_err(error)
    }
    #[wasm_bindgen(js_name=versionDiff)]
    pub fn version_diff(&self, from: &str, to: &str) -> Result<String, JsValue> {
        let (a, b) = (version(from)?, version(to)?);
        let items = self.engine.diff_versions(self.doc, a, b).map_err(error)?;
        let names = self.engine.diff_names(self.doc, a, b).map_err(error)?;
        serde_json::to_string(&json!({"items":items,"names":names})).map_err(error)
    }
    #[wasm_bindgen(js_name=pruneVersions)]
    pub fn prune_versions(&mut self) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("CONFLICT: finish active tool first"));
        }
        let removed = self.engine.prune_versions(self.doc).map_err(error)?;
        let mut value: serde_json::Value = serde_json::from_str(&self.view()?).map_err(error)?;
        value["removed"] = json!(removed);
        serde_json::to_string(&value).map_err(error)
    }
    #[wasm_bindgen(js_name=releaseImport)]
    pub fn release_import(&mut self, hash: &str) -> Result<bool, JsValue> {
        let hash = hash.parse().map_err(|_| error("INVALID_ARGUMENT: hash"))?;
        Ok(self.engine.release_import(hash))
    }
    #[wasm_bindgen(js_name=restoreVersion)]
    pub fn restore_version(&mut self, hash: &str) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("CONFLICT: finish active tool first"));
        }
        let hash: ContentHash = hash
            .parse()
            .map_err(|_| error("INVALID_ARGUMENT: commit hash"))?;
        let (ops, intent) = self.engine.restore_version(self.doc, hash).map_err(error)?;
        self.journal.push(
            self.engine
                .journal_record(self.doc, JournalKind::Transact, intent, &ops)
                .map_err(error)?,
        );
        self.refresh()?;
        self.view()
    }
    #[wasm_bindgen(js_name=folderEntries)]
    pub fn folder_entries(&self) -> Result<Vec<u8>, JsValue> {
        if self.sampler.is_some() {
            return Err(error("CONFLICT: active tool must be flushed"));
        }
        Ok(frame(&self.engine.folder_entries(self.doc).map_err(error)?))
    }
    /// Entries for the folder the document came from, without reading what stays there.
    #[wasm_bindgen(js_name=folderEntriesKeepingExternal)]
    pub fn folder_entries_keeping_external(&self) -> Result<Vec<u8>, JsValue> {
        if self.sampler.is_some() {
            return Err(error("CONFLICT: active tool must be flushed"));
        }
        Ok(frame(
            &self
                .engine
                .folder_entries_keeping_external(self.doc)
                .map_err(error)?,
        ))
    }
    #[wasm_bindgen(js_name=folderPlanOnDemand)]
    pub fn folder_plan_on_demand(&self, framed: &[u8]) -> Result<String, JsValue> {
        serde_json::to_string(&Engine::folder_plan_on_demand(unframe(framed)?).map_err(error)?)
            .map_err(error)
    }
    /// `known` is a JSON list of the object paths in the folder.
    #[wasm_bindgen(js_name=openFolderOnDemand)]
    pub fn open_folder_on_demand(&mut self, framed: &[u8], known: &str) -> Result<String, JsValue> {
        let paths: Vec<String> = serde_json::from_str(known).map_err(error)?;
        let known = paths
            .iter()
            .filter_map(|p| p.rsplit('/').next()?.parse().ok())
            .collect();
        let mut engine = self.fresh_engine()?;
        let (doc, warnings) = engine
            .open_entries_on_demand(unframe(framed)?, known)
            .map_err(error)?;
        self.install(engine, doc, warnings)
    }
    #[wasm_bindgen(js_name=missingObjects)]
    pub fn missing_objects(&self, target: &str) -> Result<String, JsValue> {
        let target: ObjectTarget =
            serde_json::from_str(target).map_err(|e| error(format!("INVALID_ARGUMENT: {e}")))?;
        serde_json::to_string(
            &self
                .engine
                .missing_objects(self.doc, &target)
                .map_err(error)?,
        )
        .map_err(error)
    }
    #[wasm_bindgen(js_name=provideObjects)]
    pub fn provide_objects(&mut self, framed: &[u8], target: &str) -> Result<(), JsValue> {
        let target: ObjectTarget =
            serde_json::from_str(target).map_err(|e| error(format!("INVALID_ARGUMENT: {e}")))?;
        self.engine
            .provide_objects(self.doc, unframe(framed)?, &target)
            .map_err(error)
    }
    #[wasm_bindgen(js_name=releaseObjects)]
    pub fn release_objects(&mut self) {
        self.engine.release_objects(self.doc);
    }
    #[wasm_bindgen(js_name=markFolderSaved)]
    pub fn mark_folder_saved(&mut self) -> Result<(), JsValue> {
        self.engine.mark_folder_saved(self.doc).map_err(error)
    }
    /// After a thin save's store write: the versions' objects stay in the store, even for
    /// a document that held every object until now.
    #[wasm_bindgen(js_name=markStoreSaved)]
    pub fn mark_store_saved(&mut self) -> Result<(), JsValue> {
        self.engine.mark_store_saved(self.doc).map_err(error)
    }
    #[wasm_bindgen(js_name=externalObjects)]
    pub fn external_objects(&self) -> Result<u32, JsValue> {
        Ok(self.engine.external_objects(self.doc).map_err(error)?.len() as u32)
    }
    /// Whether the document came from a thin .alto, whose versions belong in a store.
    #[wasm_bindgen(js_name=historyIsExternal)]
    pub fn history_is_external(&self) -> bool {
        self.engine.history_is_external(self.doc)
    }
    /// Whether a history store serves this document. `framed` holds the store's
    /// `manifest.json` and the snapshot it names; `objects` is a JSON list of its object
    /// paths. With `warn`, a mismatch becomes a `HISTORY_STORE_UNAVAILABLE` warning; a
    /// store that serves the document clears that warning. `contained`: whether every root
    /// of the store's manifest is one of this document's versions.
    #[wasm_bindgen(js_name=checkHistoryStore)]
    pub fn check_history_store(
        &mut self,
        framed: &[u8],
        objects: &str,
        warn: bool,
    ) -> Result<String, JsValue> {
        let paths: Vec<String> = serde_json::from_str(objects).map_err(error)?;
        let present = paths
            .iter()
            .filter_map(|p| p.rsplit('/').next()?.parse().ok())
            .collect();
        let entries = unframe(framed)?;
        let manifest = entries.get("manifest.json").cloned().unwrap_or_default();
        let mismatch = self
            .engine
            .check_history_store(self.doc, entries, &present)
            .map_err(error)?;
        let contained = self
            .engine
            .store_roots_contained(self.doc, &manifest)
            .map_err(error)?;
        match mismatch {
            Some(m) if warn => self.warnings.push(
                alto_engine::Warning::new(alto_engine::HISTORY_STORE_UNAVAILABLE)
                    .with("reason", m.reason()),
            ),
            Some(_) => {}
            None => self
                .warnings
                .retain(|w| w.code != alto_engine::HISTORY_STORE_UNAVAILABLE),
        }
        let state: serde_json::Value = serde_json::from_str(&self.view()?).map_err(error)?;
        serde_json::to_string(&json!({
            "mismatch": mismatch.map(|m| m.reason()),
            "contained": contained,
            "state": state,
        }))
        .map_err(error)
    }
    /// A history store was attached after all: its `HISTORY_STORE_UNAVAILABLE` warning goes.
    #[wasm_bindgen(js_name=historyStoreAttached)]
    pub fn history_store_attached(&mut self) -> Result<String, JsValue> {
        self.warnings
            .retain(|w| w.code != alto_engine::HISTORY_STORE_UNAVAILABLE);
        self.view()
    }
    /// Whether every root of a store's `manifest.json` is one of this document's versions.
    #[wasm_bindgen(js_name=storeRootsContained)]
    pub fn store_roots_contained(&self, manifest: &[u8]) -> Result<bool, JsValue> {
        self.engine
            .store_roots_contained(self.doc, manifest)
            .map_err(error)
    }
    /// The working document alone. A normal save fails rather than dropping history.
    #[wasm_bindgen(js_name=saveWithoutHistory)]
    pub fn save_without_history(&self) -> Result<Vec<u8>, JsValue> {
        self.engine.save_without_history(self.doc).map_err(error)
    }
    /// A .alto holding the working document's objects; versions' objects stay in the
    /// folder it was just saved to.
    #[wasm_bindgen(js_name=saveThinPackage)]
    pub fn save_thin_package(&mut self) -> Result<Vec<u8>, JsValue> {
        if self.sampler.is_some() {
            return Err(error("CONFLICT: active tool must be flushed"));
        }
        self.engine.save_thin_package(self.doc).map_err(error)
    }
    /// `packageBytes` for the thin .alto. Known without reading anything.
    #[wasm_bindgen(js_name=thinPackageBytes)]
    pub fn thin_package_bytes(&self) -> Result<f64, JsValue> {
        Ok(self.engine.thin_package_bytes(self.doc).map_err(error)? as f64)
    }
    #[wasm_bindgen(js_name=folderPlan)]
    pub fn folder_plan(&self, framed: &[u8]) -> Result<String, JsValue> {
        serde_json::to_string(&Engine::folder_plan(unframe(framed)?).map_err(error)?).map_err(error)
    }
    #[wasm_bindgen(js_name=openFolder)]
    pub fn open_folder(&mut self, framed: &[u8]) -> Result<String, JsValue> {
        let mut engine = self.fresh_engine()?;
        let (doc, warnings) = engine.open_entries(unframe(framed)?).map_err(error)?;
        self.install(engine, doc, warnings)
    }
    /// A recovery checkpoint as framed entries: the package without the objects
    /// already stored, plus `checkpoint.json` naming every object it references and
    /// the opaque ones. The returned objects are counted as stored from now on: if
    /// writing them fails, storage stops accepting writes altogether.
    #[wasm_bindgen(js_name=checkpointParts)]
    pub fn checkpoint_parts(&mut self) -> Result<Vec<u8>, JsValue> {
        let checkpoint = self
            .engine
            .recovery_checkpoint(self.doc, &self.stored_objects)
            .map_err(error)?;
        self.stored_objects.extend(checkpoint.written);
        // The store may drop what this checkpoint no longer lists.
        self.stored_objects
            .retain(|hash| !checkpoint.external.contains(hash));
        Ok(frame(&checkpoint.entries))
    }
    /// Object paths (`objects/ab/<hash>`) the recovery store already holds.
    #[wasm_bindgen(js_name=noteStoredObjects)]
    pub fn note_stored_objects(&mut self, paths: &str) -> Result<(), JsValue> {
        let paths: Vec<String> = serde_json::from_str(paths).map_err(error)?;
        for p in paths {
            if let Some(h) = p.rsplit('/').next().and_then(|h| h.parse().ok()) {
                self.stored_objects.insert(h);
            }
        }
        Ok(())
    }
    /// Object paths the recovery store deleted: the next checkpoint that references
    /// one writes it again.
    #[wasm_bindgen(js_name=forgetStoredObjectPaths)]
    pub fn forget_stored_object_paths(&mut self, paths: &str) -> Result<(), JsValue> {
        let paths: Vec<String> = serde_json::from_str(paths).map_err(error)?;
        // An object kept as stored by mistake would be left out of the next checkpoint,
        // which the store then refuses; a path that names no object is an error here.
        let mut hashes = Vec::with_capacity(paths.len());
        for p in &paths {
            let hash = p
                .strip_prefix("objects/")
                .and_then(|rest| rest.rsplit('/').next())
                .and_then(|h| h.parse::<ContentHash>().ok())
                .filter(|h| alto_engine::object_path(h) == *p)
                .ok_or_else(|| error(format!("INVALID_ARGUMENT: not an object path: {p}")))?;
            hashes.push(hash);
        }
        for hash in hashes {
            self.stored_objects.remove(&hash);
        }
        Ok(())
    }
    /// The recovery store was emptied (its journal discarded); the next checkpoint
    /// writes every object again.
    #[wasm_bindgen(js_name=forgetStoredObjects)]
    pub fn forget_stored_objects(&mut self) {
        self.stored_objects.clear();
    }
    /// Only called by the journal worker after verifying the checkpoint digest.
    /// With `timed`, returns the verify/restore/register milliseconds in `stages`.
    #[wasm_bindgen(js_name=openRecoveryEntries)]
    pub fn open_recovery_entries(
        &mut self,
        framed: &[u8],
        opaque: &str,
        timed: Option<bool>,
    ) -> Result<String, JsValue> {
        let opaque: std::collections::BTreeSet<ContentHash> =
            serde_json::from_str(opaque).map_err(error)?;
        let mut engine = self.fresh_engine()?;
        let timed = timed.unwrap_or(false);
        let clock = &clock as &dyn Fn() -> f64;
        let (doc, warnings, stages) = engine
            .open_recovery_entries_with_clock(unframe(framed)?, &opaque, timed.then_some(clock))
            .map_err(error)?;
        if timed {
            // The recovery caller reads only the stages; skip building the full view.
            self.adopt_engine(engine, doc, warnings)?;
            serde_json::to_string(&json!({ "stages": stages })).map_err(error)
        } else {
            self.install(engine, doc, warnings)
        }
    }
    /// The canonical .alto bytes for a folder, used as the recovery checkpoint.
    #[wasm_bindgen(js_name=checkpointBytes)]
    pub fn checkpoint_bytes(&mut self) -> Result<Vec<u8>, JsValue> {
        self.engine.save_package(self.doc).map_err(error)
    }
}
