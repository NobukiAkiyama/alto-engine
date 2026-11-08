#![no_main]
//! Commit JSON, both the strict parser and the one for commits already in a package.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = alto_doc::Commit::parse(data);
    let _ = alto_doc::Commit::parse_history(data);
});
