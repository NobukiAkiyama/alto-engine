#![no_main]
//! Recovery checkpoints, read with the trusted-checkpoint decoder.
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeSet;

fuzz_target!(|data: &[u8]| {
    if let Ok(entries) = alto_store::read_entries(data) {
        let _ = alto_store::decode_recovery_entries(entries, &BTreeSet::new());
    }
});
