use super::*;
use alto_doc::{DocumentMeta, Node};
fn setup() -> (Engine, DocHandle, NodeId) {
    let mut e = Engine::new();
    let h = e.create_document(DocumentMeta::srgb_u8(300, 10)).unwrap();
    let mut n = Node::raster("test");
    let id = n.id;
    let b = ImageBounds {
        x: 254,
        y: -1,
        width: 3,
        height: 2,
    };
    let mut data = BTreeMap::new();
    for (x, y, p) in [
        (254, -1, [255, 0, 0, 255]),
        (255, -1, [0, 255, 0, 255]),
        (256, -1, [0, 0, 255, 255]),
        (254, 0, [255, 255, 255, 255]),
    ] {
        write(&mut data, x, y, p);
    }
    let mut tiles = TileMap::new(PixelFormat::Rgba8);
    for (k, v) in data {
        let hash = e
            .put_tile(TileData::new(PixelFormat::Rgba8, v).unwrap())
            .unwrap();
        tiles.tiles.insert(k, hash);
    }
    n.kind = NodeKind::Raster { bounds: b, tiles };
    let root = e.document(h).unwrap().root;
    e.transact(
        h,
        "add",
        vec![Op::InsertNode {
            node: n,
            parent: root,
            index: 0,
        }],
        0,
    )
    .unwrap();
    (e, h, id)
}
fn edit(e: &mut Engine, h: DocHandle, request: ImageEdit) -> ImageEditResult {
    let intent = request.intent();
    let p = e.prepare_image_edit(h, request).unwrap();
    e.apply_image_edit(h, p, intent).unwrap()
}
#[test]
fn rotate_crop_flip_across_tiles_and_negative_origin_undo_save() {
    let (mut e, h, id) = setup();
    let before = e.snapshot(h).unwrap();
    let r = edit(
        &mut e,
        h,
        ImageEdit::Rotate {
            node: id,
            quarter_turns: 1,
        },
    );
    assert_eq!((r.bounds.width, r.bounds.height), (2, 3));
    assert_eq!(
        e.pick_color(h, id, [255.0, -1.0]).unwrap().bytes,
        [255, 0, 0, 255]
    );
    assert_eq!(
        e.pick_color(h, id, [255.0, 1.0]).unwrap().bytes,
        [0, 0, 255, 255]
    );
    e.undo(h).unwrap();
    assert_eq!(e.snapshot(h).unwrap(), before);
    let b = ImageBounds {
        x: 255,
        y: -1,
        width: 2,
        height: 1,
    };
    edit(
        &mut e,
        h,
        ImageEdit::Crop {
            node: id,
            bounds: b,
        },
    );
    assert!(e.pick_color(h, id, [254.0, -1.0]).is_err());
    let c = e.commit(h, "cropped", 1, None, false).unwrap();
    let bytes = e.save_package(h).unwrap();
    let (d, w) = e.open_package(&bytes).unwrap();
    assert!(w.is_empty());
    assert_eq!(e.version_status(d).unwrap().head, Some(c.hash));
    assert_eq!(
        e.pick_color(d, id, [256.0, -1.0]).unwrap().bytes,
        [0, 0, 255, 255]
    );
    e.undo(h).unwrap();
    assert_eq!(e.snapshot(h).unwrap(), before);
    edit(
        &mut e,
        h,
        ImageEdit::Flip {
            node: id,
            axis: FlipAxis::Horizontal,
        },
    );
    assert_eq!(
        e.pick_color(h, id, [254.0, -1.0]).unwrap().bytes,
        [0, 0, 255, 255]
    );
}
#[test]
fn resize_transparency_and_failed_stale_prepare_are_atomic() {
    let (mut e, h, id) = setup();
    edit(
        &mut e,
        h,
        ImageEdit::Resize {
            node: id,
            width: 1,
            height: 1,
            filter: ImageFilter::Bilinear,
        },
    );
    assert_eq!(
        e.pick_color(h, id, [254.0, -1.0]).unwrap().bytes,
        [0, 255, 0, 128]
    );
    let before = e.snapshot(h).unwrap();
    assert!(
        e.prepare_image_edit(
            h,
            ImageEdit::Resize {
                node: id,
                width: 65536,
                height: 65536,
                filter: ImageFilter::Nearest
            }
        )
        .is_err()
    );
    assert_eq!(e.snapshot(h).unwrap(), before);
    let prepared = e
        .prepare_image_edit(
            h,
            ImageEdit::Flip {
                node: id,
                axis: FlipAxis::Horizontal,
            },
        )
        .unwrap();
    e.undo(h).unwrap();
    assert!(e.apply_image_edit(h, prepared, "stale").is_err());
}
/// A one-row image of the given pixels at the origin.
fn row(pixels: &[[u8; 4]]) -> (Engine, DocHandle, NodeId) {
    let mut e = Engine::new();
    let h = e.create_document(DocumentMeta::srgb_u8(300, 10)).unwrap();
    let mut n = Node::raster("row");
    let id = n.id;
    let mut data = BTreeMap::new();
    for (x, p) in pixels.iter().enumerate() {
        write(&mut data, x as i32, 0, *p);
    }
    let mut tiles = TileMap::new(PixelFormat::Rgba8);
    for (k, v) in data {
        let hash = e
            .put_tile(TileData::new(PixelFormat::Rgba8, v).unwrap())
            .unwrap();
        tiles.tiles.insert(k, hash);
    }
    n.kind = NodeKind::Raster {
        bounds: ImageBounds {
            x: 0,
            y: 0,
            width: pixels.len() as u32,
            height: 1,
        },
        tiles,
    };
    let root = e.document(h).unwrap().root;
    e.transact(
        h,
        "add",
        vec![Op::InsertNode {
            node: n,
            parent: root,
            index: 0,
        }],
        0,
    )
    .unwrap();
    (e, h, id)
}
fn resize_row(pixels: &[[u8; 4]], width: u32, filter: ImageFilter) -> Vec<[u8; 4]> {
    let (mut e, h, id) = row(pixels);
    edit(
        &mut e,
        h,
        ImageEdit::Resize {
            node: id,
            width,
            height: 1,
            filter,
        },
    );
    (0..width)
        .map(|x| e.pick_color(h, id, [f64::from(x), 0.0]).unwrap().bytes)
        .collect()
}
#[test]
fn area_filter_averages_premultiplied_source_exactly() {
    let black = [0, 0, 0, 255];
    let white = [255, 255, 255, 255];
    assert_eq!(
        resize_row(&[black, white, black, white], 2, ImageFilter::Area),
        [[128, 128, 128, 255]; 2],
        "bilinear would alias this pattern"
    );
    // Three source pixels into two: 1.5 source pixels each, with exact weights.
    assert_eq!(
        resize_row(&[black, white, black], 2, ImageFilter::Area),
        [[85, 85, 85, 255]; 2]
    );
    // Transparent pixels do not darken the color.
    assert_eq!(
        resize_row(&[[255, 0, 0, 255], [0, 0, 0, 0]], 1, ImageFilter::Area),
        [[255, 0, 0, 128]]
    );
}
#[test]
fn catmull_rom_keeps_flat_areas_and_stays_in_range() {
    let flat = [[10, 20, 30, 255]; 4];
    assert_eq!(
        resize_row(&flat, 9, ImageFilter::CatmullRom),
        [[10, 20, 30, 255]; 9]
    );
    // Enlarging an edge overshoots only up to the clamp, and passes through source
    // values where the destination center lands exactly on a source center.
    let edge = [
        [0, 0, 0, 255],
        [0, 0, 0, 255],
        [255, 255, 255, 255],
        [255, 255, 255, 255],
    ];
    let up = resize_row(&edge, 12, ImageFilter::CatmullRom);
    assert_eq!(up[0], [0, 0, 0, 255]);
    assert_eq!(up[11], [255, 255, 255, 255]);
    assert!(up.windows(2).all(|w| w[0][3] == 255 && w[1][3] == 255));
    let bilinear = resize_row(&edge, 12, ImageFilter::Bilinear);
    assert_ne!(up, bilinear, "the kernels differ");
    // A half-transparent pixel never gets more color than its alpha allows.
    let soft = resize_row(
        &[[255, 0, 0, 255], [0, 0, 0, 0], [255, 0, 0, 255]],
        7,
        ImageFilter::CatmullRom,
    );
    assert!(soft.iter().all(|p| p[3] == 0 || p[0] == 255), "{soft:?}");
}
#[test]
fn filters_are_reproducible_and_undo_and_save() {
    let pixels: Vec<[u8; 4]> = (0..40u8).map(|i| [i * 6, 255 - i, i, 200 + i]).collect();
    for filter in [ImageFilter::CatmullRom, ImageFilter::Area] {
        assert_eq!(
            resize_row(&pixels, 13, filter),
            resize_row(&pixels, 13, filter)
        );
    }
    let (mut e, h, id) = row(&pixels);
    let before = e.snapshot(h).unwrap();
    edit(
        &mut e,
        h,
        ImageEdit::Resize {
            node: id,
            width: 13,
            height: 1,
            filter: ImageFilter::Area,
        },
    );
    let bytes = e.save_package(h).unwrap();
    assert!(Engine::new().open_package(&bytes).is_ok());
    e.undo(h).unwrap();
    assert_eq!(e.snapshot(h).unwrap(), before);
}
#[test]
fn fill_uses_four_neighbors_and_seed_comparison_and_journal() {
    let (mut e, h, id) = setup();
    let before = e.save_package(h).unwrap();
    let edit = ImageEdit::Fill {
        node: id,
        point: [256.0, 0.0],
        color: [1.0, 0.0, 1.0, 1.0],
        tolerance: 0,
        selection: SelectionMode::Ignore,
    };
    let p = e.prepare_image_edit(h, edit).unwrap();
    let ops = p.ops.clone();
    let result = e.apply_image_edit(h, p, "image.fill").unwrap();
    assert!(!result.no_op);
    assert_eq!(
        e.pick_color(h, id, [255.0, 0.0]).unwrap().bytes,
        [255, 0, 255, 255]
    );
    assert_eq!(
        e.pick_color(h, id, [256.0, -1.0]).unwrap().bytes,
        [0, 0, 255, 255]
    );
    let journal = e
        .journal_record(h, crate::JournalKind::Transact, "image.fill", &ops)
        .unwrap();
    let (mut f, d) = {
        let mut f = Engine::new();
        let (d, _) = f.open_package(&before).unwrap();
        (f, d)
    };
    f.replay_record(d, &journal).unwrap();
    assert_eq!(f.snapshot(d).unwrap(), e.snapshot(h).unwrap());
}
#[test]
fn gradient_is_premultiplied_and_empty_image_bounds_survive() {
    let (mut e, h, id) = setup();
    let options = GradientOptions {
        kind: GradientKind::Linear,
        start: [254.0, 0.0],
        end: [257.0, 0.0],
        stops: vec![
            GradientStop {
                offset: 0.0,
                color: [1.0, 0.0, 0.0, 0.0],
            },
            GradientStop {
                offset: 1.0,
                color: [0.0, 1.0, 0.0, 1.0],
            },
        ],
        opacity: 1.0,
        selection: SelectionMode::Ignore,
    };
    edit(&mut e, h, ImageEdit::Gradient { node: id, options });
    let px = e.pick_color(h, id, [255.0, 0.0]).unwrap().bytes;
    assert_eq!(px, [0, 255, 0, 128]);
    let b = ImageBounds {
        x: 256,
        y: 0,
        width: 1,
        height: 1,
    };
    e.undo(h).unwrap();
    edit(
        &mut e,
        h,
        ImageEdit::Crop {
            node: id,
            bounds: b,
        },
    );
    assert_eq!(e.pick_color(h, id, [256.0, 0.0]).unwrap().bytes, [0; 4]);
    let bytes = e.save_package(h).unwrap();
    let (d, _) = e.open_package(&bytes).unwrap();
    let NodeKind::Raster { bounds, .. } = e.document(d).unwrap().node(id).unwrap().kind else {
        panic!()
    };
    assert_eq!(bounds, b);
}
