//! Times layer edits on a large image: `cargo run --release -p alto-engine --example image_bench`.
use alto_core::{PixelFormat, TileKey};
use alto_doc::{DocumentMeta, ImageBounds, Node, NodeKind, Op};
use alto_engine::Engine;
use alto_engine::image::{
    FlipAxis, GradientKind, GradientOptions, GradientStop, ImageEdit, ImageFilter, SelectionMode,
};
use alto_raster::{TileData, TileMap};
use std::time::Instant;

fn main() {
    let size = 4096u32;
    let mut e = Engine::new();
    let h = e
        .create_document(DocumentMeta::srgb_u8(size, size))
        .unwrap();
    let mut tiles = TileMap::new(PixelFormat::Rgba8);
    for ty in 0..(size / 256) as i32 {
        for tx in 0..(size / 256) as i32 {
            let mut pixels = vec![0u8; 262144];
            for (i, p) in pixels.chunks_exact_mut(4).enumerate() {
                p.copy_from_slice(&[
                    (i + tx as usize) as u8,
                    (i / 256 + ty as usize) as u8,
                    7,
                    255,
                ]);
            }
            let hash = e
                .put_tile(TileData::new(PixelFormat::Rgba8, pixels).unwrap())
                .unwrap();
            tiles.tiles.insert(TileKey { tx, ty }, hash);
        }
    }
    let mut node = Node::raster("big");
    let id = node.id;
    node.kind = NodeKind::Raster {
        bounds: ImageBounds {
            x: 0,
            y: 0,
            width: size,
            height: size,
        },
        tiles,
    };
    let root = e.document(h).unwrap().root;
    e.transact(
        h,
        "add",
        vec![Op::InsertNode {
            node,
            parent: root,
            index: 0,
        }],
        0,
    )
    .unwrap();
    let time = |e: &mut Engine, name: &str, edit: ImageEdit| {
        let t = Instant::now();
        let p = e.prepare_image_edit(h, edit).unwrap();
        // The hash of the output, to show that a faster build makes the same bytes.
        let Op::ReplaceNode { node, .. } = &p.ops[0] else {
            panic!()
        };
        let bytes = serde_json::to_vec(&node.kind).unwrap();
        println!(
            "{name:<24} {:>8.1} ms  {}",
            t.elapsed().as_secs_f64() * 1000.0,
            &alto_core::ContentHash::of(&bytes).hex()[..16]
        );
        e.release_staged_tiles(p.stage);
    };
    time(
        &mut e,
        "flip",
        ImageEdit::Flip {
            node: id,
            axis: FlipAxis::Horizontal,
        },
    );
    time(
        &mut e,
        "rotate",
        ImageEdit::Rotate {
            node: id,
            quarter_turns: 1,
        },
    );
    for (name, filter) in [
        ("resize nearest 1/2", ImageFilter::Nearest),
        ("resize bilinear 1/2", ImageFilter::Bilinear),
        ("resize catmull-rom 1/2", ImageFilter::CatmullRom),
        ("resize area 1/2", ImageFilter::Area),
    ] {
        time(
            &mut e,
            name,
            ImageEdit::Resize {
                node: id,
                width: size / 2,
                height: size / 2,
                filter,
            },
        );
    }
    time(
        &mut e,
        "gradient",
        ImageEdit::Gradient {
            node: id,
            options: GradientOptions {
                kind: GradientKind::Linear,
                start: [0.0, 0.0],
                end: [4096.0, 0.0],
                stops: vec![
                    GradientStop {
                        offset: 0.0,
                        color: [1.0, 0.0, 0.0, 0.5],
                    },
                    GradientStop {
                        offset: 1.0,
                        color: [0.0, 0.0, 1.0, 0.5],
                    },
                ],
                opacity: 1.0,
                selection: SelectionMode::Ignore,
            },
        },
    );
}
