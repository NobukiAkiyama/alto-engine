//! Editable vector artwork with strokes, masks and a group clipping path.
use alto_core::{Affine2, PixelFormat, TileKey};
use alto_doc::{Node, NodeKind, Op};
use alto_engine::Engine;
use alto_raster::{TileData, TileMap};
use alto_vector::{
    ClipPath, FillRule, LineCap, LineJoin, PathData, Stroke, StyledPath, VectorMask,
};
fn path(svg: &str, fill: Option<[f32; 4]>, stroke: Option<([f32; 4], f32)>) -> StyledPath {
    StyledPath {
        data: PathData::from_svg(svg).unwrap(),
        fill,
        stroke: stroke.map(|(color, width)| Stroke {
            color,
            width,
            cap: LineCap::Round,
            join: LineJoin::Round,
            miter_limit: 4.,
        }),
        fill_rule: FillRule::NonZero,
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut e = Engine::new();
    let h = e.create_document(alto_doc::DocumentMeta::srgb_u8(960, 640))?;
    let root = e.document(h)?.root;
    let mut bg = Node::raster("背景");
    let mut tiles = TileMap::new(PixelFormat::Rgba8);
    for y in 0..3 {
        for x in 0..4 {
            let mut pixels = vec![0; 262144];
            for py in 0..256 {
                for px in 0..256 {
                    let gx = x * 256 + px;
                    let gy = y * 256 + py;
                    let i = (py * 256 + px) * 4;
                    let grid = if gx % 64 == 0 || gy % 64 == 0 { 5 } else { 0 };
                    pixels[i..i + 4].copy_from_slice(&[17 + grid, 24 + grid, 42 + grid, 255]);
                }
            }
            let hash = e
                .put_tile(TileData::new(PixelFormat::Rgba8, pixels).expect("fixed tile size"))
                .unwrap();
            tiles.tiles.insert(
                TileKey {
                    tx: x as i32,
                    ty: y as i32,
                },
                hash,
            );
        }
    }
    bg.kind = NodeKind::Raster { tiles };
    let mut group = Node::group("クリッピングパスのグループ");
    group.props.clip_path = Some(Box::new(ClipPath {
        path: PathData::from_svg("M85 80 Q480 -10 875 80 V520 Q480 650 85 520 Z")?,
        fill_rule: FillRule::NonZero,
        enabled: true,
        transform: Affine2::default(),
    }));
    let gid = group.id;
    let mut shapes = Node::raster("曲線・塗り・線");
    shapes.kind = NodeKind::Vector {
        paths: vec![
            path(
                "M100 410 C170 80 370 70 480 350 C610 660 750 430 860 120 L920 620 H60 Z",
                Some([0.25, 0.78, 0.75, 1.]),
                Some(([0.65, 1., 0.92, 1.], 6.)),
            ),
            path(
                "M100 490 C230 210 430 190 510 390 C600 600 730 490 900 250 L920 640 H70 Z",
                Some([0.44, 0.38, 0.88, 0.9]),
                Some(([0.77, 0.72, 1., 1.], 5.)),
            ),
        ],
    };
    let sid = shapes.id;
    let mut ring = Node::raster("Even-odd リング");
    let mut p = path(
        "M735 205 A105 105 0 1 0 525 205 A105 105 0 1 0 735 205 Z M690 205 A60 60 0 1 0 570 205 A60 60 0 1 0 690 205 Z",
        Some([1., 0.7, 0.34, 1.]),
        Some(([1., 0.88, 0.62, 1.], 3.)),
    );
    p.fill_rule = FillRule::EvenOdd;
    ring.kind = NodeKind::Vector { paths: vec![p] };
    let mut glow = Node::raster("ベクターマスク付きの波");
    glow.kind = NodeKind::Vector {
        paths: vec![path(
            "M30 330 C170 210 280 500 430 280 S690 240 910 380",
            None,
            Some(([0.94, 0.99, 1., 1.], 14.)),
        )],
    };
    glow.props.vector_mask = Some(Box::new(VectorMask {
        path: PathData::from_svg("M120 80 H840 V580 H120 Z")?,
        fill_rule: FillRule::NonZero,
        feather: 24.,
        enabled: true,
        transform: Affine2::default(),
    }));
    let ops = vec![
        Op::InsertNode {
            node: bg,
            parent: root,
            index: 0,
        },
        Op::InsertNode {
            node: group,
            parent: root,
            index: 1,
        },
        Op::InsertNode {
            node: shapes,
            parent: gid,
            index: 0,
        },
        Op::InsertNode {
            node: ring,
            parent: gid,
            index: 1,
        },
        Op::InsertNode {
            node: glow,
            parent: gid,
            index: 2,
        },
    ];
    e.transact(h, "M6 sample", ops, 0)?;
    let output = std::env::args()
        .nth(1)
        .unwrap_or("verification/sample-m6.alto".into());
    let bytes = e.save_package(h)?;
    std::fs::write(&output, &bytes)?;
    let mut check = Engine::new();
    let (other, warnings) = check.open_package(&bytes)?;
    assert!(warnings.is_empty());
    assert_eq!(
        e.document(h)?.snapshot_bytes()?,
        check.document(other)?.snapshot_bytes()?
    );
    assert_eq!(
        e.document(h)?.node(sid)?.kind,
        check.document(other)?.node(sid)?.kind
    );
    println!("Saved and verified {output}: {} bytes", bytes.len());
    Ok(())
}
