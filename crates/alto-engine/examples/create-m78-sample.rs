//! Japanese vertical and horizontal text with embedded OFL fonts and a short version
//! history (three commits). Writes verification/sample-m78.alto and, when a second path
//! is given, the same document in the folder form.
use alto_core::{Affine2, AssetId, PixelFormat, TileKey};
use alto_doc::{Node, NodeKind, Op, PropPatch};
use alto_engine::Engine;
use alto_raster::{TileData, TileMap};
use alto_text::{
    FontFeature, StylePatch, TextAlign, TextContent, TextFrame, TextRange, TextSpan, TextStyle,
    WritingMode,
};

const JP: &[u8] = include_bytes!("../../../tests/fonts/AltoTestJP.otf");
const LATIN: &[u8] = include_bytes!("../../../tests/fonts/AltoTestLatin.ttf");
const PAPER: [f32; 4] = [0.15, 0.13, 0.11, 1.0];

fn style(fonts: &[AssetId], size: f32) -> TextStyle {
    TextStyle {
        fonts: fonts.to_vec(),
        size,
        color: PAPER,
        line_height: 1.75,
        ..Default::default()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut e = Engine::new();
    let h = e.create_document(alto_doc::DocumentMeta::srgb_u8(1200, 800))?;
    let root = e.document(h)?.root;
    let jp = e.import_font(JP.to_vec(), 0)?;
    let latin = e.import_font(LATIN.to_vec(), 0)?;
    let fonts = [jp.record.id, latin.record.id];

    let mut bg = Node::raster("紙");
    let mut tiles = TileMap::new(PixelFormat::Rgba8);
    for y in 0..4 {
        for x in 0..5 {
            let mut pixels = vec![0; 262144];
            for p in pixels.as_chunks_mut::<4>().0 {
                p.copy_from_slice(&[246, 241, 228, 255]);
            }
            let hash = e
                .put_tile(TileData::new(PixelFormat::Rgba8, pixels).expect("fixed tile size"))
                .unwrap();
            tiles.tiles.insert(TileKey { tx: x, ty: y }, hash);
        }
    }
    bg.kind = NodeKind::Raster { tiles };
    let bg_id = bg.id;

    let body = TextContent {
        text: "吾輩は猫である。名前はまだ無い。\nどこで生れたかとんと見当がつかぬ。何でも薄暗いじめじめした所でニャーニャー泣いていた事だけは記憶している。\n令和8年10月19日、第12回「縦組み」の確認。Alto Engine".into(),
        style: style(&fonts, 34.0),
        frame: TextFrame::Area { width: 620.0, height: 600.0 },
        writing_mode: WritingMode::VerticalRl,
        align: TextAlign::Justify,
        spans: vec![TextSpan {
            start: 0,
            end: 7,
            style: StylePatch {
                color: Some([0.64, 0.12, 0.10, 1.0]),
                ..Default::default()
            },
        }],
        ..Default::default()
    };
    let mut vertical = Node::text("本文（縦組み）", body);
    vertical.props.transform = Affine2([1.0, 0.0, 0.0, 1.0, 520.0, 90.0]);
    let title = TextContent {
        text: "縦組みと横組み — Alto Engine M7".into(),
        style: TextStyle {
            features: vec![FontFeature {
                tag: "palt".into(),
                value: 1,
            }],
            ..style(&fonts, 40.0)
        },
        ..Default::default()
    };
    let mut horizontal = Node::text("見出し（横組み）", title);
    horizontal.props.transform = Affine2([1.0, 0.0, 0.0, 1.0, 60.0, 60.0]);
    let note = TextContent {
        text: "記録2026年\n約物「、。」".into(),
        style: style(&fonts, 30.0),
        writing_mode: WritingMode::VerticalRl,
        tcy: vec![TextRange { start: 2, end: 6 }],
        ..Default::default()
    };
    let mut side = Node::text("注記", note);
    side.props.transform = Affine2([1.0, 0.0, 0.0, 1.0, 360.0, 160.0]);
    let (vid, hid) = (vertical.id, horizontal.id);
    e.transact(
        h,
        "フォントと紙",
        vec![
            Op::SetAsset {
                id: jp.record.id,
                record: Some(jp.record.clone()),
            },
            Op::SetAsset {
                id: latin.record.id,
                record: Some(latin.record.clone()),
            },
            Op::InsertNode {
                node: bg,
                parent: root,
                index: 0,
            },
        ],
        0,
    )?;
    let c1 = e.commit(
        h,
        "紙とフォントを用意",
        1_792_335_600_000,
        Some("Alto".into()),
        false,
    )?;
    let rev = e.document(h)?.revision;
    e.transact(
        h,
        "テキスト追加",
        vec![
            Op::InsertNode {
                node: vertical,
                parent: root,
                index: 1,
            },
            Op::InsertNode {
                node: horizontal,
                parent: root,
                index: 2,
            },
        ],
        rev,
    )?;
    let c2 = e.commit(
        h,
        "本文と見出しを組む",
        1_792_336_200_000,
        Some("Alto".into()),
        false,
    )?;
    let rev = e.document(h)?.revision;
    e.transact(
        h,
        "注記と紙の色",
        vec![
            Op::InsertNode {
                node: side,
                parent: root,
                index: 3,
            },
            Op::SetProps {
                id: bg_id,
                patch: PropPatch {
                    opacity: Some(0.92),
                    ..Default::default()
                },
            },
        ],
        rev,
    )?;
    let c3 = e.commit(
        h,
        "縦中横の注記を追加",
        1_792_336_800_000,
        Some("Alto".into()),
        false,
    )?;

    let output = std::env::args()
        .nth(1)
        .unwrap_or("verification/sample-m78.alto".into());
    let bytes = e.save_package(h)?;
    std::fs::write(&output, &bytes)?;
    let mut check = Engine::new();
    let (other, warnings) = check.open_package(&bytes)?;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(e.snapshot(h)?, check.snapshot(other)?);
    let log: Vec<_> = check.log(other)?.into_iter().map(|c| c.hash).collect();
    assert_eq!(log, vec![c3.hash, c2.hash, c1.hash]);
    for id in [vid, hid] {
        let info = check.text_info(other, id)?;
        assert!(
            info.missing.is_empty(),
            "sample text must be covered by the test fonts"
        );
    }
    if let Some(folder) = std::env::args().nth(2) {
        alto_store::folder::write(std::path::Path::new(&folder), &e.folder_entries(h)?, None)?;
    }
    println!(
        "Saved and verified {output}: {} bytes, {} versions",
        bytes.len(),
        log.len()
    );
    Ok(())
}
