use super::*;
use alto_core::NodeId;
use alto_doc::{Node, PropPatch};
use alto_scene::RenderBlend;

fn layer(engine: &mut Engine, handle: DocHandle) -> NodeId {
    let node = Node::raster("paint");
    let id = node.id;
    let root = engine.document(handle).unwrap().root;
    engine
        .transact(
            handle,
            "layer",
            vec![Op::InsertNode {
                node,
                parent: root,
                index: 0,
            }],
            engine.document(handle).unwrap().revision,
        )
        .unwrap();
    id
}

#[test]
fn overlay_reaches_the_renderer_and_levels_bump_the_format() {
    let mut engine = Engine::new();
    let handle = engine
        .create_document(DocumentMeta::srgb_u8(32, 32))
        .unwrap();
    let id = layer(&mut engine, handle);
    let mut node = engine.document(handle).unwrap().node(id).unwrap().clone();
    node.props.blend = BlendMode::Overlay;
    node.props.transform = alto_core::Affine2([0.1, 0.0, 0.0, 1.0, 0.3, 4.5]);
    let revision = engine.document(handle).unwrap().revision;
    engine
        .transact(
            handle,
            "blend",
            vec![Op::ReplaceNode { id, node }],
            revision,
        )
        .unwrap();
    let scene = engine.render_scene(handle).unwrap();
    assert_eq!(scene.nodes[1].blend, RenderBlend::Overlay);
    assert_eq!(scene.nodes[1].blend as u32, 3);
    assert_eq!(scene.nodes[1].transform[0], 0.1);
    assert_eq!(scene.nodes[1].transform[4], 0.3);
    engine.undo(handle).unwrap();
    assert_eq!(
        engine.render_scene(handle).unwrap().nodes[1].blend,
        RenderBlend::Normal
    );
    let revision = engine.document(handle).unwrap().revision;
    engine
        .transact(
            handle,
            "levels",
            vec![Op::SetProps {
                id,
                patch: PropPatch {
                    effects: Some(vec![alto_doc::Effect::Levels {
                        in_black: 0.0,
                        in_white: 1.0,
                        gamma: 1.0,
                        out_black: 0.0,
                        out_white: 1.0,
                        selection: None,
                        extra: Default::default(),
                    }]),
                    ..PropPatch::default()
                },
            }],
            revision,
        )
        .unwrap();
    let snapshot = engine.snapshot(handle).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
    assert_eq!(value["meta"]["format_version"], 5);
    assert_eq!(value["meta"]["render_version"], 3);
    let reopened = engine.open_snapshot(&snapshot).unwrap();
    assert_eq!(engine.document(reopened).unwrap().meta.format_version, 5);
    engine.undo(handle).unwrap();
    let snapshot = engine.snapshot(handle).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
    assert_eq!(value["meta"]["format_version"], 5);
    assert_eq!(value["meta"]["render_version"], 3);
    engine.open_snapshot(&snapshot).unwrap();
}
