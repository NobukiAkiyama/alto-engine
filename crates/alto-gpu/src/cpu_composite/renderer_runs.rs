//! Cold and warm cache runs of renderer cases on the GPU.
use super::*;

/// A display tile `(level, tx, ty)` and the refusal it must give, if any.
pub(super) type ColdWarmKey = ((i32, i32, i32), Option<&'static str>);

/// A scene change for [`renderer_cold_warm`]: `before` and `after` share the
/// canvas and node identities; `keys` are the display tiles read after it,
/// each with the refusal it must give (`None`: drawn, equal to the CPU).
pub(super) struct ColdWarmCase {
    pub(super) name: &'static str,
    pub(super) before: RendererCase,
    pub(super) after: RendererCase,
    pub(super) keys: Vec<ColdWarmKey>,
}

/// The scene changes of [`renderer_cold_warm`] (Linus 04:40 item 5): the
/// large-translation-out-of-range layer, the 2^23 boundary (drawn, refused, and
/// a far layer at the refused origin), a moved layer read at levels 0 and 1,
/// a source tile added where the f64 and integer selections part (the
/// 999936 case), and a masked M5 scene.
pub(super) fn cold_warm_cases() -> Vec<ColdWarmCase> {
    let blank = |canvas: [i32; 2]| RendererCase {
        name: String::new(),
        level: 0,
        key: [0, 0],
        canvas,
        layers: vec![],
        refused: None,
    };
    let with = |canvas: [i32; 2], layers: Vec<RendererLayer>| RendererCase {
        layers,
        ..blank(canvas)
    };
    let raster = |tiles: Vec<([i32; 2], TileSpec)>, transform: [f64; 6]| RendererLayer::Raster {
        tiles,
        transform,
        adjust: None,
        selection: None,
        mask: None,
        opacity: 0.8,
        blend: RenderBlend::Normal,
    };
    let (s, c) = 0.3f64.sin_cos();
    let rotated = [c, s, -s, c, 300.25, 17.5];
    let far = [0.9999900000999989, 0.0, 0.0, 1.0, 11.00180998190018, 0.0];
    let edge = [3908 * 256, 256];
    let boundary = [32769 * 256, 256];
    let masked = renderer_mask_cases().swap_remove(1);
    // The browser's 2^23 document (Linus 08:10): the empty v3 fixture 8388864 +
    // 256 px wide, opened through `open_package` (the `engine.open` path), with
    // one 256x256 raster moved by a translation, as the browser test imports it.
    let wide = {
        let bytes = include_bytes!("../../../../tests/fixtures/render-v3-empty-8389120x256.alto");
        let mut engine = alto_engine::Engine::new();
        let (doc, warnings) = engine.open_package(bytes).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let scene = engine.render_scene(doc).unwrap();
        assert_eq!(scene.render_version, alto_core::RENDER_VERSION_EXACT);
        [scene.canvas.x1, scene.canvas.y1]
    };
    assert_eq!(wide, [8_389_120, 256]);
    let moved = |x: f64| {
        with(
            wide,
            vec![RendererLayer::Raster {
                tiles: vec![([0, 0], TileSpec::Raster(8))],
                transform: [1.0, 0.0, 0.0, 1.0, x, 0.0],
                adjust: None,
                selection: None,
                mask: None,
                opacity: 1.0,
                blend: RenderBlend::Normal,
            }],
        )
    };
    vec![
        ColdWarmCase {
            name: "wide fixture: plain tile 32767 draws",
            before: blank(wide),
            after: moved(8_388_352.0),
            keys: vec![((0, 32_767, 0), None)],
        },
        ColdWarmCase {
            name: "wide fixture: plain tile 32768 with a contribution is refused",
            before: blank(wide),
            after: moved(8_388_608.0),
            keys: vec![(
                (0, 32_768, 0),
                Some("v3 draw ROI has no integer tile range"),
            )],
        },
        ColdWarmCase {
            name: "wide fixture: plain tile 32768 without a contribution is transparent",
            before: blank(wide),
            after: moved(8_388_352.0),
            keys: vec![((0, 32_768, 0), None)],
        },
        ColdWarmCase {
            name: "large-translation-out-of-range",
            before: blank([256, 256]),
            after: with(
                [256, 256],
                vec![raster(
                    vec![([0, 0], TileSpec::Raster(1))],
                    [0.01, 0.0, 0.0, 0.01, 84_000.0, 0.0],
                )],
            ),
            keys: vec![((0, 0, 0), None)],
        },
        ColdWarmCase {
            name: "2^23 boundary",
            before: blank(boundary),
            after: with(
                boundary,
                vec![
                    raster(vec![([32_767, 0], TileSpec::Raster(2))], ident()),
                    raster(vec![([32_768, 0], TileSpec::Raster(3))], ident()),
                ],
            ),
            keys: vec![
                ((0, 32_767, 0), None),
                (
                    (0, 32_768, 0),
                    Some("v3 draw ROI has no integer tile range"),
                ),
            ],
        },
        ColdWarmCase {
            name: "far layer at the 2^23 origin",
            before: blank(boundary),
            after: with(
                boundary,
                vec![raster(vec![([0, 0], TileSpec::Raster(4))], ident())],
            ),
            keys: vec![((0, 32_768, 0), None), ((0, 0, 0), None)],
        },
        ColdWarmCase {
            name: "moved layer",
            before: with(
                [1024, 512],
                vec![raster(
                    (0..=2).map(|x| ([x, 0], TileSpec::Raster(x))).collect(),
                    [1.0, 0.0, 0.0, 1.0, 0.5, 0.25],
                )],
            ),
            after: with(
                [1024, 512],
                vec![raster(
                    (0..=2).map(|x| ([x, 0], TileSpec::Raster(x))).collect(),
                    rotated,
                )],
            ),
            keys: vec![
                ((0, 0, 0), None),
                ((0, 1, 0), None),
                ((0, 2, 0), None),
                ((0, 1, 1), None),
                ((1, 0, 0), None),
            ],
        },
        ColdWarmCase {
            name: "source tile added at the 999936 edge",
            before: with(
                edge,
                vec![raster(
                    vec![
                        ([3905, 0], TileSpec::Raster(5)),
                        ([3906, 0], TileSpec::Raster(6)),
                    ],
                    far,
                )],
            ),
            after: with(
                edge,
                vec![raster(
                    vec![
                        ([3905, 0], TileSpec::Raster(5)),
                        ([3906, 0], TileSpec::Raster(6)),
                        ([3907, 0], TileSpec::Opaque(7)),
                    ],
                    far,
                )],
            ),
            keys: vec![((0, 3906, 0), None)],
        },
        ColdWarmCase {
            name: "masked M5",
            before: blank(RendererCase::from(&masked).canvas),
            after: RendererCase::from(&masked),
            keys: vec![((0, masked.key[0], masked.key[1]), None)],
        },
    ]
}

/// Linus 04:40 item 5: each [`cold_warm_cases`] scene read warm (the tiles of
/// `before` cached, then `scene_damage` applied) and cold (caches reset). Every
/// pair, per key: the warm and cold results.
pub(super) type ColdWarm = Vec<(
    Result<Vec<u8>, crate::RenderError>,
    Result<Vec<u8>, crate::RenderError>,
)>;

pub(super) fn renderer_cold_warm(cases: &[ColdWarmCase]) -> Result<Vec<ColdWarm>, String> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::METAL | wgpu::Backends::VULKAN | wgpu::Backends::DX12;
    let instance = wgpu::Instance::new(descriptor);
    let mut renderer = pollster::block_on(crate::renderer::Renderer::headless(&instance, 256))
        .map_err(|e| format!("headless renderer: {e}"))?;
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let poller = {
        let (device, done) = (renderer.device.clone(), done.clone());
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                let _ = device.poll(wgpu::PollType::Poll);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    let out = cases
        .iter()
        .map(|case| {
            let ids: Vec<NodeId> = (0..=case.before.layers.len().max(case.after.layers.len()))
                .map(|_| NodeId::new())
                .collect();
            let mut store = alto_raster::ObjectStore::default();
            let before = renderer_scene(&case.before, &mut store, &ids);
            let after = renderer_scene(&case.after, &mut store, &ids);
            let read = |r: &mut crate::renderer::Renderer, scene: &RenderScene, key| {
                pollster::block_on(r.read_tile(scene, &store, key))
            };
            renderer.reset_render_cache();
            for (key, _) in &case.keys {
                let _ = read(&mut renderer, &before, *key);
            }
            renderer.invalidate_damage(&crate::damage::scene_damage(&before, &after));
            let warm: Vec<_> = case
                .keys
                .iter()
                .map(|(k, _)| read(&mut renderer, &after, *k))
                .collect();
            let cold: Vec<_> = case
                .keys
                .iter()
                .map(|(k, _)| {
                    renderer.reset_render_cache();
                    read(&mut renderer, &after, *k)
                })
                .collect();
            warm.into_iter().zip(cold).collect()
        })
        .collect();
    done.store(true, std::sync::atomic::Ordering::Release);
    poller
        .join()
        .map_err(|_| "device poller panicked".to_string())?;
    Ok(out)
}

/// One render result per [`RendererCase`].
pub(super) type RendererResults = Vec<Result<Vec<u8>, crate::RenderError>>;

/// [`RendererCase`]s composed by one headless `Renderer` (caches reset between
/// cases) and read back, each with its render result.
/// The render scene of a [`RendererCase`], its tiles inserted into `store`.
/// `ids` (root first, then one per layer) keep node identities across two
/// scenes, so their `scene_damage` sees moves and edits rather than new nodes.
pub(super) fn renderer_scene(
    case: &RendererCase,
    store: &mut alto_raster::ObjectStore,
    ids: &[NodeId],
) -> RenderScene {
    use alto_core::{PixelFormat, TileKey};
    use alto_raster::{ObjectStore, TileData};
    let tiles = |store: &mut ObjectStore, format, list: &[([i32; 2], TileSpec)]| {
        list.iter()
            .map(|(key, spec)| crate::RenderTile {
                key: TileKey {
                    tx: key[0],
                    ty: key[1],
                },
                hash: store.insert_tile(&TileData::new(format, spec.bytes()).expect("tile size")),
            })
            .collect::<Vec<_>>()
    };
    let mask = |store: &mut ObjectStore, m: &Option<RendererCaseMask>| {
        m.as_ref().map(|m| RMask {
            tiles: tiles(store, PixelFormat::R8, &m.tiles),
            linked: m.linked,
            transform: m.transform,
        })
    };
    let children: Vec<usize> = (1..=case.layers.len()).collect();
    let mut scene = scene(
        RContent::Group {
            children,
            isolated: false,
        },
        RFeatures::default(),
    );
    scene.render_version = alto_core::RENDER_VERSION_EXACT;
    if let Some(root) = ids.first() {
        scene.nodes[0].key = *root;
    }
    scene.canvas = IRect {
        x0: 0,
        y0: 0,
        x1: case.canvas[0],
        y1: case.canvas[1],
    };
    let color_adjust =
        |(contrast, saturation, brightness): (f32, f32, f32)| REffectKind::ColorAdjust {
            brightness,
            contrast,
            saturation,
        };
    for (i, layer) in case.layers.iter().enumerate() {
        let mut node = scene.nodes[0].clone();
        node.key = ids.get(i + 1).copied().unwrap_or_else(NodeId::new);
        node.features = RFeatures::default();
        match layer {
            RendererLayer::Raster {
                tiles: raster,
                transform,
                adjust,
                selection,
                mask: m,
                opacity,
                blend,
            } => {
                node.transform = *transform;
                node.opacity = *opacity;
                node.blend = *blend;
                node.content = RContent::Tiles(tiles(store, PixelFormat::Rgba8, raster));
                node.features.mask = mask(store, m);
                if let Some(settings) = adjust {
                    node.features.effects = vec![REffect {
                        kind: color_adjust(*settings),
                        selection: mask(store, selection),
                    }];
                }
            }
            RendererLayer::Adjust {
                settings,
                mask: m,
                opacity,
                blend,
            } => {
                node.opacity = *opacity;
                node.blend = *blend;
                node.content = RContent::Adjustment(REffect {
                    kind: color_adjust(*settings),
                    selection: None,
                });
                node.features.mask = mask(store, m);
            }
        }
        node.effects = node.features.effects.len();
        scene.nodes.push(node);
    }
    scene
}

pub(super) fn renderer_gpu(cases: &[RendererCase]) -> Result<RendererResults, String> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::METAL | wgpu::Backends::VULKAN | wgpu::Backends::DX12;
    let instance = wgpu::Instance::new(descriptor);
    let mut renderer = pollster::block_on(crate::renderer::Renderer::headless(&instance, 256))
        .map_err(|e| format!("headless renderer: {e}"))?;
    // Native `map_async` completes only while the device is polled.
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let poller = {
        let (device, done) = (renderer.device.clone(), done.clone());
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                let _ = device.poll(wgpu::PollType::Poll);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    };
    let result = cases
        .iter()
        .map(|case| {
            let mut store = alto_raster::ObjectStore::default();
            let scene = renderer_scene(case, &mut store, &[]);
            renderer.reset_render_cache();
            pollster::block_on(renderer.read_tile(
                &scene,
                &store,
                (case.level, case.key[0], case.key[1]),
            ))
        })
        .collect();
    done.store(true, std::sync::atomic::Ordering::Release);
    poller
        .join()
        .map_err(|_| "device poller panicked".to_string())?;
    Ok(result)
}
