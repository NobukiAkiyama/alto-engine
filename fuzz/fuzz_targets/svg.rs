#![no_main]
//! SVG files imported as vector layers.
use alto_doc::DocumentMeta;
use alto_engine::Engine;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(svg) = std::str::from_utf8(data) else { return };
    let mut engine = Engine::new();
    let h = engine.create_document(DocumentMeta::srgb_u8(256, 256)).unwrap();
    let root = engine.document(h).unwrap().root;
    let _ = engine.prepare_svg_import(h, svg, root, 0);
});
