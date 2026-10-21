//! Extends the M4 sample with editable masks, clipping, an adjustment and Gaussian blur.
use alto_core::{PixelFormat, TileKey};
use alto_doc::{Isolation, Node, NodeKind, Op, PropPatch, TileChange};
use alto_engine::{DocHandle, Engine};
use alto_raster::TileData;
use serde_json::json;
fn commit(e: &mut Engine, h: DocHandle, ops: Vec<Op>) -> Result<(), Box<dyn std::error::Error>> {
    let rev = e.document(h)?.revision;
    e.transact(h, "M5 sample", ops, rev)?;
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "verification/sample.alto".into());
    let output = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "verification/sample-m5.alto".into());
    let mut engine = Engine::new();
    let (h, warnings) = engine.open_package(&std::fs::read(input)?)?;
    assert!(warnings.is_empty());
    let doc = engine.document(h)?;
    let root = doc.root;
    let shapes = doc
        .nodes
        .values()
        .find(|n| n.name == "色のかたち")
        .expect("source sample shapes")
        .id;
    let paint = doc
        .nodes
        .values()
        .find(|n| n.name == "ペイント")
        .expect("source sample paint")
        .id;
    let mut group = Node::group("ぼかしとクリッピング");
    if let NodeKind::Group { isolation, .. } = &mut group.kind {
        *isolation = Isolation::Isolated;
    }
    group.props.effects = vec![json!({"type":"gaussian_blur","radius":12})];
    let group_id = group.id;
    let mut stripes = Node::raster("クリップしたストライプ");
    stripes.props.clip_to_below = true;
    let stripe_id = stripes.id;
    let mut changes = vec![];
    for ty in 0..2 {
        for tx in 0..3 {
            let mut pixels = vec![0u8; 262144];
            for y in 0..256 {
                for x in 0..256 {
                    let px = tx * 256 + x;
                    let py = ty * 256 + y;
                    if ((px + py) / 45) % 2 == 0 {
                        let i = (y * 256 + x) as usize * 4;
                        pixels[i..i + 4].copy_from_slice(&[117, 87, 235, 110]);
                    }
                }
            }
            if let Some(hash) = engine
                .put_tile(TileData::new(PixelFormat::Rgba8, pixels).expect("fixed dimensions"))
            {
                changes.push(TileChange {
                    key: TileKey { tx, ty },
                    before: None,
                    after: Some(hash),
                });
            }
        }
    }
    commit(
        &mut engine,
        h,
        vec![
            Op::InsertNode {
                node: group,
                parent: root,
                index: 1,
            },
            Op::MoveNode {
                id: shapes,
                parent: group_id,
                index: 0,
            },
            Op::InsertNode {
                node: stripes,
                parent: group_id,
                index: 1,
            },
            Op::SetTiles {
                layer: stripe_id,
                changes,
            },
        ],
    )?;
    let ops = engine.selection_ops(
        h,
        &json!({"kind":"ellipse","x":120,"y":110,"width":520,"height":330,"feather":16}),
    )?;
    commit(&mut engine, h, ops)?;
    let mask = engine.mask_for_node(h, paint, false, true)?;
    commit(
        &mut engine,
        h,
        vec![Op::SetMask {
            layer: paint,
            mask: Some(mask),
        }],
    )?;
    let ops = engine.selection_ops(
        h,
        &json!({"kind":"rectangle","x":384,"y":0,"width":384,"height":512,"feather":24}),
    )?;
    commit(&mut engine, h, ops)?;
    let mut adjustment = Node::raster("右半分の色調整");
    adjustment.kind = NodeKind::Adjustment {
        adjustment: json!({"type":"color_adjust","brightness":0.04,"contrast":1.12,"saturation":1.35}),
    };
    adjustment.props.mask = engine.document(h)?.selection.clone();
    let index = engine
        .document(h)?
        .node(root)?
        .children()
        .expect("root group")
        .len();
    commit(
        &mut engine,
        h,
        vec![
            Op::InsertNode {
                node: adjustment,
                parent: root,
                index,
            },
            Op::SetProps {
                id: paint,
                patch: PropPatch {
                    name: Some("マスク付きペイント".into()),
                    ..Default::default()
                },
            },
        ],
    )?;
    let ops = engine.selection_ops(
        h,
        &json!({"kind":"ellipse","x":170,"y":120,"width":420,"height":310,"feather":0}),
    )?;
    commit(&mut engine, h, ops)?;
    let bytes = engine.save_package(h)?;
    let mut fresh = Engine::new();
    let (loaded, warnings) = fresh.open_package(&bytes)?;
    assert!(warnings.is_empty());
    assert_eq!(engine.snapshot(h)?, fresh.snapshot(loaded)?);
    std::fs::write(&output, bytes)?;
    println!("Created {output}");
    Ok(())
}
