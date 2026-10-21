use super::*;
use alto_core::Affine2;
use alto_vector::{ClipPath, FillRule, PathData, StyledPath, VectorMask};
fn shape() -> StyledPath {
    StyledPath {
        data: PathData::from_svg("M0.1234567890123 20 Q128 -80 256 20 L256 300 H0 Z").unwrap(),
        fill: Some([0.2, 0.3, 0.9, 0.5]),
        stroke: None,
        fill_rule: FillRule::EvenOdd,
    }
}
#[test]
fn vector_paths_masks_clips_are_reversible_serializable_and_journaled() {
    let mut e = Engine::new();
    let h = e.create_document(DocumentMeta::srgb_u8(512, 512)).unwrap();
    let root = e.document(h).unwrap().root;
    let node = alto_doc::Node::vector("vector", vec![shape()]);
    let id = node.id;
    e.transact(
        h,
        "insert",
        vec![Op::InsertNode {
            node,
            parent: root,
            index: 0,
        }],
        0,
    )
    .unwrap();
    let base = e.save_package(h).unwrap();
    let before = e.snapshot(h).unwrap();
    let path = shape().data;
    let ops = vec![
        Op::SetVectorMask {
            id,
            mask: Some(VectorMask {
                path: path.clone(),
                fill_rule: FillRule::NonZero,
                feather: 8.,
                enabled: true,
                transform: Affine2([1., 0., 0.1, 1., 12., 4.]),
            }),
        },
        Op::SetClipPath {
            id,
            path: Some(ClipPath {
                path: path.clone(),
                fill_rule: FillRule::EvenOdd,
                enabled: true,
                transform: Affine2::default(),
            }),
        },
        Op::SetVector {
            id,
            paths: vec![StyledPath {
                fill: Some([1., 0., 0., 1.]),
                ..shape()
            }],
        },
    ];
    e.transact(h, "edit", ops.clone(), 1).unwrap();
    let after = e.snapshot(h).unwrap();
    let record = e.journal_record("edit", &ops).unwrap();
    e.undo(h).unwrap();
    assert_eq!(before, e.snapshot(h).unwrap());
    e.redo(h).unwrap();
    assert_eq!(after, e.snapshot(h).unwrap());
    let bytes = e.save_package(h).unwrap();
    let mut reopened = Engine::new();
    let (other, w) = reopened.open_package(&bytes).unwrap();
    assert!(w.is_empty());
    assert_eq!(after, reopened.snapshot(other).unwrap());
    let mut recovered = Engine::new();
    let (other, _) = recovered.open_package(&base).unwrap();
    recovered.replay_record(other, &record).unwrap();
    assert_eq!(after, recovered.snapshot(other).unwrap());
    let scene = e.render_scene(h).unwrap();
    assert!(matches!(scene.nodes[1].content, RContent::Mesh(_)));
    assert!(scene.nodes[1].features.vector_mask.is_some());
    assert!(scene.nodes[1].features.clip_path.is_some());
}
#[test]
fn invalid_insert_path_and_mask_are_atomic() {
    let mut e = Engine::new();
    let h = e.create_document(DocumentMeta::srgb_u8(512, 512)).unwrap();
    let root = e.document(h).unwrap().root;
    let before = e.snapshot(h).unwrap();
    let mut p = shape();
    p.data.commands = vec![alto_vector::PathCommand::Line { x: 0., y: 0. }];
    let node = alto_doc::Node::vector("bad", vec![p]);
    assert!(
        e.transact(
            h,
            "bad",
            vec![Op::InsertNode {
                node,
                parent: root,
                index: 0
            }],
            0
        )
        .is_err()
    );
    assert_eq!(before, e.snapshot(h).unwrap());
    let node = alto_doc::Node::vector("good", vec![shape()]);
    let id = node.id;
    e.transact(
        h,
        "good",
        vec![Op::InsertNode {
            node,
            parent: root,
            index: 0,
        }],
        0,
    )
    .unwrap();
    let before = e.snapshot(h).unwrap();
    let mut p = shape();
    p.fill = Some([-1., 0., 0., 1.]);
    let bad = vec![
        Op::SetVector { id, paths: vec![p] },
        Op::SetVectorMask {
            id,
            mask: Some(VectorMask {
                path: shape().data,
                fill_rule: FillRule::NonZero,
                feather: f32::NAN,
                enabled: true,
                transform: Affine2::default(),
            }),
        },
    ];
    for op in bad {
        assert!(e.transact(h, "bad", vec![op], 1).is_err());
        assert_eq!(before, e.snapshot(h).unwrap());
    }
}
