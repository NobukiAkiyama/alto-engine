#![no_main]
//! Font files (OTF, TTF, TTC) added to a document.
use alto_text::{FontData, inspect};
use libfuzzer_sys::fuzz_target;
use std::sync::Arc;

fuzz_target!(|data: &[u8]| {
    let _ = inspect(data, 0);
    let _ = FontData::new(alto_core::AssetId(Default::default()), Arc::new(data.to_vec()), 0);
});
