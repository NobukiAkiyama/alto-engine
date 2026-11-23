//! Backdrop adjustments share their sibling prefix (#198). The planned walk
//! must match the recursive walk pixel for pixel, at tile edges and at mixed
//! sampling levels, without rendering a sibling once per earlier blur.
use super::*;
use alto_core::{PixelFormat, TileKey};
use alto_scene::RMask;

const TILE: TileKey = TileKey { tx: 0, ty: 0 };

/// Renders `key` through the planned or the recursive sibling walk, with the
/// number of node renders it made.
fn walk(
    document: &RenderScene,
    store: &alto_raster::ObjectStore,
    key: (i32, i32, i32),
    reference: bool,
) -> Option<(Result<Vec<u8>, String>, usize)> {
    let (rendered, renders) =
        crate::renderer::with_sibling_walk(reference, || v3_test_try_render(document, store, key));
    rendered.map(|r| (r.map_err(|e| e.to_string()), renders))
}

fn blur(radius: f32) -> REffectKind {
    REffectKind::GaussianBlur {
        radius,
        radius_q8: alto_core::blur::blur_radius_q8_f32(radius).unwrap(),
    }
}

fn node(content: RContent, features: RFeatures) -> RNode {
    let mut node = scene(content, features).nodes.remove(0);
    node.key = NodeId::new();
    node
}

fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64, fill: [f32; 4]) -> RNode {
    node(
        RContent::Mesh(std::sync::Arc::new(vec![test_rectangle(
            x0, y0, x1, y1, fill,
        )])),
        RFeatures::default(),
    )
}

/// A raster that reaches the right edge of tile (0, 0), so blurs spill into
/// tile (1, 0), and a horizontal gradient mask over that tile.
fn store() -> (alto_raster::ObjectStore, alto_core::ContentHash, RMask) {
    let mut bytes = vec![0u8; 256 * 256 * 4];
    for y in 40..220 {
        for x in 160..256 {
            bytes[(y * 256 + x) * 4..][..4].copy_from_slice(&[x as u8, 255 - y as u8, 96, 255]);
        }
    }
    let mut store = alto_raster::ObjectStore::default();
    let raster = store.insert_tile(&alto_raster::TileData::new(PixelFormat::Rgba8, bytes).unwrap());
    let gradient = (0..256 * 256).map(|i| (i % 256) as u8).collect();
    let mask = store.insert_tile(&alto_raster::TileData::new(PixelFormat::R8, gradient).unwrap());
    let mask = RMask {
        tiles: vec![crate::RenderTile {
            key: TILE,
            hash: mask,
        }],
        linked: true,
        transform: ident(),
    };
    (store, raster, mask)
}

/// A root group of `nodes`, the first child at index 1.
fn document(nodes: Vec<RNode>, children: Vec<usize>) -> RenderScene {
    let mut document = scene(
        RContent::Group {
            children,
            isolated: false,
        },
        RFeatures::default(),
    );
    document.canvas.x1 = 512;
    document.canvas.y1 = 512;
    document.nodes.extend(nodes);
    document.render_version = alto_core::RENDER_VERSION_EXACT;
    document
}

fn raster(hash: alto_core::ContentHash, effects: Vec<REffect>) -> RNode {
    node(
        RContent::Tiles(vec![crate::RenderTile { key: TILE, hash }]),
        RFeatures {
            effects,
            ..RFeatures::default()
        },
    )
}

/// Tiles across the raster's right edge at three levels: the level 0 tile and
/// its right neighbour, a level -2 tile right of the raster (halo only), and a
/// level -4 tile on the edge at x = 160, where the coarse blurs resample.
const TILES: [(i32, i32, i32); 4] = [(0, 0, 0), (0, 1, 0), (-2, 4, 2), (-4, 10, 8)];

/// Twelve backdrop blurs, each mixed differently so that none can share an
/// effect stack with its neighbour.
fn blur_chain(raster_hash: alto_core::ContentHash, mask: &RMask, count: usize) -> RenderScene {
    let mut nodes = vec![raster(raster_hash, vec![])];
    for i in 0..count {
        let mut adjustment = v3_adjustment(
            blur([1.0, 3.0, 16.0, 6.0][i % 4]),
            RFeatures {
                mask: (i % 3 == 1).then(|| mask.clone()),
                ..RFeatures::default()
            },
        );
        if i % 3 == 2 {
            adjustment.opacity = 0.75;
        }
        nodes.push(adjustment);
    }
    let children = (1..=nodes.len()).collect();
    document(nodes, children)
}

#[test]
fn backdrop_blur_chain_renders_each_sibling_once_per_level() {
    let (store, raster_hash, mask) = store();
    // The recursive walk renders the raster 2^n times. Five blurs keep it fast.
    let short = blur_chain(raster_hash, &mask, 5);
    assert_eq!(crate::renderer::v3_gap(&short), None);
    for key in TILES {
        let Some((planned, planned_renders)) = walk(&short, &store, key, false) else {
            return;
        };
        let (recursive, recursive_renders) = walk(&short, &store, key, true).unwrap();
        let planned = planned.expect("planned blur chain");
        assert!(planned == recursive.unwrap(), "tile {key:?}");
        // One raster render per sampling level, at most one per blur radius.
        assert!(planned_renders <= 4, "tile {key:?}: {planned_renders}");
        assert!(recursive_renders >= 32, "tile {key:?}: {recursive_renders}");
    }
    let long = blur_chain(raster_hash, &mask, 12);
    for key in TILES {
        let (planned, renders) = walk(&long, &store, key, false).unwrap();
        planned.expect("twelve backdrop blurs fit the command budget");
        assert!(renders <= 4, "tile {key:?}: {renders}");
    }
}

#[test]
fn backdrop_blur_chain_matches_the_layer_stack_across_tiles() {
    // An unmasked normal adjustment at opacity 1 replaces its backdrop, so a
    // chain of them over one raster is that raster's own effect stack. That
    // stack's halo comes from `input_roi_v3`, independently of the planner.
    let (store, raster_hash, _) = store();
    let radii = [2.0, 7.0, 16.0, 1.0, 40.0, 3.0];
    let mut nodes = vec![raster(raster_hash, vec![])];
    nodes.extend(
        radii
            .iter()
            .map(|&radius| v3_adjustment(blur(radius), RFeatures::default())),
    );
    let backdrop = document(nodes, (1..=radii.len() + 1).collect());
    let effects = radii
        .iter()
        .map(|&radius| REffect {
            kind: blur(radius),
            selection: None,
        })
        .collect();
    let layer = document(vec![raster(raster_hash, effects)], vec![1]);
    for version in [1, 2, 3] {
        let mut backdrop = backdrop.clone();
        let mut layer = layer.clone();
        backdrop.render_version = version;
        layer.render_version = version;
        for key in TILES {
            let Some((actual, renders)) = walk(&backdrop, &store, key, false) else {
                return;
            };
            let actual = actual.expect("backdrop blur chain");
            let (recursive, _) = walk(&backdrop, &store, key, true).unwrap();
            assert!(
                actual == recursive.unwrap(),
                "render version {version}, tile {key:?}"
            );
            // Versions 1 and 2 round the f16 backdrop differently from a
            // layer's own stack (one unit at some edges), so only version 3
            // is exact against it.
            if version == 3 {
                let (layer, _) = walk(&layer, &store, key, false).unwrap();
                assert!(actual == layer.unwrap(), "tile {key:?}");
            }
            assert!(
                renders <= 4,
                "render version {version}, tile {key:?}: {renders}"
            );
            if key.1 == 1 {
                // The neighbour tile holds only the halo spilled across x = 256.
                assert!(actual.chunks(4).any(|pixel| pixel[3] != 0));
            }
        }
    }
}

#[test]
fn mixed_backdrop_adjustments_match_the_recursive_walk() {
    let (store, raster_hash, mask) = store();
    let mut nodes = vec![raster(raster_hash, vec![])];
    let add = |nodes: &mut Vec<RNode>, node: RNode| {
        nodes.push(node);
        nodes.len()
    };
    let mut children = vec![1];
    // Translucent blur, then a layer between two backdrop blurs.
    let mut translucent = v3_adjustment(blur(2.0), RFeatures::default());
    translucent.opacity = 0.5;
    children.push(add(&mut nodes, translucent));
    children.push(add(
        &mut nodes,
        rectangle(120.0, 100.0, 300.0, 140.0, [0.0, 0.5, 1.0, 0.75]),
    ));
    // A masked blur coarse enough to sample at a coarser level when zoomed in.
    children.push(add(
        &mut nodes,
        v3_adjustment(
            blur(16.0),
            RFeatures {
                mask: Some(mask.clone()),
                ..RFeatures::default()
            },
        ),
    ));
    // A pointwise adjustment between blurs.
    let mut brighten = v3_adjustment(
        REffectKind::ColorAdjust {
            brightness: 0.1,
            contrast: 1.2,
            saturation: 0.8,
        },
        RFeatures::default(),
    );
    brighten.opacity = 0.6;
    children.push(add(&mut nodes, brighten));
    // A selected blur at a third level, which also keeps the fine original.
    let mut selected = v3_adjustment(blur(40.0), RFeatures::default());
    let RContent::Adjustment(effect) = &mut selected.content else {
        unreachable!()
    };
    effect.selection = Some(mask.clone());
    children.push(add(&mut nodes, selected));
    // A clipping run: base, clipped blur, clipped layer, translucent clipped blur.
    children.push(add(
        &mut nodes,
        rectangle(150.0, 20.0, 280.0, 90.0, [1.0, 1.0, 0.0, 1.0]),
    ));
    let clipped = RFeatures {
        clip: true,
        ..RFeatures::default()
    };
    children.push(add(&mut nodes, v3_adjustment(blur(6.0), clipped.clone())));
    let mut clipped_layer = rectangle(200.0, 0.0, 230.0, 256.0, [1.0, 0.0, 1.0, 1.0]);
    clipped_layer.features.clip = true;
    children.push(add(&mut nodes, clipped_layer));
    let mut clipped_blur = v3_adjustment(blur(3.0), clipped);
    clipped_blur.opacity = 0.6;
    children.push(add(&mut nodes, clipped_blur));
    // An invisible blur does nothing; an isolated group blurs only its own content.
    let mut invisible = v3_adjustment(blur(5.0), RFeatures::default());
    invisible.visible = false;
    children.push(add(&mut nodes, invisible));
    let inner_raster = add(&mut nodes, raster(raster_hash, vec![]));
    let inner_blur = add(&mut nodes, v3_adjustment(blur(4.0), RFeatures::default()));
    let mut group = node(
        RContent::Group {
            children: vec![inner_raster, inner_blur],
            isolated: true,
        },
        RFeatures::default(),
    );
    group.transform[4] = 48.0;
    group.opacity = 0.5;
    children.push(add(&mut nodes, group));
    // A last blur under a blend mode.
    let mut screen = v3_adjustment(blur(1.0), RFeatures::default());
    screen.blend = RenderBlend::Screen;
    children.push(add(&mut nodes, screen));
    let document = document(nodes, children);
    assert_eq!(crate::renderer::v3_gap(&document), None);
    for version in [1, 2, 3] {
        let mut document = document.clone();
        document.render_version = version;
        for key in TILES {
            let Some((planned, planned_renders)) = walk(&document, &store, key, false) else {
                return;
            };
            let (recursive, recursive_renders) = walk(&document, &store, key, true).unwrap();
            if version == 3 {
                assert!(planned.is_ok(), "tile {key:?}: {planned:?}");
            }
            assert!(
                planned == recursive,
                "render version {version}, tile {key:?}"
            );
            assert!(
                planned_renders < recursive_renders,
                "render version {version}, tile {key:?}: {planned_renders} >= {recursive_renders}"
            );
        }
    }
}
