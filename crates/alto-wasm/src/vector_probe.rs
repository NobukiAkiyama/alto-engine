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
        size: 512 * 512 * 4,
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
                bytes_per_row: Some(2048),
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
            &mut encoder,
            &lyon_target,
            &paths,
            [1., 0., 0., 1., 0., 0.],
            [0., 0., 1., 512., 512.],
            i + 1,
            128 * 1024 * 1024,
        )
        .map_err(error)?;
        queue.submit(Some(encoder.finish()));
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
