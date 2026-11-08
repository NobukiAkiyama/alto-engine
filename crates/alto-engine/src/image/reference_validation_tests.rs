use super::*;
use alto_doc::{DocumentMeta, Node};
fn crafted_package(key: TileKey, outside_pixel: bool) -> BTreeMap<String, Vec<u8>> {
    let mut engine = Engine::new();
    let doc = engine
        .create_document(DocumentMeta::srgb_u8(512, 256))
        .unwrap();
    let node = Node::raster("crafted");
    let id = node.id;
    let mut pixels = vec![0; 262144];
    pixels[0..4].copy_from_slice(&[255, 0, 0, 255]);
    if outside_pixel {
        pixels[4..8].copy_from_slice(&[0, 255, 0, 255]);
    }
    let hash = engine
        .put_tile(TileData::new(PixelFormat::Rgba8, pixels).unwrap())
        .unwrap();
    engine
        .transact(
            doc,
            "source",
            vec![
                Op::InsertNode {
                    node,
                    parent: engine.document(doc).unwrap().root,
                    index: 0,
                },
                Op::SetTiles {
                    layer: id,
                    changes: vec![alto_doc::TileChange {
                        key,
                        before: None,
                        after: Some(hash),
                    }],
                },
            ],
            0,
        )
        .unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&engine.snapshot(doc).unwrap()).unwrap();
    let node = value["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|n| n["id"].as_str() == Some(id.0.to_string().as_str()))
        .unwrap();
    node["kind"]["bounds"] = serde_json::json!({"x":0,"y":0,"width":1,"height":1});
    let snapshot = alto_core::json::canonical_bytes(&value).unwrap();
    let bytes = alto_store::encode_package(
        &snapshot,
        BTreeMap::from([(hash, engine.encoded(hash).unwrap().unwrap())]),
    )
    .unwrap();
    alto_store::read_entries(&bytes).unwrap()
}
#[test]
fn normal_and_recovery_open_reject_nonzero_boundary_padding() {
    let entries = crafted_package(TileKey { tx: 0, ty: 0 }, true);
    for deferred in [false, true] {
        let mut engine = Engine::new();
        let result = if deferred {
            engine.open_recovery_entries(entries.clone(), &Default::default())
        } else {
            engine.open_entries(entries.clone())
        };
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("nonzero pixels outside image bounds")
        );
        assert!(engine.documents.is_empty());
    }
    let valid = crafted_package(TileKey { tx: 0, ty: 0 }, false);
    for deferred in [false, true] {
        let mut engine = Engine::new();
        let (doc, warnings) = if deferred {
            engine
                .open_recovery_entries(valid.clone(), &Default::default())
                .unwrap()
        } else {
            engine.open_entries(valid.clone()).unwrap()
        };
        assert!(warnings.is_empty());
        let saved = engine.save_package(doc).unwrap();
        assert!(Engine::new().open_package(&saved).is_ok());
    }
}
#[test]
fn snapshot_rejects_tiles_disjoint_from_image_bounds() {
    let entries = crafted_package(TileKey { tx: 1, ty: 0 }, false);
    let package = alto_store::decode_entries(entries.clone()).unwrap();
    assert!(
        alto_doc::Document::from_snapshot_bytes(&package.snapshot)
            .unwrap_err()
            .to_string()
            .contains("does not intersect image bounds")
    );
    assert!(Engine::new().open_entries(entries.clone()).is_err());
    assert!(
        Engine::new()
            .open_recovery_entries(entries, &Default::default())
            .is_err()
    );
}
#[test]
fn rgba_tile_cannot_be_attached_as_an_r8_mask() {
    let mut e = Engine::new();
    let h = e.create_document(DocumentMeta::srgb_u8(16, 16)).unwrap();
    let n = Node::raster("raster");
    let id = n.id;
    let root = e.document(h).unwrap().root;
    e.transact(
        h,
        "add",
        vec![Op::InsertNode {
            node: n,
            parent: root,
            index: 0,
        }],
        0,
    )
    .unwrap();
    let hash = e
        .put_tile(TileData::new(PixelFormat::Rgba8, vec![255; 262144]).unwrap())
        .unwrap();
    let before = e.snapshot(h).unwrap();
    let revision = e.document(h).unwrap().revision;
    let history = e.history_state(h).unwrap();
    let mut mask = TileMap::new(PixelFormat::R8);
    mask.tiles.insert(TileKey { tx: 0, ty: 0 }, hash);
    let effect = alto_doc::Effect::Invert {
        selection: Some(mask.clone()),
        extra: Default::default(),
    };
    let invalid = [
        Op::SetMask {
            layer: id,
            mask: Some(mask),
        },
        Op::SetProps {
            id,
            patch: alto_doc::PropPatch {
                effects: Some(vec![effect.clone()]),
                ..Default::default()
            },
        },
        Op::SetAdjustment {
            id,
            adjustment: effect,
        },
    ];
    for op in invalid {
        let error = e
            .transact(h, "invalid mask reference", vec![op], revision)
            .unwrap_err();
        assert!(
            error.to_string().contains("mask tile requires R8"),
            "{error}"
        );
    }
    assert_eq!(e.snapshot(h).unwrap(), before);
    assert_eq!(e.document(h).unwrap().revision, revision);
    assert_eq!(e.history_state(h).unwrap(), history);
}

fn edit(e: &mut Engine, h: DocHandle, request: ImageEdit) -> ImageEditResult {
    let intent = request.intent();
    let p = e.prepare_image_edit(h, request).unwrap();
    e.apply_image_edit(h, p, intent).unwrap()
}
/// A 4x1 row (red, green, blue, white) with an Invert limited to the pixel at x=0.
fn row_with_effect_selection() -> (Engine, DocHandle, NodeId) {
    let mut e = Engine::new();
    let h = e.create_document(DocumentMeta::srgb_u8(4, 1)).unwrap();
    let mut n = Node::raster("row");
    let id = n.id;
    let mut out = BTreeMap::new();
    for (x, p) in [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255; 4],
    ]
    .into_iter()
    .enumerate()
    {
        write(&mut out, x as i32, 0, p);
    }
    let mut tiles = TileMap::new(PixelFormat::Rgba8);
    for (k, v) in out {
        let hash = e
            .put_tile(TileData::new(PixelFormat::Rgba8, v).unwrap())
            .unwrap();
        tiles.tiles.insert(k, hash);
    }
    n.kind = NodeKind::Raster {
        bounds: ImageBounds {
            x: 0,
            y: 0,
            width: 4,
            height: 1,
        },
        tiles,
    };
    let mut r8 = vec![0; 65536];
    r8[0] = 255;
    let mut selection = TileMap::new(PixelFormat::R8);
    selection.tiles.insert(
        TileKey { tx: 0, ty: 0 },
        e.put_tile(TileData::new(PixelFormat::R8, r8).unwrap())
            .unwrap(),
    );
    n.props.effects = vec![alto_doc::Effect::Invert {
        selection: Some(selection),
        extra: Default::default(),
    }];
    let root = e.document(h).unwrap().root;
    e.transact(
        h,
        "add",
        vec![Op::InsertNode {
            node: n,
            parent: root,
            index: 0,
        }],
        0,
    )
    .unwrap();
    (e, h, id)
}
fn effect_selection(e: &Engine, h: DocHandle, id: NodeId, x: i32, y: i32) -> u8 {
    let node = e.document(h).unwrap().node(id).unwrap();
    read(e, node.props.effects[0].selection().unwrap(), x, y).unwrap()[3]
}
#[test]
fn flip_and_rotate_move_effect_selections_with_the_pixels() {
    let (mut e, h, id) = row_with_effect_selection();
    let before = e.snapshot(h).unwrap();
    edit(
        &mut e,
        h,
        ImageEdit::Flip {
            node: id,
            axis: FlipAxis::Horizontal,
        },
    );
    assert_eq!(
        e.pick_color(h, id, [3.0, 0.0]).unwrap().bytes,
        [255, 0, 0, 255]
    );
    assert_eq!(effect_selection(&e, h, id, 3, 0), 255);
    assert_eq!(effect_selection(&e, h, id, 0, 0), 0);
    e.undo(h).unwrap();
    assert_eq!(e.snapshot(h).unwrap(), before);
    // A quarter turn sends the old (3, 0) to (0, 3).
    edit(
        &mut e,
        h,
        ImageEdit::Rotate {
            node: id,
            quarter_turns: 1,
        },
    );
    assert_eq!(
        e.pick_color(h, id, [0.0, 0.0]).unwrap().bytes,
        [255, 0, 0, 255]
    );
    assert_eq!(effect_selection(&e, h, id, 0, 0), 255);
    e.undo(h).unwrap();
    edit(
        &mut e,
        h,
        ImageEdit::Rotate {
            node: id,
            quarter_turns: 3,
        },
    );
    assert_eq!(
        e.pick_color(h, id, [0.0, 3.0]).unwrap().bytes,
        [255, 0, 0, 255]
    );
    assert_eq!(effect_selection(&e, h, id, 0, 3), 255);
    assert_eq!(effect_selection(&e, h, id, 0, 0), 0);
    e.undo(h).unwrap();
    edit(
        &mut e,
        h,
        ImageEdit::Rotate {
            node: id,
            quarter_turns: 2,
        },
    );
    assert_eq!(effect_selection(&e, h, id, 3, 0), 255);
    assert_eq!(effect_selection(&e, h, id, 0, 0), 0);
}
#[test]
fn resize_resamples_effect_selections_and_keeps_an_empty_one_empty() {
    let (mut e, h, id) = row_with_effect_selection();
    edit(
        &mut e,
        h,
        ImageEdit::Resize {
            node: id,
            width: 8,
            height: 2,
            filter: ImageFilter::Nearest,
        },
    );
    // Red now covers x 0..2; the selection's bilinear edge stays around it.
    for y in 0..2 {
        assert!(effect_selection(&e, h, id, 0, y) > 128);
        assert!(effect_selection(&e, h, id, 1, y) > 128);
        assert_eq!(effect_selection(&e, h, id, 3, y), 0);
    }
    // Halving samples between x=0 (selected) and x=1 for the first new pixel.
    let (mut e, h, id) = row_with_effect_selection();
    edit(
        &mut e,
        h,
        ImageEdit::Resize {
            node: id,
            width: 2,
            height: 1,
            filter: ImageFilter::Bilinear,
        },
    );
    assert_eq!(effect_selection(&e, h, id, 0, 0), 128);
    assert_eq!(effect_selection(&e, h, id, 1, 0), 0);
    // An empty selection stays an empty `Some`: the effect must not spread to all pixels.
    let emptied = {
        let (mut e, h, id) = row_with_effect_selection();
        let mut node = e.document(h).unwrap().node(id).unwrap().clone();
        *node.props.effects[0].selection_mut() = Some(TileMap::new(PixelFormat::R8));
        let revision = e.document(h).unwrap().revision;
        e.transact(h, "empty", vec![Op::ReplaceNode { id, node }], revision)
            .unwrap();
        edit(
            &mut e,
            h,
            ImageEdit::Flip {
                node: id,
                axis: FlipAxis::Vertical,
            },
        );
        e.document(h).unwrap().node(id).unwrap().props.effects[0]
            .selection()
            .cloned()
    };
    assert!(emptied.is_some_and(|m| m.tiles.is_empty()));
}
