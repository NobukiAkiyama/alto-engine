//! Render version 3 text bakes (doc/design/2026-10-28-v3-integer-text.md,
//! implementation order 0). Baking never changes pixels: v1 / v2 never bake,
//! and no renderer reads a bake yet.
use super::*;
use crate::text::BAKER_VERSION;
use crate::unstable_render::UnstableRenderV3Guard;
use alto_core::text_bake::{BakeCommand, PLACEHOLDER_FONT, TextBake};
use alto_core::{AssetId, NodeId, RENDER_VERSION_EXACT};
use alto_doc::Node;
use alto_text::{TextContent, TextFrame, TextStyle, WritingMode};

const JP: &[u8] = include_bytes!("../../../tests/fonts/AltoTestJP.otf");
const LATIN: &[u8] = include_bytes!("../../../tests/fonts/AltoTestLatin.ttf");
/// blake3 of the parity fonts (memo "字形の同一性").
const LATIN_BLAKE3: &str = "cecfaa71790a6d0aa8744ddfdb3cb9fd6fbac9e2f8e0db476827710df1e0dbc3";
const JP_BLAKE3: &str = "5e2213b3dfdcc9736b3bd951f51ec58935a3bde47bc4f9521838f042c0db8981";

fn document(version: u32) -> (Engine, DocHandle, AssetId, AssetId) {
    let mut e = Engine::new();
    let mut meta = DocumentMeta::srgb_u8(1024, 768);
    if version == RENDER_VERSION_EXACT {
        meta.render_version = RENDER_VERSION_EXACT;
        meta.format_version = 4;
    }
    let d = e.create_document(meta).unwrap();
    let jp = e.import_font(JP.to_vec(), 0).unwrap();
    let latin = e.import_font(LATIN.to_vec(), 0).unwrap();
    assert_eq!(latin.record.hash.to_string(), LATIN_BLAKE3);
    assert_eq!(jp.record.hash.to_string(), JP_BLAKE3);
    let ops = [&jp, &latin]
        .map(|f| Op::SetAsset {
            id: f.record.id,
            record: Some(f.record.clone()),
        })
        .to_vec();
    e.transact(d, "fonts", ops, 0).unwrap();
    (e, d, latin.record.id, jp.record.id)
}

/// The parity texts: Latin horizontal, Japanese vertical, and text with no
/// usable font (the placeholder box; a font's own missing glyph is its .notdef).
fn texts(latin: AssetId, jp: AssetId) -> Vec<(&'static str, TextContent)> {
    let style = |fonts: Vec<AssetId>, size| TextStyle {
        fonts,
        size,
        ..Default::default()
    };
    vec![
        (
            "text-v3-latin",
            TextContent {
                text: "Alto 0.25 px/em".into(),
                style: style(vec![latin], 37.5),
                ..Default::default()
            },
        ),
        (
            "text-v3-vertical",
            TextContent {
                text: "吾輩は猫である。\n第12回".into(),
                style: style(vec![latin, jp], 40.0),
                writing_mode: WritingMode::VerticalRl,
                frame: TextFrame::Area {
                    width: 300.0,
                    height: 400.0,
                },
                ..Default::default()
            },
        ),
        (
            "text-v3-placeholder",
            TextContent {
                text: "A B".into(),
                style: style(vec![], 24.0),
                ..Default::default()
            },
        ),
    ]
}

fn insert(e: &mut Engine, d: DocHandle, content: TextContent) -> NodeId {
    let node = Node::text("本文", content);
    let id = node.id;
    let doc = e.document(d).unwrap();
    let (root, rev) = (doc.root, doc.revision);
    e.transact(
        d,
        "text",
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

fn bake_of(e: &Engine, d: DocHandle, id: NodeId) -> Option<ContentHash> {
    match &e.document(d).unwrap().node(id).unwrap().kind {
        NodeKind::Text { bake, .. } => *bake,
        _ => panic!("not text"),
    }
}

fn decoded(e: &Engine, hash: ContentHash) -> TextBake {
    TextBake::decode(e.blobs.get(&hash).expect("bake blob stored")).expect("valid bake")
}

#[test]
fn text_bake_dependencies_are_pinned() {
    let lock = include_str!("../../../Cargo.lock");
    for (name, version) in [
        ("rustybuzz", "0.20.1"),
        ("ttf-parser", "0.25.1"),
        ("unicode-bidi", "0.3.18"),
        ("unicode-linebreak", "0.1.5"),
        ("unicode-segmentation", "1.13.3"),
        ("unicode-script", "0.5.8"),
        ("unicode-ccc", "0.4.0"),
        ("unicode-properties", "0.1.4"),
    ] {
        let entry = format!("name = \"{name}\"\nversion = \"{version}\"\n");
        assert!(lock.contains(&entry), "{name} is not locked at {version}");
        assert_eq!(
            lock.matches(&format!("name = \"{name}\"\n")).count(),
            1,
            "{name} is locked at more than one version"
        );
    }
    let text = include_str!("../../alto-text/Cargo.toml");
    assert!(
        text.contains("rustybuzz = \"=0.20.1\""),
        "rustybuzz is not pinned exactly"
    );
}

#[test]
fn text_bake_roundtrip_and_validation() {
    let (mut e, d, latin, jp) = document(RENDER_VERSION_EXACT);
    let content = texts(latin, jp).remove(1).1;
    let id = insert(&mut e, d, content.clone());
    let hash = bake_of(&e, d, id).expect("a v3 text node is baked on insert");
    let bake = decoded(&e, hash);
    assert_eq!(bake.baker, BAKER_VERSION);
    assert_eq!(
        bake.encode(),
        e.blobs[&hash],
        "decode then encode is the identity"
    );
    assert!(!bake.glyphs.is_empty());
    // Only fonts that produced glyphs are listed, as (blake3, face).
    let listed: Vec<String> = bake.fonts.iter().map(|f| f.hash.to_string()).collect();
    assert!(listed.contains(&JP_BLAKE3.to_string()), "{listed:?}");
    assert!(bake.fonts.iter().all(|f| f.face_index == 0));
    // The bake is a referenced object of the document and its snapshot.
    assert!(crate::versions::doc_hashes(e.document(d).unwrap()).contains(&hash));
    let snapshot = e.snapshot(d).unwrap();
    assert!(String::from_utf8_lossy(&snapshot).contains(&hash.to_string()));
    let reopened = alto_doc::Document::from_snapshot_bytes(&snapshot).unwrap();
    assert_eq!(
        reopened.node(id).unwrap().kind,
        e.document(d).unwrap().node(id).unwrap().kind
    );

    // An edit re-bakes in the same op; undo and redo restore each bake.
    let mut edited = content.clone();
    edited.text = "吾輩は犬である".into();
    let rev = e.document(d).unwrap().revision;
    e.transact(
        d,
        "edit",
        vec![Op::SetText {
            id,
            content: Box::new(edited),
            bake: None,
        }],
        rev,
    )
    .unwrap();
    let rebaked = bake_of(&e, d, id).unwrap();
    assert_ne!(rebaked, hash);
    e.undo(d).unwrap();
    assert_eq!(bake_of(&e, d, id), Some(hash));
    e.redo(d).unwrap();
    assert_eq!(bake_of(&e, d, id), Some(rebaked));

    // Decoding refuses every out-of-bound field.
    let good = e.blobs[&hash].clone();
    let corrupt = |at: usize, bytes: &[u8]| {
        let mut b = good.clone();
        b[at..at + bytes.len()].copy_from_slice(bytes);
        TextBake::decode(&b)
    };
    assert!(corrupt(0, b"ALTX").is_err(), "magic");
    assert!(corrupt(4, &2u16.to_le_bytes()).is_err(), "version");
    let mut trailing = good.clone();
    trailing.push(0);
    assert!(TextBake::decode(&trailing).is_err(), "trailing byte");
    assert!(
        TextBake::decode(&good[..good.len() - 1]).is_err(),
        "truncated"
    );
    let header = 4 + 2 + 2 + 4 + bake.fonts.len() * 36 + 4;
    assert!(
        corrupt(header, &(bake.fonts.len() as u32 + 7).to_le_bytes()).is_err(),
        "font index"
    );
    assert!(corrupt(header + 6, &0u16.to_le_bytes()).is_err(), "upem 0");
    assert!(
        corrupt(header + 6, &16385u16.to_le_bytes()).is_err(),
        "upem"
    );
    assert!(
        corrupt(header + 8, &4001u32.to_le_bytes()).is_err(),
        "command count"
    );
    assert!(
        corrupt(header + 13, &((1i32 << 22) + 1).to_le_bytes()).is_err(),
        "coordinate"
    );
    let mut far = bake.clone();
    far.glyphs[0].t[0] = (1i64 << 60) + 1;
    assert!(TextBake::decode(&far.encode()).is_err(), "translation");
    let mut colour = bake.clone();
    colour.glyphs[0].color[3] = 16385;
    assert!(TextBake::decode(&colour.encode()).is_err(), "colour");
    let mut index = bake.clone();
    index.glyphs[0].outline = bake.outlines.len() as u32;
    assert!(TextBake::decode(&index.encode()).is_err(), "outline index");
}

#[test]
fn text_bake_only_on_render_version_three() {
    let (mut e, d, latin, jp) = document(alto_core::RENDER_VERSION);
    let content = texts(latin, jp).remove(0).1;
    let id = insert(&mut e, d, content.clone());
    assert_eq!(bake_of(&e, d, id), None, "v1 / v2 never bake");
    let snapshot = String::from_utf8(e.snapshot(d).unwrap()).unwrap();
    assert!(
        !snapshot.contains("\"bake\""),
        "a v2 snapshot has no bake field"
    );
    // A bake on a v2 document is refused, whoever sends it.
    let hash = e.put_blob(
        e.bake_text(&e.document(d).unwrap().assets, &content)
            .encode(),
    );
    let rev = e.document(d).unwrap().revision;
    let refused = e.transact(
        d,
        "bake",
        vec![Op::SetText {
            id,
            content: Box::new(content),
            bake: Some(hash),
        }],
        rev,
    );
    assert!(refused.is_err());
    assert_eq!(bake_of(&e, d, id), None);
}

#[test]
fn text_bake_on_render_upgrade_is_one_undoable_edit() {
    let _v3 = UnstableRenderV3Guard::enable();
    let (mut e, d, latin, jp) = document(alto_core::RENDER_VERSION);
    let ids: Vec<NodeId> = texts(latin, jp)
        .into_iter()
        .map(|(_, content)| insert(&mut e, d, content))
        .collect();
    e.upgrade_render_version_unstable(d).unwrap();
    assert_eq!(
        e.document(d).unwrap().meta.render_version,
        RENDER_VERSION_EXACT
    );
    for &id in &ids {
        assert!(
            bake_of(&e, d, id).is_some(),
            "upgrade bakes every text node"
        );
    }
    e.undo(d).unwrap();
    assert_eq!(
        e.document(d).unwrap().meta.render_version,
        alto_core::RENDER_VERSION
    );
    for &id in &ids {
        assert_eq!(
            bake_of(&e, d, id),
            None,
            "undo removes the bakes with the upgrade"
        );
    }
}

#[test]
fn text_bake_follows_font_replacement_in_the_same_transaction() {
    let (mut e, d, latin, jp) = document(RENDER_VERSION_EXACT);
    let content = texts(latin, jp).remove(0).1;
    let id = insert(&mut e, d, content);
    let before = bake_of(&e, d, id).unwrap();
    assert_eq!(decoded(&e, before).fonts[0].hash.to_string(), LATIN_BLAKE3);
    // Replace the Latin asset's bytes with the Japanese font under the same id.
    let jp_record = e.document(d).unwrap().assets[&jp].clone();
    let mut replacement = e.document(d).unwrap().assets[&latin].clone();
    replacement.hash = jp_record.hash;
    replacement.byte_len = jp_record.byte_len;
    let rev = e.document(d).unwrap().revision;
    let change = e
        .transact(
            d,
            "font.replace",
            vec![Op::SetAsset {
                id: latin,
                record: Some(replacement),
            }],
            rev,
        )
        .unwrap();
    assert!(
        change.nodes.contains(&id),
        "the re-bake is in the same transaction"
    );
    let after = bake_of(&e, d, id).unwrap();
    assert_ne!(after, before);
    assert_eq!(decoded(&e, after).fonts[0].hash.to_string(), JP_BLAKE3);
    // One undo restores both the asset and the old bake.
    e.undo(d).unwrap();
    assert_eq!(bake_of(&e, d, id), Some(before));
}

#[test]
fn text_bake_keeps_a_received_bake() {
    // A bake in an incoming op is bytes from its author, applied as it is.
    let (mut e, d, latin, jp) = document(RENDER_VERSION_EXACT);
    let content = texts(latin, jp).remove(2).1;
    let id = insert(&mut e, d, content.clone());
    let mut foreign = decoded(&e, bake_of(&e, d, id).unwrap());
    foreign.baker = 999;
    let hash = e.put_blob(foreign.encode());
    let rev = e.document(d).unwrap().revision;
    e.transact(
        d,
        "remote",
        vec![Op::SetText {
            id,
            content: Box::new(content),
            bake: Some(hash),
        }],
        rev,
    )
    .unwrap();
    assert_eq!(bake_of(&e, d, id), Some(hash));
}

#[test]
fn text_bake_placeholder_and_fixture() {
    let (mut e, d, latin, jp) = document(RENDER_VERSION_EXACT);
    let mut lines = vec![];
    for (name, content) in texts(latin, jp) {
        let id = insert(&mut e, d, content);
        let hash = bake_of(&e, d, id).unwrap();
        let bake = decoded(&e, hash);
        if name == "text-v3-placeholder" {
            let holder = bake
                .outlines
                .iter()
                .find(|o| o.font == PLACEHOLDER_FONT)
                .expect("the missing character bakes the placeholder");
            assert_eq!((holder.glyph, holder.upem), (0, 1000));
            assert_eq!(holder.commands[0], BakeCommand::Move([80 * 64, 0]));
        }
        lines.push(format!("{name} {hash}"));
    }
    // Native and wasm must bake these bytes (memo "依存の版"); a change needs a
    // BAKER_VERSION bump and a reason. Regenerate with ALTO_WRITE_PARITY=1.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/text-bake.txt"
    );
    let text = format!(
        "# blake3 of the render version 3 text bakes of the parity texts (BAKER_VERSION {BAKER_VERSION}). Regenerate with ALTO_WRITE_PARITY=1.\n{}\n",
        lines.join("\n")
    );
    if std::env::var_os("ALTO_WRITE_PARITY").is_some() {
        std::fs::write(path, &text).unwrap();
    }
    assert_eq!(std::fs::read_to_string(path).unwrap(), text);
}

/// A copy of the Latin font's record pointing at the Japanese font's bytes.
fn latin_as_jp(e: &Engine, d: DocHandle, latin: AssetId, jp: AssetId) -> alto_doc::AssetRecord {
    let jp_record = e.document(d).unwrap().assets[&jp].clone();
    let mut replacement = e.document(d).unwrap().assets[&latin].clone();
    replacement.hash = jp_record.hash;
    replacement.byte_len = jp_record.byte_len;
    replacement
}

#[test]
fn text_bake_uses_the_fonts_the_transaction_leaves() {
    let (mut e, d, latin, jp) = document(RENDER_VERSION_EXACT);
    let content = texts(latin, jp).remove(0).1;
    let id = insert(&mut e, d, content.clone());
    // A SetText before the font replacement still bakes with the new font.
    let replacement = latin_as_jp(&e, d, latin, jp);
    let rev = e.document(d).unwrap().revision;
    e.transact(
        d,
        "edit then replace",
        vec![
            Op::SetText {
                id,
                content: Box::new(content),
                bake: None,
            },
            Op::SetAsset {
                id: latin,
                record: Some(replacement),
            },
        ],
        rev,
    )
    .unwrap();
    let bake = decoded(&e, bake_of(&e, d, id).unwrap());
    assert_eq!(bake.fonts[0].hash.to_string(), JP_BLAKE3);
}

#[test]
fn text_bake_skips_nodes_the_transaction_removes() {
    let (mut e, d, latin, jp) = document(RENDER_VERSION_EXACT);
    let id = insert(&mut e, d, texts(latin, jp).remove(0).1);
    let replacement = latin_as_jp(&e, d, latin, jp);
    let rev = e.document(d).unwrap().revision;
    e.transact(
        d,
        "replace and delete",
        vec![
            Op::SetAsset {
                id: latin,
                record: Some(replacement),
            },
            Op::RemoveNode { id },
        ],
        rev,
    )
    .expect("no re-bake is added for a removed node");
    assert!(e.document(d).unwrap().node(id).is_err());
}

#[test]
fn text_bake_on_any_move_onto_version_three_and_on_restore() {
    // A bare SetMeta onto version 3 bakes the text it finds.
    let (mut e, d, latin, jp) = document(alto_core::RENDER_VERSION);
    let id = insert(&mut e, d, texts(latin, jp).remove(1).1);
    let mut meta = e.document(d).unwrap().meta.clone();
    meta.render_version = RENDER_VERSION_EXACT;
    meta.format_version = 4;
    let rev = e.document(d).unwrap().revision;
    e.transact(d, "meta", vec![Op::SetMeta { meta }], rev)
        .unwrap();
    assert!(bake_of(&e, d, id).is_some());
    // A restored subtree without a bake is baked.
    let node = e.document(d).unwrap().node(id).unwrap().clone();
    let (parent, rev) = (node.parent.unwrap(), e.document(d).unwrap().revision);
    e.transact(d, "remove", vec![Op::RemoveNode { id }], rev)
        .unwrap();
    let mut unbaked = node;
    if let NodeKind::Text { bake, .. } = &mut unbaked.kind {
        *bake = None;
    }
    let rev = e.document(d).unwrap().revision;
    e.transact(
        d,
        "restore",
        vec![Op::RestoreSubtree {
            nodes: vec![unbaked],
            parent,
            index: 0,
        }],
        rev,
    )
    .unwrap();
    let hash = bake_of(&e, d, id).expect("restored text is baked");
    assert!(e.blobs.contains_key(&hash));
}

#[test]
fn render_scenes_carry_the_bake_on_version_three_only() {
    use alto_scene::RContent;
    let (mut e, d, latin, jp) = document(RENDER_VERSION_EXACT);
    let id = insert(&mut e, d, texts(latin, jp).remove(0).1);
    let hash = bake_of(&e, d, id).unwrap();
    let scene = e.render_scene(d).unwrap();
    let node = scene.nodes.iter().find(|n| n.key == id).unwrap();
    let RContent::TextBake(Some(bake)) = &node.content else {
        panic!("a v3 text node renders from its bake")
    };
    assert_eq!(bake.hash, hash);
    assert_eq!(*bake.bytes, e.blobs[&hash]);
    // Version 2 keeps the layout runs.
    let (mut e, d, latin, jp) = document(alto_core::RENDER_VERSION);
    let id = insert(&mut e, d, texts(latin, jp).remove(0).1);
    let scene = e.render_scene(d).unwrap();
    let node = scene.nodes.iter().find(|n| n.key == id).unwrap();
    assert!(matches!(node.content, RContent::Text(_)));
}
