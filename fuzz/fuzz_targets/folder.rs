#![no_main]
//! Open mutated logical folder entries, with eager and on-demand object loading.
use alto_core::ContentHash;
use alto_engine::Engine;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(entries) = alto_store::read_entries(data) else {
        return;
    };
    let known = entries
        .keys()
        .filter_map(|name| {
            let (_, hash) = name.strip_prefix("objects/")?.split_once('/')?;
            hash.parse::<ContentHash>().ok()
        })
        .collect();
    let _ = Engine::folder_plan(entries.clone());
    let _ = Engine::new().open_entries(entries.clone());
    let _ = Engine::new().open_entries_on_demand(entries, known);
});
