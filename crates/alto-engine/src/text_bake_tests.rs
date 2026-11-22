//! Render version 3 text bakes (doc/design/2026-10-28-v3-integer-text.md,
//! implementation order 0). Text renders from its bake.
use super::*;
use crate::text::BAKER_VERSION;
use alto_core::text_bake::{BakeCommand, PLACEHOLDER_FONT, TextBake};
use alto_core::{AssetId, NodeId};
use alto_doc::Node;
use alto_text::{TextContent, TextFrame, TextStyle, Underline, UnderlineKind, WritingMode};

const JP: &[u8] = include_bytes!("../../../tests/fonts/AltoTestJP.otf");
const LATIN: &[u8] = include_bytes!("../../../tests/fonts/AltoTestLatin.ttf");
/// blake3 of the parity fonts (memo "字形の同一性").
const LATIN_BLAKE3: &str = "cecfaa71790a6d0aa8744ddfdb3cb9fd6fbac9e2f8e0db476827710df1e0dbc3";
const JP_BLAKE3: &str = "5e2213b3dfdcc9736b3bd951f51ec58935a3bde47bc4f9521838f042c0db8981";

fn document() -> (Engine, DocHandle, AssetId, AssetId) {
    let mut e = Engine::new();
    let d = e.create_document(DocumentMeta::srgb_u8(1024, 768)).unwrap();
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
        (
            "text-v3-hanging",
            TextContent {
                text: "あい。う".into(),
                style: style(vec![jp], 40.0),
                frame: TextFrame::Area {
                    width: 100.0,
                    height: 200.0,
                },
                hanging: true,
                ..Default::default()
            },
        ),
        (
            "text-v3-underline",
            TextContent {
                text: "Underline".into(),
                style: TextStyle {
                    fonts: vec![latin],
                    size: 37.5,
                    underline: Underline {
                        kind: UnderlineKind::Solid,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
        (
            "text-v3-underline-vertical",
            TextContent {
                text: "縦書き".into(),
                style: TextStyle {
                    fonts: vec![jp],
                    size: 40.0,
                    underline: Underline {
                        kind: UnderlineKind::Solid,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                writing_mode: WritingMode::VerticalRl,
                ..Default::default()
            },
        ),
        (
            "text-v3-skew",
            TextContent {
                text: "Alto".into(),
                style: TextStyle {
                    italic_skew: 13.2,
                    ..style(vec![latin], 37.5)
                },
                ..Default::default()
            },
        ),
        (
            "text-v3-skew-vertical",
            TextContent {
                text: "吾A12回".into(),
                style: TextStyle {
                    italic_skew: -24.75,
                    ..style(vec![latin, jp], 40.0)
                },
                writing_mode: WritingMode::VerticalRl,
                ..Default::default()
            },
        ),
        (
            "text-v3-vertical-rtl",
            TextContent {
                text: "吾אבג猫".into(),
                style: style(vec![latin, jp], 40.0),
                writing_mode: WritingMode::VerticalRl,
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
    let (mut e, d, latin, jp) = document();
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
fn rule_bakes_one_shared_outline() {
    let (mut e, d, latin, _) = document();
    let content = TextContent {
        text: "ABCD".into(),
        style: TextStyle {
            fonts: vec![latin],
            underline: Underline {
                kind: UnderlineKind::Solid,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let id = insert(&mut e, d, content);
    let bake = decoded(&e, bake_of(&e, d, id).unwrap());
    let rules: Vec<_> = bake
        .outlines
        .iter()
        .enumerate()
        .filter(|(_, o)| o.glyph == 0xFF02)
        .collect();
    assert_eq!(rules.len(), 1);
    assert_eq!(
        bake.glyphs
            .iter()
            .filter(|g| g.outline == rules[0].0 as u32)
            .count(),
        1
    );
}

#[test]
fn font_replacement_rebakes_locked_text_and_remains_undoable() {
    let (mut engine, doc, latin, jp) = document();
    let content = texts(latin, jp).remove(0).1;
    let id = insert(&mut engine, doc, content.clone());
    let before = bake_of(&engine, doc, id).unwrap();
    let rev = engine.document(doc).unwrap().revision;
    engine
        .transact(
            doc,
            "lock",
            vec![Op::SetProps {
                id,
                patch: alto_doc::PropPatch {
                    locked: Some(true),
                    ..Default::default()
                },
            }],
            rev,
        )
        .unwrap();
    let checkpoint = engine.save_package(doc).unwrap();
    let jp_record = engine.document(doc).unwrap().assets[&jp].clone();
    let mut replacement = engine.document(doc).unwrap().assets[&latin].clone();
    replacement.hash = jp_record.hash;
    replacement.byte_len = jp_record.byte_len;
    let ops = engine
        .bake_text_ops(
            doc,
            vec![Op::SetAsset {
                id: latin,
                record: Some(replacement),
            }],
        )
        .unwrap();
    let rev = engine.document(doc).unwrap().revision;
    engine
        .transact(doc, "font.replace", ops.clone(), rev)
        .unwrap();
    let after = bake_of(&engine, doc, id).unwrap();
    assert_ne!(before, after);
    let journal = engine
        .journal_record(doc, JournalKind::Transact, "font.replace", &ops)
        .unwrap();
    let mut recovered = Engine::new();
    let (recovered_doc, _) = recovered.open_package(&checkpoint).unwrap();
    assert_eq!(
        recovered.replay_record(recovered_doc, &journal).unwrap(),
        Replayed::Applied
    );
    assert_eq!(bake_of(&recovered, recovered_doc, id), Some(after));
    engine.undo(doc).unwrap();
    assert_eq!(bake_of(&engine, doc, id), Some(before));
    engine.redo(doc).unwrap();
    assert_eq!(bake_of(&engine, doc, id), Some(after));
    let rev = engine.document(doc).unwrap().revision;
    assert!(
        engine
            .transact(
                doc,
                "text.edit",
                vec![Op::SetText {
                    id,
                    content: Box::new(content),
                    bake: None
                }],
                rev
            )
            .is_err()
    );
    assert!(
        engine
            .transact(
                doc,
                "fake.bake",
                vec![Op::SetTextBake {
                    id,
                    bake: Some(before)
                }],
                rev
            )
            .is_err()
    );
}

#[test]
fn text_bake_follows_font_replacement_in_the_same_transaction() {
    let (mut e, d, latin, jp) = document();
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
    let (mut e, d, latin, jp) = document();
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
fn skew_zero_bakes_the_same_bytes() {
    let (mut engine, doc, latin, _) = document();
    let mut content = TextContent {
        text: "Alto".into(),
        style: TextStyle {
            fonts: vec![latin],
            ..Default::default()
        },
        ..Default::default()
    };
    let plain = insert(&mut engine, doc, content.clone());
    content.style.italic_skew = -0.0;
    let zero = insert(&mut engine, doc, content.clone());
    assert_eq!(bake_of(&engine, doc, plain), bake_of(&engine, doc, zero));

    content.style.italic_skew = 18.0;
    content.spans.push(alto_text::TextSpan {
        start: 0,
        end: 4,
        style: alto_text::StylePatch {
            italic_skew: Some(0.0),
            ..Default::default()
        },
    });
    let reset = insert(&mut engine, doc, content);
    assert_eq!(bake_of(&engine, doc, plain), bake_of(&engine, doc, reset));
}

#[test]
fn text_bake_placeholder_and_fixture() {
    let (mut e, d, latin, jp) = document();
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
    let (mut e, d, latin, jp) = document();
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
    let (mut e, d, latin, jp) = document();
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
fn text_bake_on_restore() {
    let (mut e, d, latin, jp) = document();
    let id = insert(&mut e, d, texts(latin, jp).remove(1).1);
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
fn render_scenes_carry_the_bake() {
    use alto_scene::RContent;
    let (mut e, d, latin, jp) = document();
    let id = insert(&mut e, d, texts(latin, jp).remove(0).1);
    let hash = bake_of(&e, d, id).unwrap();
    let scene = e.render_scene(d).unwrap();
    let node = scene.nodes.iter().find(|n| n.key == id).unwrap();
    let RContent::TextBake(Some(bake)) = &node.content else {
        panic!("a v3 text node renders from its bake")
    };
    assert_eq!(bake.hash, hash);
    assert_eq!(*bake.bytes, e.blobs[&hash]);
}

#[test]
fn new_format_document_saves_and_reopens() {
    let (mut e, d, _, jp) = document();
    let id = insert(
        &mut e,
        d,
        TextContent {
            text: "あい。う".into(),
            style: TextStyle {
                fonts: vec![jp],
                ..Default::default()
            },
            ..Default::default()
        },
    );
    e.commit(d, "before hanging", 1, None, false).unwrap();
    let mut content = match &e.document(d).unwrap().node(id).unwrap().kind {
        NodeKind::Text { content, .. } => content.clone(),
        _ => unreachable!(),
    };
    content.hanging = true;
    content.frame = TextFrame::Area {
        width: 80.0,
        height: 200.0,
    };
    let rev = e.document(d).unwrap().revision;
    e.transact(
        d,
        "text.edit",
        vec![Op::SetText {
            id,
            content,
            bake: None,
        }],
        rev,
    )
    .unwrap();
    e.commit(d, "hanging", 2, None, false).unwrap();
    let old_objects = e.version_only_objects(d).unwrap();
    assert!(!old_objects.is_empty());
    let check_manifest = |entries: &std::collections::BTreeMap<String, Vec<u8>>, format| {
        let m: alto_store::Manifest = serde_json::from_slice(&entries["manifest.json"]).unwrap();
        assert_eq!(m.format, format);
        assert_eq!(m.document_format, 6);
    };
    let mut fresh = Engine::new();
    let bytes = e.save_package(d).unwrap();
    check_manifest(&alto_store::read_entries(&bytes).unwrap(), 2);
    let package = fresh.open_package(&bytes).unwrap();
    let folder = e.folder_entries(d).unwrap();
    check_manifest(&folder, 2);
    let plan = e.prepare_gc(d, &folder, false).unwrap();
    check_manifest(&plan.entries, 2);
    let folder = fresh.open_entries(folder).unwrap();
    let thin = e.save_thin_package(d).unwrap();
    check_manifest(&alto_store::read_entries(&thin).unwrap(), 3);
    let mut thin_engine = Engine::new();
    let thin = thin_engine.open_package(&thin).unwrap();
    assert_eq!(thin_engine.external_objects(thin.0).unwrap(), old_objects);
    for (reopened, (h, warnings)) in [(&fresh, package), (&fresh, folder), (&thin_engine, thin)] {
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(reopened.document(h).unwrap().meta.format_version, 6);
        assert_eq!(reopened.text_info(h, id).unwrap().lines[0].end, 3);
        assert_eq!(
            reopened.text_bake(h, id).unwrap(),
            e.text_bake(d, id).unwrap()
        );
        assert!(!reopened.version_status(h).unwrap().changed);
    }
}

/// A two-face collection: face 0 is the Latin test font, face 1 the Japanese one.
fn collection() -> Vec<u8> {
    let faces = [LATIN, JP];
    let mut offsets = vec![];
    let mut at = 12 + 4 * faces.len();
    for face in faces {
        offsets.push(at);
        at += face.len().div_ceil(4) * 4;
    }
    let mut out = b"ttcf\0\x01\0\0".to_vec();
    out.extend((faces.len() as u32).to_be_bytes());
    for o in &offsets {
        out.extend((*o as u32).to_be_bytes());
    }
    for (face, base) in faces.iter().zip(&offsets) {
        let mut sfnt = face.to_vec();
        let tables = u16::from_be_bytes([sfnt[4], sfnt[5]]) as usize;
        for i in 0..tables {
            let p = 12 + i * 16 + 8;
            let offset = u32::from_be_bytes(sfnt[p..p + 4].try_into().unwrap()) + *base as u32;
            sfnt[p..p + 4].copy_from_slice(&offset.to_be_bytes());
        }
        sfnt.resize(face.len().div_ceil(4) * 4, 0);
        out.extend(sfnt);
    }
    out
}

fn info_json(e: &Engine, d: DocHandle, id: NodeId) -> String {
    serde_json::to_string(&e.text_info(d, id).unwrap()).unwrap()
}

#[test]
fn switching_the_face_of_a_collection_refreshes_the_layout() {
    let mut e = Engine::new();
    let d = e.create_document(DocumentMeta::srgb_u8(1024, 768)).unwrap();
    let font = e.import_font(collection(), 0).unwrap().record;
    let rev = e.document(d).unwrap().revision;
    e.transact(
        d,
        "font",
        vec![Op::SetAsset {
            id: font.id,
            record: Some(font.clone()),
        }],
        rev,
    )
    .unwrap();
    let id = insert(
        &mut e,
        d,
        TextContent {
            text: "AAAA".into(),
            style: TextStyle {
                fonts: vec![font.id],
                size: 40.0,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let face0 = info_json(&e, d, id);
    let mut second = font.clone();
    let alto_doc::AssetKind::Font { face_index, .. } = &mut second.kind else {
        panic!()
    };
    *face_index = 1;
    let rev = e.document(d).unwrap().revision;
    e.transact(
        d,
        "face",
        vec![Op::SetAsset {
            id: font.id,
            record: Some(second),
        }],
        rev,
    )
    .unwrap();
    // A fresh engine lays the same document out from scratch.
    let mut fresh = Engine::new();
    fresh.import_font(collection(), 0).unwrap();
    let r = fresh.open_snapshot(&e.snapshot(d).unwrap()).unwrap();
    let face1 = info_json(&fresh, r, id);
    assert_ne!(
        face0, face1,
        "the faces lay out alike; the test needs them to differ"
    );
    assert_eq!(info_json(&e, d, id), face1);
}

#[test]
fn a_layout_made_before_the_font_arrives_is_not_kept() {
    let (mut e, d, latin, jp) = document();
    let content = texts(latin, jp).remove(0).1;
    let id = insert(&mut e, d, content.clone());
    let mut fresh = Engine::new();
    let r = fresh.open_snapshot(&e.snapshot(d).unwrap()).unwrap();
    let missing = info_json(&fresh, r, id);
    fresh.import_font(LATIN.to_vec(), 0).unwrap();
    fresh.import_font(JP.to_vec(), 0).unwrap();
    assert_ne!(missing, info_json(&e, d, id));
    assert_eq!(info_json(&fresh, r, id), info_json(&e, d, id));
    // An edit after the bytes arrive bakes with the font, not a placeholder.
    let rev = fresh.document(r).unwrap().revision;
    fresh
        .transact(
            r,
            "edit",
            vec![Op::SetText {
                id,
                content: Box::new(content),
                bake: None,
            }],
            rev,
        )
        .unwrap();
    let bake = decoded(&fresh, bake_of(&fresh, r, id).unwrap());
    assert_eq!(bake.fonts[0].hash.to_string(), LATIN_BLAKE3);
}
