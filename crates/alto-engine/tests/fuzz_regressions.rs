//! Replays every input the fuzzers found (`tests/fixtures/fuzz/<target>/`) through
//! the same entry points as `fuzz/fuzz_targets/<target>.rs`. Each input once made
//! its target panic; returning an error is the expected outcome now.
use alto_doc::DocumentMeta;
use alto_engine::Engine;
use std::path::Path;
use std::sync::Arc;

static SAMPLE: &[u8] = include_bytes!("../../../verification/sample-m6.alto");

fn run(target: &str, data: &[u8]) {
    match target {
        "package" => {
            let _ = alto_store::decode_package(data);
            let _ = Engine::new().open_package(data);
        }
        "recovery" => {
            if let Ok(entries) = alto_store::read_entries(data) {
                let _ = alto_store::decode_recovery_entries(entries, &Default::default());
            }
        }
        "snapshot" => {
            let _ = Engine::new().open_snapshot(data);
        }
        "commit" => {
            let _ = alto_doc::Commit::parse(data);
            let _ = alto_doc::Commit::parse_history(data);
        }
        "svg" => {
            let Ok(svg) = std::str::from_utf8(data) else {
                return;
            };
            let mut engine = Engine::new();
            let h = engine
                .create_document(DocumentMeta::srgb_u8(256, 256))
                .unwrap();
            let root = engine.document(h).unwrap().root;
            let _ = engine.prepare_svg_import(h, svg, root, 0);
        }
        "font" => {
            let _ = alto_text::inspect(data, 0);
            let id = alto_core::AssetId(Default::default());
            let _ = alto_text::FontData::new(id, Arc::new(data.to_vec()), 0);
        }
        "path" => {
            let Ok(d) = std::str::from_utf8(data) else {
                return;
            };
            let _ = alto_vector::PathData::from_svg(d);
        }
        "journal" => {
            let mut engine = Engine::new();
            let (h, _) = engine.open_package(SAMPLE).unwrap();
            let _ = engine.replay_record(h, data);
        }
        other => panic!("tests/fixtures/fuzz/{other}: no such fuzz target"),
    }
}

#[test]
fn fuzz_regressions_do_not_panic() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fuzz");
    let mut replayed = 0;
    for dir in std::fs::read_dir(&root).unwrap() {
        let dir = dir.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        let target = dir.file_name().unwrap().to_str().unwrap().to_owned();
        for input in std::fs::read_dir(&dir).unwrap() {
            let input = input.unwrap().path();
            let data = std::fs::read(&input).unwrap();
            let caught = std::panic::catch_unwind(|| run(&target, &data));
            assert!(caught.is_ok(), "{} panicked", input.display());
            replayed += 1;
        }
    }
    assert!(
        replayed > 0,
        "no fuzz regression inputs under {}",
        root.display()
    );
}
