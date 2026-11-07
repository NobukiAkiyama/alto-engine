//! Text and font calls at the WASM boundary.
use crate::{Runtime, error};
use alto_core::NodeId;
use wasm_bindgen::prelude::*;

fn node(id: &str) -> Result<NodeId, JsValue> {
    serde_json::from_str(&format!("\"{id}\"")).map_err(|_| error("INVALID_ARGUMENT: node id"))
}
fn finite(p: &[f64]) -> Result<(), JsValue> {
    if p.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(error("INVALID_ARGUMENT: coordinates"))
    }
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
    /// A text node's render version 3 bake for tests: `{ hash, hex }`, both null
    /// when the node has none. The JS reference reads exactly these bytes.
    #[wasm_bindgen(js_name=textBakeForTesting)]
    pub fn text_bake_for_testing(&self, id: &str) -> Result<String, JsValue> {
        let bake = self.engine.text_bake(self.doc, node(id)?).map_err(error)?;
        let (hash, hex) = match bake {
            Some((hash, bytes)) => (
                serde_json::Value::String(hash.to_string()),
                serde_json::Value::String(bytes.iter().map(|b| format!("{b:02x}")).collect()),
            ),
            None => (serde_json::Value::Null, serde_json::Value::Null),
        };
        serde_json::to_string(&serde_json::json!({ "hash": hash, "hex": hex })).map_err(error)
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
}
