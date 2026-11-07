//! Text layers and fonts.
use super::*;
use alto_core::AssetId;
use alto_doc::Node;
use alto_text::{Motion, TextContent, TextFrame, TextStyle, WritingMode};

const JP: &[u8] = include_bytes!("../../../tests/fonts/AltoTestJP.otf");
const LATIN: &[u8] = include_bytes!("../../../tests/fonts/AltoTestLatin.ttf");

pub(crate) fn setup() -> (Engine, DocHandle, AssetId, AssetId) {
    let mut e = Engine::new();
    let d = e.create_document(DocumentMeta::srgb_u8(1024, 768)).unwrap();
    let jp = e.import_font(JP.to_vec(), 0).unwrap();
    let latin = e.import_font(LATIN.to_vec(), 0).unwrap();
    assert!(jp.warnings.is_empty());
    assert_eq!(jp.info.family, "Noto Sans JP");
    let ops = vec![
        Op::SetAsset {
            id: jp.record.id,
            record: Some(jp.record.clone()),
        },
        Op::SetAsset {
            id: latin.record.id,
            record: Some(latin.record.clone()),
        },
    ];
    e.transact(d, "fonts", ops, 0).unwrap();
    (e, d, latin.record.id, jp.record.id)
}
pub(crate) fn vertical(fonts: Vec<AssetId>) -> TextContent {
    TextContent {
        text: "吾輩は猫である。\n第12回".into(),
        style: TextStyle {
            fonts,
            size: 40.0,
            ..Default::default()
        },
        writing_mode: WritingMode::VerticalRl,
        frame: TextFrame::Area {
            width: 300.0,
            height: 400.0,
        },
        ..Default::default()
    }
}
pub(crate) fn add_text(e: &mut Engine, d: DocHandle, content: TextContent) -> alto_core::NodeId {
    let mut node = Node::text("本文", content);
    node.props.transform = alto_core::Affine2([1.0, 0.0, 0.0, 1.0, 100.0, 50.0]);
    let id = node.id;
    let root = e.document(d).unwrap().root;
    let rev = e.document(d).unwrap().revision;
    e.transact(
        d,
        "テキスト追加",
        vec![Op::InsertNode {
            node,
            parent: root,
            index: 0,
        }],
        rev,
    )
    .unwrap();
    id
}

#[test]
fn text_layers_render_select_save_and_recover() {
    let (mut e, d, latin, jp) = setup();
    let id = add_text(&mut e, d, vertical(vec![latin, jp]));
    let scene = e.render_scene(d).unwrap();
    assert!(
        matches!(&scene.nodes[1].content, RContent::TextBake(Some(_))),
        "text renders from its bake"
    );
    let info = e.text_info(d, id).unwrap();
    assert_eq!(info.lines.len(), 2);
    assert!(info.missing.is_empty());
    assert_eq!(
        info.fonts_used,
        vec![jp, latin],
        "digits fall back to the first font that has them"
    );
    assert_eq!(
        info.quad[0],
        [100.0, 50.0],
        "frame origin in document space"
    );
    // First column: x in [100+240, 100+300] document px; the 3rd character sits at y 50+80..50+120.
    assert_eq!(e.text_hit_test(d, id, [370.0, 50.0 + 81.0]).unwrap(), 2);
    let caret = e.text_caret(d, id, 2).unwrap().unwrap();
    assert_eq!((caret.a[1], caret.b[1]), (130.0, 130.0));
    let quads = e.text_selection(d, id, 7, 10).unwrap();
    assert_eq!(quads.len(), 2, "selection spans two columns");
    assert_eq!(
        e.text_navigate(d, id, 3, Motion::NextLine).unwrap(),
        13,
        "same height in the next column"
    );
    // Edits are reversible and the cached layout follows the content.
    let mut edited = vertical(vec![latin, jp]);
    edited.text = "吾輩はABCである".into();
    let rev = e.document(d).unwrap().revision;
    let ops = vec![Op::SetText {
        id,
        content: Box::new(edited),
        bake: None,
    }];
    e.transact(d, "テキスト編集", ops.clone(), rev).unwrap();
    assert_eq!(e.text_info(d, id).unwrap().fonts_used, vec![jp, latin]);
    let record = e
        .journal_record(d, JournalKind::Transact, "テキスト編集", &ops)
        .unwrap();
    e.undo(d).unwrap();
    assert_eq!(e.text_info(d, id).unwrap().lines.len(), 2);
    e.redo(d).unwrap();
    // Fonts travel in the package; a fresh engine lays out identically.
    let bytes = e.save_package(d).unwrap();
    let mut fresh = Engine::new();
    let (f, warnings) = fresh.open_package(&bytes).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(fresh.snapshot(f).unwrap(), e.snapshot(d).unwrap());
    let a = e.render_scene(d).unwrap();
    let b = fresh.render_scene(f).unwrap();
    assert_eq!(a.nodes[1].content, b.nodes[1].content);
    // Journal replay over the pre-edit base reproduces the edit.
    e.undo(d).unwrap();
    let base = e.save_package(d).unwrap();
    let mut recovered = Engine::new();
    let (r, _) = recovered.open_package(&base).unwrap();
    recovered.replay_record(r, &record).unwrap();
    e.redo(d).unwrap();
    assert_eq!(recovered.snapshot(r).unwrap(), e.snapshot(d).unwrap());
}

#[test]
fn restricted_fonts_warn_and_invalid_fonts_are_rejected() {
    let mut e = Engine::new();
    let d = e.create_document(DocumentMeta::srgb_u8(64, 64)).unwrap();
    assert!(e.import_font(b"bad".to_vec(), 0).is_err());
    let mut bytes = LATIN.to_vec();
    let tables = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    let os2 = (0..tables)
        .map(|i| 12 + i * 16)
        .find(|o| &bytes[*o..*o + 4] == b"OS/2")
        .unwrap();
    let offset = u32::from_be_bytes(bytes[os2 + 8..os2 + 12].try_into().unwrap()) as usize;
    bytes[offset + 8..offset + 10].copy_from_slice(&2u16.to_be_bytes());
    let err = e.import_font(bytes, 0).unwrap_err().to_string();
    assert!(
        err.contains("INVALID_ARGUMENT") && err.contains("forbids embedding"),
        "{err}"
    );
    assert!(e.font_warnings(d).unwrap().is_empty());
}
