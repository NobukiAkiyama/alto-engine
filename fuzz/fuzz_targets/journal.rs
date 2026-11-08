#![no_main]
//! One recovery journal record replayed onto a document.
//! The seeds in `seeds/journal` are records made against this same sample, so
//! node ids in them resolve and the fuzzer reaches the replay itself.
use alto_engine::Engine;
use libfuzzer_sys::fuzz_target;

static SAMPLE: &[u8] = include_bytes!("../../verification/sample-m6.alto");

fuzz_target!(|data: &[u8]| {
    let mut engine = Engine::new();
    let (h, _) = engine.open_package(SAMPLE).unwrap();
    let _ = engine.replay_record(h, data);
});
