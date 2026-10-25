//! Separate optional WASM probe; Vello is not linked into the production engine.
use super::*;
use alto_gpu::renderer::vector::VectorResources;
use alto_vector::{FillRule, LineCap, LineJoin, PathData, Stroke, StyledPath};
async fn idle(queue: &wgpu::Queue) -> Result<(), JsValue> {
    let (tx, rx) = futures_channel::oneshot::channel();
    queue.on_submitted_work_done(move || {
        let _ = tx.send(());
    });
    rx.await.map_err(error)
}
async fn pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Vec<u8>, JsValue> {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("comparison readback"),
        size: if texture.format() == wgpu::TextureFormat::Rgba16Float {
            512 * 512 * 8
        } else {
            512 * 512 * 4
        },
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(if texture.format() == wgpu::TextureFormat::Rgba16Float {
                    4096
                } else {
                    2048
                }),
                rows_per_image: Some(512),
            },
        },
        wgpu::Extent3d {
            width: 512,
            height: 512,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    let (tx, rx) = futures_channel::oneshot::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    rx.await.map_err(error)?.map_err(error)?;
    let view = buffer.slice(..).get_mapped_range().map_err(error)?;
    let bytes = view.to_vec();
    drop(view);
    buffer.unmap();
    buffer.destroy();
    Ok(bytes)
}
#[wasm_bindgen(js_name=compareVectorBackends)]
pub async fn compare_vector_backends() -> Result<String, JsValue> {
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
        .map_err(error)?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("M6 comparison"),
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        })
        .await
        .map_err(error)?;
    let make = |name| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(name),
            size: wgpu::Extent3d {
                width: 512,
                height: 512,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    };
    let lyon_target = make("lyon comparison");
    let vello_target = make("vello comparison");
    let mut paths = vec![];
    // One shared canonical workload: 100 curved, translucent stroked paths in a 10x10 grid.
    for y in 0..10 {
        for x in 0..10 {
            let xx = 8. + x as f64 * 50.;
            let yy = 8. + y as f64 * 50.;
            paths.push(StyledPath {
                data: PathData::from_svg(&format!(
                    "M{xx} {} Q{} {} {} {} C{} {} {} {} {xx} {} Z",
                    yy + 36.,
                    xx + 24.,
                    yy - 8.,
                    xx + 42.,
                    yy + 36.,
                    xx + 35.,
                    yy + 48.,
                    xx + 8.,
                    yy + 48.,
                    yy + 36.
                ))
                .map_err(error)?,
                fill: Some([x as f32 / 12. + 0.1, y as f32 / 12. + 0.1, 0.8, 0.7]),
                stroke: Some(Stroke {
                    color: [0.9, 0.8, 0.3, 0.75],
                    width: 2.,
                    cap: LineCap::Round,
                    join: LineJoin::Round,
                    miter_limit: 4.,
                }),
                fill_rule: FillRule::NonZero,
            });
        }
    }
    let time = clock();
    let mut lyon = VectorResources::new(&device, wgpu::TextureFormat::Rgba8Unorm);
    lyon.set_mode(alto_gpu::renderer::vector::VectorMode::Legacy);
    let lyon_setup = clock() - time;
    let time = clock();
    let mut vello =
        vello::Renderer::new(&device, vello::RendererOptions::default()).map_err(error)?;
    let vello_setup = clock() - time;
    let time = clock();
    let scene = alto_vector::VectorRasterizer::prepare(&mut VelloPreparation, &paths, 0.1)
        .map_err(error)?;
    let vello_encoding = clock() - time;
    let params = vello::RenderParams {
        base_color: vello::peniko::Color::TRANSPARENT,
        width: 512,
        height: 512,
        antialiasing_method: vello::AaConfig::Area,
    };
    let mut times_lyon = vec![];
    let mut times_vello = vec![];
    let mut first_lyon = 0.;
    let mut first_vello = 0.;
    for i in 0..23 {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let start = clock();
        let mut encoder = device.create_command_encoder(&Default::default());
        lyon.render(
            &device,
            &queue,
            &mut encoder,
            &lyon_target,
            &paths.iter().collect::<Vec<_>>(),
            [1., 0., 0., 1., 0., 0.],
            [0., 0., 1., 512., 512.],
            i + 1,
            128 * 1024 * 1024,
        )
        .map_err(error)?;
        queue.submit(Some(encoder.finish()));
        lyon.after_submit();
        idle(&queue).await?;
        let t = clock() - start;
        if i == 0 {
            first_lyon = t;
        }
        if i >= 3 {
            times_lyon.push(t);
        }
        if let Some(e) = scope.pop().await {
            return Err(error(e));
        }
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let start = clock();
        vello
            .render_to_texture(
                &device,
                &queue,
                &scene,
                &vello_target.create_view(&Default::default()),
                &params,
            )
            .map_err(error)?;
        idle(&queue).await?;
        let t = clock() - start;
        if i == 0 {
            first_vello = t;
        }
        if i >= 3 {
            times_vello.push(t);
        }
        if let Some(e) = scope.pop().await {
            return Err(error(e));
        }
    }
    let a = pixels(&device, &queue, &lyon_target).await?;
    let mut b = pixels(&device, &queue, &vello_target).await?;
    for p in b.as_chunks_mut::<4>().0 {
        for i in 0..3 {
            p[i] = ((p[i] as u32 * p[3] as u32 + 127) / 255) as u8;
        }
    }
    let mut max = 0;
    let mut total = 0u64;
    let mut interior = 0;
    let mut interior_bad = 0;
    let mut occupied_a = 0;
    let mut occupied_b = 0;
    for (i, (pa, pb)) in a
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0.iter())
        .enumerate()
    {
        if pa[3] > 0 {
            occupied_a += 1;
        }
        if pb[3] > 0 {
            occupied_b += 1;
        }
        let diff = pa
            .iter()
            .zip(pb)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        max = max.max(diff);
        total += pa
            .iter()
            .zip(pb)
            .map(|(a, b)| a.abs_diff(*b) as u64)
            .sum::<u64>();
        let x = i % 512;
        let y = i / 512;
        let interior_pixel = x >= 8
            && y >= 8
            && (16..=28).contains(&((x - 8) % 50))
            && (24..=32).contains(&((y - 8) % 50))
            && x < 508
            && y < 508;
        if interior_pixel {
            interior += 1;
            if diff > 3 {
                interior_bad += 1;
            }
        }
    }
    times_lyon.sort_by(f64::total_cmp);
    times_vello.sort_by(f64::total_cmp);
    let (hits, misses, entries) = lyon.counts();
    let result = serde_json::json!({"workload":{"width":512,"height":512,"paths":100,"warmupFrames":3,"measuredFrames":20,"sameCachedScene":true},"adapter":format!("{:?}",adapter.get_info()),"lyon":{"version":"1.0.22","aa":"4x MSAA R8 coverage","setupMs":lyon_setup,"firstDrawMs":first_lyon,"medianGpuCompleteMs":times_lyon[10],"p95GpuCompleteMs":times_lyon[18],"samplesMs":times_lyon,"cacheGpuBytes":lyon.bytes(),"cacheHits":hits,"cacheMisses":misses,"cacheEntries":entries},"vello":{"version":"0.11.0","aa":"Area","setupMs":vello_setup,"encodingMs":vello_encoding,"firstDrawMs":first_vello,"medianGpuCompleteMs":times_vello[10],"p95GpuCompleteMs":times_vello[18],"samplesMs":times_vello},"pixelComparison":{"maxChannelDifference":max,"meanAbsoluteChannelDifference":total as f64/a.len() as f64,"occupiedPixelsLyon":occupied_a,"occupiedPixelsVello":occupied_b,"interiorSampleRule":"local x16..28,y24..32 within each grid path; excludes stroke/AA boundary","interiorSamples":interior,"interiorDifferenceAbove3":interior_bad},"notes":["Includes CPU encoding, submission and queue completion, not GPU timestamps.","Cached unedited scene, not full editor, masks, effects or arbitrary workload.","Vello returns straight alpha; its readback is normalized to premultiplied RGBA before pixel comparison. AA methods differ; edge pixels are not expected to match exactly.","Vello internal GPU allocations are not measured; no memory-performance ranking is inferred."]});
    device.destroy();
    Ok(result.to_string())
}

// Both backends prepare the same canonical paths through the public trait.
struct VelloPreparation;
impl alto_vector::VectorRasterizer for VelloPreparation {
    type Prepared = vello::Scene;
    fn prepare(
        &mut self,
        paths: &[StyledPath],
        _tolerance: f32,
    ) -> std::result::Result<vello::Scene, alto_vector::VectorError> {
        alto_vector::validate_paths(paths)?;
        let mut scene = vello::Scene::new();
        for p in paths {
            let data = p.data.bez_path();
            if let Some(c) = p.fill {
                scene.fill(
                    match p.fill_rule {
                        FillRule::NonZero => vello::peniko::Fill::NonZero,
                        FillRule::EvenOdd => vello::peniko::Fill::EvenOdd,
                    },
                    vello::kurbo::Affine::IDENTITY,
                    vello::peniko::Color::new(c),
                    None,
                    &data,
                );
            }
            if let Some(s) = &p.stroke {
                scene.stroke(
                    &vello::kurbo::Stroke::new(s.width as f64)
                        .with_caps(match s.cap {
                            LineCap::Butt => vello::kurbo::Cap::Butt,
                            LineCap::Round => vello::kurbo::Cap::Round,
                            LineCap::Square => vello::kurbo::Cap::Square,
                        })
                        .with_join(match s.join {
                            LineJoin::Miter => vello::kurbo::Join::Miter,
                            LineJoin::Round => vello::kurbo::Join::Round,
                            LineJoin::Bevel => vello::kurbo::Join::Bevel,
                        })
                        .with_miter_limit(s.miter_limit as f64),
                    vello::kurbo::Affine::IDENTITY,
                    vello::peniko::Color::new(s.color),
                    None,
                    &data,
                );
            }
        }
        Ok(scene)
    }
}

fn metrics(c: alto_gpu::renderer::vector::Counters) -> serde_json::Value {
    serde_json::json!({"renderPasses":c.passes,"coveragePasses":c.coverage,"paintPasses":c.paint,"geometryDraws":c.geometry_draws,"paintDraws":c.paint_draws,"uniformBuffersCreated":c.uniform_buffers,"bindGroupsCreated":c.bind_groups,"uploadBytes":c.upload_bytes,"clearPixels":c.clear_pixels,"resolvePixels":c.resolve_pixels,"paths":c.paths,"groups":c.groups,"comparisons":c.comparisons,"barriers":c.barriers,"denseGroups":c.dense_groups})
}
fn grid_paths() -> Vec<StyledPath> {
    (0..100)
        .map(|i| {
            let x = 8. + (i % 10) as f64 * 50.;
            let y = 8. + (i / 10) as f64 * 50.;
            StyledPath {
                data: PathData::from_svg(&format!(
                    "M{x} {} Q{} {} {} {} C{} {} {} {} {x} {} Z",
                    y + 36.,
                    x + 24.,
                    y - 8.,
                    x + 42.,
                    y + 36.,
                    x + 35.,
                    y + 48.,
                    x + 8.,
                    y + 48.,
                    y + 36.
                ))
                .unwrap(),
                fill: Some([0.2, 0.6, 0.8, 0.7]),
                stroke: Some(Stroke {
                    color: [0.9, 0.8, 0.3, 0.75],
                    width: 2.,
                    cap: LineCap::Round,
                    join: LineJoin::Round,
                    miter_limit: 4.,
                }),
                fill_rule: FillRule::NonZero,
            }
        })
        .collect()
}
#[allow(clippy::too_many_arguments)]
async fn timed_vectors(
    r: &mut VectorResources,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &wgpu::Texture,
    paths: &[StyledPath],
    world: [f64; 6],
    roi: [f64; 5],
    frames: u32,
) -> Result<serde_json::Value, JsValue> {
    let refs: Vec<_> = paths.iter().collect();
    let mut samples = vec![];
    let mut cpu = vec![];
    let mut counters = serde_json::Value::Null;
    let mut first = 0.;
    let mut cold_cpu = 0.;
    for i in 0..frames + 3 {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        r.take_counters();
        let t = clock();
        let mut encoder = device.create_command_encoder(&Default::default());
        r.render(
            device,
            queue,
            &mut encoder,
            target,
            &refs,
            world,
            roi,
            i as u64 + 1,
            128 * 1024 * 1024,
        )
        .map_err(error)?;
        let encoded = clock() - t;
        queue.submit(Some(encoder.finish()));
        r.after_submit();
        await_idle(queue).await?;
        let elapsed = clock() - t;
        if i == 0 {
            first = elapsed;
            cold_cpu = encoded;
        }
        if i >= 3 {
            samples.push(elapsed);
            cpu.push(encoded);
        }
        counters = metrics(r.take_counters());
        if let Some(e) = scope.pop().await {
            return Err(error(e));
        }
    }
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    cpu.sort_by(f64::total_cmp);
    Ok(
        serde_json::json!({"firstGpuCompleteMs":first,"firstEncodeMs":cold_cpu,"medianGpuCompleteMs":sorted[sorted.len()/2],"p95GpuCompleteMs":sorted[(sorted.len()*95).div_ceil(100).saturating_sub(1)],"medianEncodeMs":cpu[cpu.len()/2],"samplesMs":samples,"counters":counters,"arenaBytes":r.arena_bytes(),"managedGpuBytes":r.bytes(),"meshCounts":r.counts(),"queueSubmitsPerFrame":1}),
    )
}
async fn await_idle(queue: &wgpu::Queue) -> Result<(), JsValue> {
    idle(queue).await
}

#[wasm_bindgen(js_name=verifyVectorOptimization)]
pub async fn verify_vector_optimization(frames: u32, reverse: bool) -> Result<String, JsValue> {
    use alto_gpu::renderer::vector::VectorMode;
    if frames == 0 || frames > 200 {
        return Err(error("frames must be 1..200"));
    }
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
        .map_err(error)?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("vector optimization verification"),
            ..Default::default()
        })
        .await
        .map_err(error)?;
    let make = || {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("RGBA16F vector verification"),
            size: wgpu::Extent3d {
                width: 512,
                height: 512,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    };
    let a = make();
    let b = make();
    let base = grid_paths();
    let mut overlap = base.clone();
    for (i, p) in overlap.iter_mut().enumerate() {
        p.data = PathData::from_svg(&format!("M{} 100 Q300 20 400 300 L100 400 Z", 100 + i % 5))
            .map_err(error)?;
        p.fill = Some([0.2, 0.4, 0.8, 0.03]);
    }
    let dense: Vec<_> = (0..1000)
        .map(|i| {
            let mut p = overlap[i % overlap.len()].clone();
            p.fill = Some([0.2, 0.4, 0.8, 0.002]);
            p
        })
        .collect();
    let mut cases = vec![
        (
            "disjoint-100",
            base.clone(),
            [1., 0., 0., 1., 0., 0.],
            [0., 0., 1., 512., 512.],
        ),
        (
            "overlap-100",
            overlap,
            [1., 0., 0., 1., 0., 0.],
            [0., 0., 1., 512., 512.],
        ),
        (
            "rotate-shear-ROI",
            base.clone(),
            [0.8, 0.2, -0.35, 0.9, 100., -40.],
            [-24., -16., 0.7, 512., 512.],
        ),
    ];
    cases.push((
        "overlap-1000",
        dense,
        [1., 0., 0., 1., 0., 0.],
        [0., 0., 1., 512., 512.],
    ));
    cases.push((
        "reflection-zoom",
        base.clone(),
        [-0.8, 0.2, 0.4, 0.9, 350., -80.],
        [-10., -10., 0.5, 512., 512.],
    ));
    let mut strokes = base.clone();
    for (i, p) in strokes.iter_mut().enumerate() {
        if i % 2 == 0 {
            p.fill = None;
        }
        let stroke = p.stroke.as_mut().unwrap();
        stroke.cap = if i % 2 == 0 {
            LineCap::Square
        } else {
            LineCap::Butt
        };
        stroke.join = if i % 2 == 0 {
            LineJoin::Miter
        } else {
            LineJoin::Bevel
        };
    }
    cases.push((
        "stroke-only-and-caps",
        strokes,
        [1., 0., 0., 1., 0., 0.],
        [0., 0., 1., 512., 512.],
    ));
    let mut edge = vec![];
    for (i, data) in [
        "M10 10H90V90H10Z M30 30H70V70H30Z",
        "M90 10H170V90H90Z",
        "M200 30L270 90L200 90L270 30",
        "M300 20Q340 100 380 20C390 80 310 80 300 20Z",
    ]
    .iter()
    .enumerate()
    {
        let mut p = base[i].clone();
        p.data = PathData::from_svg(data).map_err(error)?;
        p.fill_rule = FillRule::EvenOdd;
        p.stroke.as_mut().unwrap().width = 7.;
        edge.push(p);
    }
    cases.push((
        "holes-touch-self-overlap",
        edge,
        [1., 0., 0., 1., 0., 0.],
        [0., 0., 1., 512., 512.],
    ));
    let mut large = base[..4].to_vec();
    for (i, p) in large.iter_mut().enumerate() {
        let x = 1e5 + i as f64 * 80.;
        p.data = PathData::from_svg(&format!("M{x} 100h50v50h-50Z")).map_err(error)?;
    }
    cases.push((
        "large-origin",
        large,
        [1., 0., 0., 1., -1e5, 0.],
        [0., 0., 1., 512., 512.],
    ));
    let mut order_paths = vec![];
    for i in 0..33 {
        let mut p = base[i % base.len()].clone();
        p.data = PathData::from_svg("M80 90Q430 0 410 360L100 430Z").map_err(error)?;
        p.fill = Some([
            ((i * 37) % 100) as f32 / 100.,
            ((i * 11) % 100) as f32 / 100.,
            0.7,
            [0., 0.0001, 0.03, 0.5, 1.][i % 5],
        ]);
        p.stroke.as_mut().unwrap().color = [0.9, 0.1, 0.3, 0.2];
        if i == 0 {
            p.stroke = None;
        }
        order_paths.push(p);
    }
    cases.push((
        "dense-color-order-partial-group",
        order_paths.clone(),
        [1., 0., 0., 1., 0., 0.],
        [0., 0., 1., 512., 512.],
    ));
    cases.push((
        "dense-transform-ROI",
        order_paths,
        [0.8, 0.2, -0.35, 0.9, 100., -40.],
        [-24., -16., 0.7, 512., 512.],
    ));
    let mut results = vec![];
    let mut pass = true;
    for (case_index, (name, paths, world, roi)) in cases.iter().enumerate() {
        let mut reference = None;
        let mut modes = serde_json::Map::new();
        let order = if (case_index % 2 == 0) != reverse {
            [
                VectorMode::Legacy,
                VectorMode::Pooled,
                VectorMode::Disjoint,
                VectorMode::Dense,
                VectorMode::Batched,
            ]
        } else {
            [
                VectorMode::Batched,
                VectorMode::Dense,
                VectorMode::Disjoint,
                VectorMode::Pooled,
                VectorMode::Legacy,
            ]
        };
        let mut images = vec![];
        for mode in order {
            let t = clock();
            let mut r = VectorResources::new(&device, wgpu::TextureFormat::Rgba16Float);
            r.set_mode(mode);
            let setup = clock() - t;
            let mut timing = timed_vectors(
                &mut r,
                &device,
                &queue,
                &a,
                paths,
                *world,
                *roi,
                if ["disjoint-100", "overlap-100", "overlap-1000"].contains(name) {
                    frames
                } else {
                    1
                },
            )
            .await?;
            timing["setupMs"] = serde_json::json!(setup);
            let image = pixels(&device, &queue, &a).await?;
            if mode == VectorMode::Legacy {
                reference = Some(image.clone());
            }
            images.push((format!("{mode:?}"), image));
            modes.insert(format!("{mode:?}"), timing);
            r.destroy_after_submit();
        }
        if *name == "disjoint-100" {
            pass &= modes["Legacy"]["counters"]["renderPasses"] == 401;
            pass &= modes["Pooled"]["counters"]["renderPasses"] == 401;
            pass &= modes["Batched"]["counters"]["renderPasses"] == 5;
            pass &= modes["Batched"]["counters"]["uniformBuffersCreated"] == 0;
        }
        if VectorResources::supports_dense(&device)
            && ["overlap-100", "overlap-1000"].contains(name)
        {
            let expected = if *name == "overlap-100" { 51 } else { 501 };
            pass &= modes["Batched"]["counters"]["renderPasses"] == expected;
            pass &= modes["Batched"]["counters"]["denseGroups"]
                .as_u64()
                .unwrap_or(0)
                > 0;
        }
        let reference = reference.unwrap();
        let mut differences = serde_json::Map::new();
        for (name, image) in images {
            let changed = image.iter().zip(&reference).filter(|(a, b)| a != b).count();
            pass &= changed == 0;
            differences.insert(name, serde_json::json!(changed));
        }
        results.push(
            serde_json::json!({"case":name,"modes":modes,"differentRGBA16FBytes":differences}),
        );
    }
    // Same resource set, two distinct render calls in one encoder. No arena reuse before submit.
    let mut expected = vec![];
    for (i, target) in [&a, &b].iter().enumerate() {
        let mut r = VectorResources::new(&device, wgpu::TextureFormat::Rgba16Float);
        r.set_mode(VectorMode::Legacy);
        let mut e = device.create_command_encoder(&Default::default());
        r.render(
            &device,
            &queue,
            &mut e,
            target,
            &base.iter().collect::<Vec<_>>(),
            [1., 0., 0., 1., i as f64 * 12., 0.],
            [0., 0., 1., 512., 512.],
            1,
            128 * 1024 * 1024,
        )
        .map_err(error)?;
        queue.submit(Some(e.finish()));
        r.after_submit();
        expected.push(pixels(&device, &queue, target).await?);
        r.destroy_after_submit();
    }
    let mut r = VectorResources::new(&device, wgpu::TextureFormat::Rgba16Float);
    let mut e = device.create_command_encoder(&Default::default());
    for (i, target) in [&a, &b].iter().enumerate() {
        r.render(
            &device,
            &queue,
            &mut e,
            target,
            &base.iter().collect::<Vec<_>>(),
            [1., 0., 0., 1., i as f64 * 12., 0.],
            [0., 0., 1., 512., 512.],
            1,
            128 * 1024 * 1024,
        )
        .map_err(error)?;
    }
    queue.submit(Some(e.finish()));
    r.after_submit();
    let mut multi = true;
    for (i, target) in [&a, &b].iter().enumerate() {
        multi &= pixels(&device, &queue, target).await? == expected[i];
    }
    pass &= multi;
    let multi_stats = metrics(r.take_counters());
    r.destroy_after_submit();
    let refs: Vec<_> = base.iter().collect();
    let mut r = VectorResources::new(&device, wgpu::TextureFormat::Rgba16Float);
    for (i, target) in [&a, &b].iter().enumerate() {
        let mut e = device.create_command_encoder(&Default::default());
        r.render(
            &device,
            &queue,
            &mut e,
            target,
            &refs,
            [1., 0., 0., 1., i as f64 * 12., 0.],
            [0., 0., 1., 512., 512.],
            1,
            128 * 1024 * 1024,
        )
        .map_err(error)?;
        queue.submit(Some(e.finish()));
        r.after_submit();
    }
    let mut queued = true;
    for (i, target) in [&a, &b].iter().enumerate() {
        queued &= pixels(&device, &queue, target).await? == expected[i];
    }
    pass &= queued;
    r.destroy_after_submit();
    // A budget that permits existing meshes/coverage but no arena page must use the reference path.
    let mut r = VectorResources::new(&device, wgpu::TextureFormat::Rgba16Float);
    r.set_mode(VectorMode::Legacy);
    let mut e = device.create_command_encoder(&Default::default());
    r.render(
        &device,
        &queue,
        &mut e,
        &a,
        &refs,
        [1., 0., 0., 1., 0., 0.],
        [0., 0., 1., 512., 512.],
        1,
        128 * 1024 * 1024,
    )
    .map_err(error)?;
    queue.submit(Some(e.finish()));
    r.after_submit();
    let budget = r.bytes() + 1;
    r.take_counters();
    r.set_mode(VectorMode::Batched);
    let mut e = device.create_command_encoder(&Default::default());
    r.render(
        &device,
        &queue,
        &mut e,
        &a,
        &refs,
        [1., 0., 0., 1., 0., 0.],
        [0., 0., 1., 512., 512.],
        2,
        budget,
    )
    .map_err(error)?;
    queue.submit(Some(e.finish()));
    r.after_submit();
    let fallback = pixels(&device, &queue, &a).await? == expected[0]
        && r.arena_bytes() == 0
        && r.take_counters().passes == 401;
    pass &= fallback;
    r.destroy_after_submit();
    let dense_lifetimes = check_dense_lifetimes(
        &device,
        &queue,
        &a,
        &b,
        &cases
            .iter()
            .find(|c| c.0 == "dense-color-order-partial-group")
            .unwrap()
            .1,
    )
    .await?;
    pass &= dense_lifetimes["pass"] == true;
    let result = serde_json::json!({"implementation":"MRT8","denseSupported":VectorResources::supports_dense(&device),"denseLifetimes":dense_lifetimes,"pass":pass,"adapter":format!("{:?}",adapter.get_info()),"format":"rgba16float","warmupFrames":3,"measuredFrames":frames,"cases":results,"multipleCallsBeforeSubmit":{"pass":multi,"counters":multi_stats},"queuedSubmissions":{"pass":queued},"arenaBudgetFallback":{"pass":fallback},"reverseOrder":reverse,"notes":["CPU encode and queue completion are measured separately; not GPU timestamps.","Readback and validation scope collection are outside frame timings.","Vello is not used in the production RGBA16F path."]});
    a.destroy();
    b.destroy();
    device.destroy();
    Ok(result.to_string())
}

// Includes coverage replacement while earlier commands are still unsubmitted.
async fn check_dense_lifetimes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    a: &wgpu::Texture,
    b: &wgpu::Texture,
    paths: &[StyledPath],
) -> Result<serde_json::Value, JsValue> {
    let paths: Vec<_> = (0..3).flat_map(|_| paths.iter().cloned()).collect();
    let refs: Vec<_> = paths.iter().collect();
    let rois = [[0., 0., 1., 512., 512.], [-24., -16., 0.7, 256., 384.]];
    let world = [0.8, 0.2, -0.35, 0.9, 100., -40.];
    let mut expected = vec![];
    for (i, target) in [a, b].iter().enumerate() {
        let mut r = VectorResources::new(device, wgpu::TextureFormat::Rgba16Float);
        r.set_mode(alto_gpu::renderer::vector::VectorMode::Legacy);
        let mut e = device.create_command_encoder(&Default::default());
        r.render(
            device,
            queue,
            &mut e,
            target,
            &refs,
            world,
            rois[i],
            1,
            128 * 1024 * 1024,
        )
        .map_err(error)?;
        queue.submit(Some(e.finish()));
        r.after_submit();
        expected.push(pixels(device, queue, target).await?);
        r.destroy_after_submit();
    }
    let mut r = VectorResources::new(device, wgpu::TextureFormat::Rgba16Float);
    let mut e = device.create_command_encoder(&Default::default());
    for (i, target) in [a, b].iter().enumerate() {
        r.render(
            device,
            queue,
            &mut e,
            target,
            &refs,
            world,
            rois[i],
            1,
            128 * 1024 * 1024,
        )
        .map_err(error)?;
    }
    queue.submit(Some(e.finish()));
    r.after_submit();
    let same_encoder = pixels(device, queue, a).await? == expected[0]
        && pixels(device, queue, b).await? == expected[1];
    for (i, target) in [a, b].iter().enumerate() {
        let mut e = device.create_command_encoder(&Default::default());
        r.render(
            device,
            queue,
            &mut e,
            target,
            &refs,
            world,
            rois[i],
            2 + i as u64,
            128 * 1024 * 1024,
        )
        .map_err(error)?;
        queue.submit(Some(e.finish()));
        r.after_submit();
    }
    let queued = pixels(device, queue, a).await? == expected[0]
        && pixels(device, queue, b).await? == expected[1];
    let before = r.bytes();
    for i in 0..4 {
        let mut e = device.create_command_encoder(&Default::default());
        r.render(
            device,
            queue,
            &mut e,
            b,
            &refs,
            world,
            rois[1],
            10 + i,
            128 * 1024 * 1024,
        )
        .map_err(error)?;
        queue.submit(Some(e.finish()));
        r.after_submit();
    }
    let stable = before == r.bytes() && pixels(device, queue, b).await? == expected[1];
    let budget = r.mesh_bytes() + 256 * 384 * 5 + r.arena_bytes().max(65536);
    r.take_counters();
    let mut e = device.create_command_encoder(&Default::default());
    r.render(device, queue, &mut e, b, &refs, world, rois[1], 20, budget)
        .map_err(error)?;
    queue.submit(Some(e.finish()));
    r.after_submit();
    let c = r.take_counters();
    let pressure = r.bytes() <= budget
        && c.dense_groups == 0
        && pixels(device, queue, b).await? == expected[1];
    let result = serde_json::json!({"pass":same_encoder && queued && stable && pressure,"sameEncoderResize":same_encoder,"queuedResize":queued,"warmBytesStable":stable,"cachedScratchEviction":pressure,"warmBytes":before,"pressureBytes":r.bytes(),"pressureBudget":budget,"pressureCounters":metrics(c)});
    r.destroy_after_submit();
    Ok(result)
}
