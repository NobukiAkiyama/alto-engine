//! Commits, diffs, history residency and pruning.
use super::*;
use crate::text_tests::{add_text, setup, vertical};
use alto_core::{PixelFormat, TileKey};
use alto_doc::{DiffItem, Node, PropPatch, TileChange};

fn paint(e: &mut Engine, d: DocHandle, layer: alto_core::NodeId, key: TileKey, v: u8) {
    let hash = e
        .put_tile(TileData::new(PixelFormat::Rgba8, vec![v; 262144]).unwrap())
        .unwrap();
    let doc = e.document(d).unwrap();
    let NodeKind::Raster { tiles, .. } = &doc.node(layer).unwrap().kind else {
        panic!()
    };
    let before = tiles.tiles.get(&key).copied();
    let rev = doc.revision;
    e.transact(
        d,
        "ブラシ",
        vec![Op::SetTiles {
            layer,
            changes: vec![TileChange {
                key,
                before,
                after: Some(hash),
            }],
        }],
        rev,
    )
    .unwrap();
}

#[test]
fn commits_diff_restore_and_persist_history() {
    let (mut e, d, latin, jp) = setup();
    let bg = Node::raster("背景");
    let id = bg.id;
    let root = e.document(d).unwrap().root;
    let rev = e.document(d).unwrap().revision;
    e.transact(
        d,
        "レイヤー追加",
        vec![Op::InsertNode {
            node: bg,
            parent: root,
            index: 0,
        }],
        rev,
    )
    .unwrap();
    paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, 10);
    let c1 = e
        .commit(d, "下地", 1_000, Some("mimi".into()), false)
        .unwrap();
    assert!(
        e.commit(d, "空", 2_000, None, false).is_err(),
        "nothing changed"
    );
    assert!(!e.version_status(d).unwrap().changed);
    for v in [20, 30, 40] {
        paint(&mut e, d, id, TileKey { tx: 1, ty: 1 }, v);
    }
    let text = add_text(&mut e, d, vertical(vec![jp, latin]));
    let rev = e.document(d).unwrap().revision;
    e.transact(
        d,
        "不透明度",
        vec![Op::SetProps {
            id,
            patch: PropPatch {
                opacity: Some(0.5),
                ..Default::default()
            },
        }],
        rev,
    )
    .unwrap();
    let status = e.version_status(d).unwrap();
    assert!(status.changed && status.changes == 4);
    let c2 = e.commit(d, "人物と本文", 3_000, None, false).unwrap();
    let brush = c2.intents.iter().find(|i| i.intent == "ブラシ").unwrap();
    assert_eq!(
        (brush.count, brush.target_names.clone()),
        (3, vec![Some("背景".to_string())])
    );
    let log = e.log(d).unwrap();
    assert_eq!(
        log.iter().map(|c| c.hash).collect::<Vec<_>>(),
        vec![c2.hash, c1.hash]
    );
    assert_eq!(log[0].parents, vec![c1.hash]);
    let diff = e
        .diff_versions(d, VersionRef::Commit(c1.hash), VersionRef::Commit(c2.hash))
        .unwrap();
    assert!(
        diff.iter()
            .any(|i| matches!(i, DiffItem::PixelsChanged { id: n, tiles: 1, .. } if *n == id)),
        "{diff:?}"
    );
    assert!(diff.iter().any(|i| matches!(i, DiffItem::PropsChanged { id: n, fields, .. } if *n == id && fields == &["opacity"])));
    assert!(
        diff.iter().any(
            |i| matches!(i, DiffItem::Added { id: n, kind, .. } if *n == text && kind == "text")
        )
    );
    // Restore the first version as an undoable edit.
    e.restore_version(d, c1.hash).unwrap();
    assert!(
        e.diff_versions(d, VersionRef::Commit(c1.hash), VersionRef::Working)
            .unwrap()
            .is_empty()
    );
    e.undo(d).unwrap();
    assert!(
        e.diff_versions(d, VersionRef::Commit(c2.hash), VersionRef::Working)
            .unwrap()
            .is_empty()
    );
    // History survives the ZIP and the folder form in a fresh engine.
    let zip = e.save_package(d).unwrap();
    let mut fresh = Engine::new();
    let (f, warnings) = fresh.open_package(&zip).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        fresh
            .log(f)
            .unwrap()
            .iter()
            .map(|c| c.hash)
            .collect::<Vec<_>>(),
        vec![c2.hash, c1.hash]
    );
    fresh.restore_version(f, c1.hash).unwrap();
    let entries = e.folder_entries(d).unwrap();
    assert!(
        entries.contains_key("document.json")
            && entries.contains_key(&format!("commits/{}.json", c1.hash))
    );
    let without_objects: BTreeMap<_, _> = entries
        .iter()
        .filter(|(k, _)| !k.starts_with("objects/"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let plan = Engine::folder_plan(without_objects).unwrap();
    let objects: Vec<_> = entries
        .keys()
        .filter(|k| k.starts_with("objects/"))
        .cloned()
        .collect();
    assert_eq!(plan, objects, "plan names exactly the reachable objects");
    let mut folder = Engine::new();
    let (g, warnings) = folder.open_entries(entries).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(folder.snapshot(g).unwrap(), e.snapshot(d).unwrap());
    assert_eq!(folder.log(g).unwrap().len(), 2);
}

#[test]
fn committed_tiles_survive_garbage_collection() {
    let mut e = Engine::new();
    let d = e.create_document(DocumentMeta::srgb_u8(512, 512)).unwrap();
    let layer = Node::raster("A");
    let id = layer.id;
    let root = e.document(d).unwrap().root;
    e.transact(
        d,
        "add",
        vec![Op::InsertNode {
            node: layer,
            parent: root,
            index: 0,
        }],
        0,
    )
    .unwrap();
    paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, 77);
    let old = {
        let NodeKind::Raster { tiles, .. } = &e.document(d).unwrap().node(id).unwrap().kind else {
            panic!()
        };
        tiles.tiles[&TileKey { tx: 0, ty: 0 }]
    };
    let c = e.commit(d, "v1", 1, None, false).unwrap();
    // Overwrite the tile more times than the undo stack keeps.
    for v in 1..=110u8 {
        paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, v);
    }
    assert_eq!(e.history_state(d).unwrap().0, 100);
    assert!(
        e.tile(&old).unwrap().is_some(),
        "committed tile is a GC root"
    );
    e.restore_version(d, c.hash).unwrap();
    let NodeKind::Raster { tiles, .. } = &e.document(d).unwrap().node(id).unwrap().kind else {
        panic!()
    };
    assert_eq!(tiles.tiles[&TileKey { tx: 0, ty: 0 }], old);
}

#[test]
fn saved_history_tiles_leave_residency_and_fault_back() {
    let mut e = Engine::new();
    let d = e.create_document(DocumentMeta::srgb_u8(512, 512)).unwrap();
    let layer = Node::raster("A");
    let id = layer.id;
    let root = e.document(d).unwrap().root;
    e.transact(
        d,
        "add",
        vec![Op::InsertNode {
            node: layer,
            parent: root,
            index: 0,
        }],
        0,
    )
    .unwrap();
    paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, 250);
    let old = {
        let NodeKind::Raster { tiles, .. } = &e.document(d).unwrap().node(id).unwrap().kind else {
            panic!()
        };
        tiles.tiles[&TileKey { tx: 0, ty: 0 }]
    };
    let c = e.commit(d, "v1", 1, None, false).unwrap();
    for v in 1..=110u8 {
        paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, v);
    }
    assert!(e.tile_resident(&old), "unsaved history stays resident");
    let hot = e.resident_bytes();
    e.save_package(d).unwrap();
    assert!(!e.tile_resident(&old));
    assert!(e.resident_bytes() < hot);
    let restored = e.tile(&old).unwrap().unwrap();
    assert_eq!(restored.pixels[0], 250);
    e.restore_version(d, c.hash).unwrap();
    assert!(e.tile_resident(&old));
    let NodeKind::Raster { tiles, .. } = &e.document(d).unwrap().node(id).unwrap().kind else {
        panic!()
    };
    assert_eq!(tiles.tiles[&TileKey { tx: 0, ty: 0 }], old);
}

#[test]
fn parked_tile_with_a_bad_hash_is_corrupt() {
    let mut e = Engine::new();
    let d = e.create_document(DocumentMeta::srgb_u8(256, 256)).unwrap();
    let layer = Node::raster("A");
    let id = layer.id;
    let root = e.document(d).unwrap().root;
    e.transact(
        d,
        "add",
        vec![Op::InsertNode {
            node: layer,
            parent: root,
            index: 0,
        }],
        0,
    )
    .unwrap();
    paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, 3);
    let old = {
        let NodeKind::Raster { tiles, .. } = &e.document(d).unwrap().node(id).unwrap().kind else {
            panic!()
        };
        tiles.tiles[&TileKey { tx: 0, ty: 0 }]
    };
    e.commit(d, "v1", 1, None, false).unwrap();
    for v in 1..=110u8 {
        paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, v);
    }
    e.save_package(d).unwrap();
    assert!(e.tamper_parked_tile(&old));
    let err = e.require_tile(&old).unwrap_err().to_string();
    assert!(
        err.contains("CORRUPT_DATA") && err.contains(&old.to_string()),
        "{err}"
    );
    assert!(
        e.save_package(d)
            .unwrap_err()
            .to_string()
            .contains("CORRUPT_DATA")
    );
    // The damage stays attached to that object; it is not a pending error for the next read.
    assert!(e.tile(&ContentHash::of(b"absent")).unwrap().is_none());
    assert!(e.tile(&old).is_err());
}

#[test]
fn history_truncation_is_rejected_without_touching_open_history() {
    let mut e = Engine::new();
    let d = e.create_document(DocumentMeta::srgb_u8(256, 256)).unwrap();
    let layer = Node::raster("A");
    let id = layer.id;
    let root = e.document(d).unwrap().root;
    e.transact(
        d,
        "add",
        vec![Op::InsertNode {
            node: layer,
            parent: root,
            index: 0,
        }],
        0,
    )
    .unwrap();
    paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, 1);
    let c1 = e.commit(d, "v1", 1, None, false).unwrap();
    paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, 2);
    e.commit(d, "v2", 2, None, false).unwrap();
    paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, 3);
    let c3 = e.commit(d, "v3", 3, None, false).unwrap();
    assert!(
        e.save_package_trimmed(d, 0)
            .unwrap_err()
            .to_string()
            .starts_with("UNSUPPORTED:")
    );
    assert_eq!(e.log(d).unwrap().len(), 3);
    assert!(e.commit_view(d, c1.hash).is_ok());
    assert!(e.commit_view(d, c3.hash).is_ok());
}

#[test]
fn divergent_history_survives_commit_and_reopen_until_prune() {
    let mut e = Engine::new();
    let d = e.create_document(DocumentMeta::srgb_u8(256, 256)).unwrap();
    let layer = Node::raster("A");
    let id = layer.id;
    let root = e.document(d).unwrap().root;
    e.transact(
        d,
        "add",
        vec![Op::InsertNode {
            node: layer,
            parent: root,
            index: 0,
        }],
        0,
    )
    .unwrap();
    paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, 1);
    let a = e.commit(d, "A", 1, None, false).unwrap();
    paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, 2);
    let only_b = {
        let NodeKind::Raster { tiles, .. } = &e.document(d).unwrap().node(id).unwrap().kind else {
            panic!()
        };
        tiles.tiles[&TileKey { tx: 0, ty: 0 }]
    };
    let b = e.commit(d, "B", 2, None, false).unwrap();
    e.restore_version(d, a.hash).unwrap();
    while e.documents.get_mut(&d).unwrap().drop_oldest_undo() {}
    e.versions.get_mut(&d).unwrap().head = Some(a.hash);
    paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, 3);
    let c = e.commit(d, "C", 3, None, false).unwrap();
    while e.documents.get_mut(&d).unwrap().drop_oldest_undo() {}
    assert_eq!(e.versions[&d].commits[&c.hash].parents, vec![a.hash]);
    assert!(e.versions[&d].commits.contains_key(&b.hash));
    assert!(e.tile(&only_b).unwrap().is_some());
    assert!(e.log(d).unwrap().iter().all(|commit| commit.hash != b.hash));
    let bytes = e.save_package(d).unwrap();
    let mut fresh = Engine::new();
    let (f, warnings) = fresh.open_package(&bytes).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(fresh.versions[&f].commits.contains_key(&b.hash));
    assert!(fresh.tile(&only_b).unwrap().is_some());
    assert_eq!(fresh.prune_versions(f).unwrap(), 1);
    assert!(!fresh.versions[&f].commits.contains_key(&b.hash));
    assert!(fresh.versions[&f].commits.contains_key(&a.hash));
    assert!(fresh.versions[&f].commits.contains_key(&c.hash));
    assert!(fresh.tile(&only_b).unwrap().is_none());
    let again = fresh.save_package(f).unwrap();
    let mut reopened = Engine::new();
    let (g, warnings) = reopened.open_package(&again).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(!reopened.versions[&g].commits.contains_key(&b.hash));
    assert_eq!(reopened.log(g).unwrap().len(), 2);
}
