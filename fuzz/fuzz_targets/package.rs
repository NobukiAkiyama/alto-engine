#![no_main]
//! `.alto` packages: the ZIP reader, the logical entries and opening them in an engine.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = alto_store::decode_package(data);
    let _ = alto_engine::Engine::new().open_package(data);
});
