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
    let record = e
        .journal_record(h, JournalKind::Transact, "edit", &ops)
        .unwrap();
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

#[test]
fn dashed_stroke_roundtrips_through_format_seven_and_replay() {
    use alto_doc::version::ContentDetail;
    use alto_vector::{Dash, LineCap, LineJoin, Stroke};

    let mut engine = Engine::new();
    let doc = engine
        .create_document(DocumentMeta::srgb_u8(256, 256))
        .unwrap();
    let root = engine.document(doc).unwrap().root;
    let mut path = shape();
    path.stroke = Some(Stroke {
        color: [0.0, 0.0, 0.0, 1.0],
        width: 4.0,
        cap: LineCap::Round,
        join: LineJoin::Round,
        miter_limit: 4.0,
        dash: None,
    });
    let node = alto_doc::Node::vector("line", vec![path.clone()]);
    let id = node.id;
    engine
        .transact(
            doc,
            "insert",
            vec![Op::InsertNode {
                node,
                parent: root,
                index: 0,
            }],
            0,
        )
        .unwrap();
    let before = engine.document(doc).unwrap().clone();
    assert_eq!(before.meta.format_version, 5);
    let base = engine.save_package(doc).unwrap();

    path.stroke.as_mut().unwrap().dash = Some(Dash {
        pattern: vec![12.0, 5.0],
        offset: -2.0,
    });
    let ops = vec![Op::SetVector {
        id,
        paths: vec![path.clone()],
    }];
    engine.transact(doc, "dash", ops.clone(), 1).unwrap();
    let after = engine.document(doc).unwrap();
    assert_eq!(after.meta.format_version, 7);
    assert!(matches!(
        alto_doc::diff(&before, after).as_slice(),
        [alto_doc::DiffItem::ContentChanged {
            detail: ContentDetail::Vector { style: true, .. },
            ..
        }]
    ));
    let expected = engine.snapshot(doc).unwrap();
    let record = engine
        .journal_record(doc, JournalKind::Transact, "dash", &ops)
        .unwrap();
    let package = engine.save_package(doc).unwrap();
    let entries = alto_store::read_entries(&package).unwrap();
    let manifest: alto_store::Manifest = serde_json::from_slice(&entries["manifest.json"]).unwrap();
    assert_eq!(manifest.document_format, 7);
    let mut reopened = Engine::new();
    let (saved, warnings) = reopened.open_package(&package).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(reopened.snapshot(saved).unwrap(), expected);
    let (replayed, _) = reopened.open_package(&base).unwrap();
    reopened.replay_record(replayed, &record).unwrap();
    assert_eq!(reopened.snapshot(replayed).unwrap(), expected);

    engine.undo(doc).unwrap();
    assert_eq!(engine.document(doc).unwrap().meta.format_version, 5);
    engine.redo(doc).unwrap();
    assert_eq!(engine.snapshot(doc).unwrap(), expected);

    let resize = crate::canvas::CanvasEdit::Resize {
        width: 512,
        height: 128,
    };
    let intent = resize.intent();
    let prepared = engine.prepare_canvas_edit(doc, resize).unwrap();
    let revision = engine.document(doc).unwrap().revision;
    engine
        .transact(doc, intent, prepared.ops, revision)
        .unwrap();
    engine.release_staged_tiles(prepared.stage);
    let resized = engine.document(doc).unwrap();
    assert_eq!(resized.meta.format_version, 7);
    assert!(matches!(&resized.node(id).unwrap().kind,
        alto_doc::NodeKind::Vector { paths } if paths == std::slice::from_ref(&path)
    ));

    let text = alto_doc::Node::text(
        "format 6 text",
        alto_text::TextContent {
            text: "あい。".into(),
            hanging: true,
            ..Default::default()
        },
    );
    let revision = resized.revision;
    engine
        .transact(
            doc,
            "add text",
            vec![Op::InsertNode {
                node: text,
                parent: root,
                index: 1,
            }],
            revision,
        )
        .unwrap();
    assert_eq!(engine.document(doc).unwrap().meta.format_version, 7);

    path.stroke.as_mut().unwrap().dash = None;
    let revision = engine.document(doc).unwrap().revision;
    engine
        .transact(
            doc,
            "clear dash",
            vec![Op::SetVector {
                id,
                paths: vec![path],
            }],
            revision,
        )
        .unwrap();
    assert_eq!(engine.document(doc).unwrap().meta.format_version, 6);
}
