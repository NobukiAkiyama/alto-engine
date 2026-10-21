//! M7 text / fonts and M8 versions / folder packages at the WASM boundary.
use crate::{Runtime, error};
use alto_core::{ContentHash, NodeId};
use alto_engine::{Engine, VersionRef};
use serde_json::json;
use std::collections::BTreeMap;
use wasm_bindgen::prelude::*;

fn node(id: &str) -> Result<NodeId, JsValue> {
    serde_json::from_str(&format!("\"{id}\"")).map_err(|_| error("INVALID_ARGUMENT: node id"))
}
fn version(r: &str) -> Result<VersionRef, JsValue> {
    if r == "working" {
        Ok(VersionRef::Working)
    } else {
        Ok(VersionRef::Commit(r.parse::<ContentHash>().map_err(
            |_| error("INVALID_ARGUMENT: version must be \"working\" or a commit hash"),
        )?))
    }
}
fn finite(p: &[f64]) -> Result<(), JsValue> {
    if p.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(error("INVALID_ARGUMENT: coordinates"))
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
    #[wasm_bindgen(js_name=importFont)]
    pub fn import_font(&mut self, bytes: &[u8], face_index: u32) -> Result<String, JsValue> {
        let f = self
            .engine
            .import_font(bytes.to_vec(), face_index)
            .map_err(error)?;
        serde_json::to_string(&f).map_err(error)
    }
    #[wasm_bindgen(js_name=textInfo)]
    pub fn text_info(&self, id: &str) -> Result<String, JsValue> {
        serde_json::to_string(&self.engine.text_info(self.doc, node(id)?).map_err(error)?)
            .map_err(error)
    }
    #[wasm_bindgen(js_name=textHitTest)]
    pub fn text_hit_test(&self, id: &str, x: f64, y: f64) -> Result<u32, JsValue> {
        finite(&[x, y])?;
        self.engine
            .text_hit_test(self.doc, node(id)?, [x, y])
            .map_err(error)
    }
    #[wasm_bindgen(js_name=textCaret)]
    pub fn text_caret(&self, id: &str, offset: u32) -> Result<String, JsValue> {
        serde_json::to_string(
            &self
                .engine
                .text_caret(self.doc, node(id)?, offset)
                .map_err(error)?,
        )
        .map_err(error)
    }
    #[wasm_bindgen(js_name=textSelection)]
    pub fn text_selection(&self, id: &str, start: u32, end: u32) -> Result<String, JsValue> {
        serde_json::to_string(
            &self
                .engine
                .text_selection(self.doc, node(id)?, start, end)
                .map_err(error)?,
        )
        .map_err(error)
    }
    #[wasm_bindgen(js_name=textNavigate)]
    pub fn text_navigate(&self, id: &str, offset: u32, motion: &str) -> Result<u32, JsValue> {
        let motion = serde_json::from_str(&format!("\"{motion}\""))
            .map_err(|_| error("INVALID_ARGUMENT: motion"))?;
        self.engine
            .text_navigate(self.doc, node(id)?, offset, motion)
            .map_err(error)
    }
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
        let c = self
            .engine
            .commit(self.doc, message, created_at as i64, author, allow_empty)
            .map_err(error)?;
        serde_json::to_string(&c).map_err(error)
    }
    #[wasm_bindgen(js_name=versionLog)]
    pub fn version_log(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.engine.log(self.doc).map_err(error)?).map_err(error)
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
        let ops = self.engine.restore_version(self.doc, hash).map_err(error)?;
        let label = format!("版を復元: {}", &hash.hex()[..10]);
        self.journal
            .push(self.engine.journal_record(&label, &ops).map_err(error)?);
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
    #[wasm_bindgen(js_name=folderPlan)]
    pub fn folder_plan(&self, framed: &[u8]) -> Result<String, JsValue> {
        serde_json::to_string(&Engine::folder_plan(unframe(framed)?).map_err(error)?).map_err(error)
    }
    #[wasm_bindgen(js_name=openFolder)]
    pub fn open_folder(&mut self, framed: &[u8]) -> Result<String, JsValue> {
        let mut engine = Engine::new();
        let (doc, warnings) = engine.open_entries(unframe(framed)?).map_err(error)?;
        self.install(engine, doc, warnings)
    }
    /// The canonical .alto bytes for a folder, used as the recovery checkpoint.
    #[wasm_bindgen(js_name=checkpointBytes)]
    pub fn checkpoint_bytes(&mut self) -> Result<Vec<u8>, JsValue> {
        self.engine.save_package(self.doc).map_err(error)
    }
}
