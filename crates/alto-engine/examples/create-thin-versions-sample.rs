//! A thin `.alto` (manifest format 3): the working document's objects inside, the
//! versions' objects listed in `external-objects.json` and left outside. Synthetic
//! pixels only: one 256×256 tile per version, the second tile overwriting the first.
//! `cargo run -p alto-engine --example create-thin-versions-sample -- <out.alto>`.
//! The default output is `tests/fixtures/thin-versions.alto`, the `folder` and
//! `package` fuzz seed (QA-1).
use alto_core::{PixelFormat, TileKey};
use alto_doc::{DocumentMeta, Node, Op, TileChange};
use alto_engine::Engine;
use alto_raster::TileData;

fn paint(e: &mut Engine, d: alto_engine::DocHandle, layer: alto_core::NodeId, value: u8) {
    let tile = TileData::new(PixelFormat::Rgba8, vec![value; 262144]).unwrap();
    let after = e.put_tile(tile).unwrap();
    let doc = e.document(d).unwrap();
    let before = match &doc.node(layer).unwrap().kind {
        alto_doc::NodeKind::Raster { tiles, .. } => tiles.tiles.values().next().copied(),
        _ => panic!("raster layer"),
    };
    let revision = doc.revision;
    e.transact(
        d,
        "paint",
        vec![Op::SetTiles {
            layer,
            changes: vec![TileChange {
                key: TileKey { tx: 0, ty: 0 },
                before,
                after: Some(after),
            }],
        }],
        revision,
    )
    .unwrap();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "tests/fixtures/thin-versions.alto".to_string());
    let mut e = Engine::new();
    let d = e.create_document(DocumentMeta::srgb_u8(256, 256))?;
    let node = Node::raster("layer");
    let layer = node.id;
    let parent = e.document(d)?.root;
    e.transact(
        d,
        "layer",
        vec![Op::InsertNode {
            node,
            parent,
            index: 0,
        }],
        0,
    )?;
    paint(&mut e, d, layer, 10);
    e.commit(d, "old", 1, None, false)?;
    paint(&mut e, d, layer, 20);
    e.commit(d, "new", 2, None, false)?;
    let bytes = e.save_thin_package(d)?;
    std::fs::write(&out, &bytes)?;
    let entries = alto_store::read_entries(&bytes)?;
    let external = entries
        .get(alto_store::EXTERNAL_OBJECTS)
        .map(|b| serde_json::from_slice::<Vec<alto_core::ContentHash>>(b))
        .transpose()?
        .unwrap_or_default();
    println!(
        "{}: {} bytes, {} entries, {} external objects",
        out,
        bytes.len(),
        entries.len(),
        external.len()
    );
    Ok(())
}
