use crate::{Runtime, error};
use alto_store::tiff::{TiffCompression, TiffStrip, rgba8_header};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
impl Runtime {
    /// Builds the TIFF metadata without transferring any pixels into WASM.
    #[wasm_bindgen(js_name = tiffHeader)]
    pub fn tiff_header(
        &self,
        width: u32,
        height: u32,
        compression: &str,
        strips_json: &str,
    ) -> Result<Vec<u8>, JsValue> {
        let compression = match compression {
            "none" => TiffCompression::None,
            "deflate" => TiffCompression::Deflate,
            _ => return Err(error("INVALID_ARGUMENT: TIFF compression")),
        };
        let strips: Vec<TiffStrip> = serde_json::from_str(strips_json)
            .map_err(|e| error(format!("INVALID_ARGUMENT: TIFF strips: {e}")))?;
        rgba8_header(width, height, compression, &strips).map_err(error)
    }
}
