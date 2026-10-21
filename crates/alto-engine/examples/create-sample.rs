//! Creates an editable, self-contained .alto document without a browser.
use alto_core::{PixelFormat, TileKey};
use alto_doc::{DocumentMeta, Node, Op, TileChange};
use alto_engine::Engine;
use alto_raster::TileData;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sample.alto".into());
    let mut engine = Engine::new();
    let doc = engine.create_document(DocumentMeta::srgb_u8(768, 512))?;
    let root = engine.document(doc)?.root;
    for (index, name) in ["背景", "色のかたち", "ペイント"].iter().enumerate() {
        let node = Node::raster(*name);
        let id = node.id;
        let mut ops = vec![Op::InsertNode {
            node,
            parent: root,
            index,
        }];
        let mut changes = Vec::new();
        for ty in 0..2 {
            for tx in 0..3 {
                let mut pixels = vec![0u8; 256 * 256 * 4];
                for y in 0..256 {
                    for x in 0..256 {
                        let px = (tx * 256 + x) as f64;
                        let py = (ty * 256 + y) as f64;
                        let rgba = match index {
                            0 => {
                                let grid = if x % 64 < 1 || y % 64 < 1 { 7.0 } else { 0.0 };
                                [
                                    ((17.0 + px / 100.0) + grid) as u8,
                                    (29.0 + py / 70.0 + grid) as u8,
                                    (49.0 + px / 70.0 + grid) as u8,
                                    255,
                                ]
                            }
                            1 => {
                                let a = (px - 290.0).hypot(py - 245.0);
                                let b = (px - 475.0).hypot(py - 235.0);
                                if b < 110.0 {
                                    [255, 151, 92, 210]
                                } else if a < 160.0 {
                                    [53, 205, 187, 225]
                                } else {
                                    [0; 4]
                                }
                            }
                            _ => {
                                let curve = 270.0 + 90.0 * (px / 110.0).sin();
                                let distance = (py - curve).abs();
                                let alpha = ((1.0 - distance / 15.0).clamp(0.0, 1.0) * 240.0) as u8;
                                if px > 95.0 && px < 675.0 {
                                    [233, 244, 255, alpha]
                                } else {
                                    [0; 4]
                                }
                            }
                        };
                        let offset = (y * 256 + x) as usize * 4;
                        pixels[offset..offset + 4].copy_from_slice(&rgba);
                    }
                }
                if let Some(hash) = engine.put_tile(
                    TileData::new(PixelFormat::Rgba8, pixels)
                        .expect("sample tiles have fixed dimensions"),
                ) {
                    changes.push(TileChange {
                        key: TileKey { tx, ty },
                        before: None,
                        after: Some(hash),
                    });
                }
            }
        }
        ops.push(Op::SetTiles { layer: id, changes });
        let revision = engine.document(doc)?.revision;
        engine.transact(doc, "サンプルを作成", ops, revision)?;
    }
    let bytes = engine.save_package(doc)?;
    let mut fresh = Engine::new();
    let (loaded, warnings) = fresh.open_package(&bytes)?;
    assert!(warnings.is_empty());
    assert_eq!(engine.snapshot(doc)?, fresh.snapshot(loaded)?);
    std::fs::write(&path, bytes)?;
    println!("Created {path}");
    Ok(())
}
