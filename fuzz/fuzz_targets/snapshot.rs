#![no_main]
//! Snapshot JSON (`document.json` and the snapshots of a package).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = alto_engine::Engine::new().open_snapshot(data);
});
