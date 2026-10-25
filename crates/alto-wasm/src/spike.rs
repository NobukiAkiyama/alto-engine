#![cfg(target_arch = "wasm32")]

use alto_core::{NodeId, PixelFormat, TileKey};
use alto_doc::{DocumentMeta, Node, NodeKind, Op, TileChange};
use alto_engine::{DocHandle, Engine};
use alto_raster::TileData;
use wasm_bindgen::prelude::*;
use wgpu::util::DeviceExt;

const TILE: u32 = 256;
const COUNT: u32 = 100;

#[wasm_bindgen]
pub struct Spike {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    tiles: wgpu::Texture,
    view_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    engine: Engine,
    document: DocHandle,
    layer: NodeId,
    edit_indices: Vec<u32>,
    redo_indices: Vec<u32>,
}

fn js_error(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}

fn now() -> Result<f64, JsValue> {
    let global: web_sys::WorkerGlobalScope = js_sys::global().dyn_into()?;
    Ok(global
        .performance()
        .ok_or_else(|| js_error("performance clock unavailable"))?
        .now())
}

fn tile_pixels(index: u32, alternate: bool) -> Vec<u8> {
    let mut out = vec![0_u8; (TILE * TILE * 4) as usize];
    for y in 0..TILE {
        for x in 0..TILE {
            let offset = ((y * TILE + x) * 4) as usize;
            let checker = ((x / 32 + y / 32 + index) & 1) == 0;
            let edge = x < 2 || y < 2;
            let color = if alternate {
                [244, if checker { 74 } else { 125 }, 77, 255]
            } else if edge {
                [69, 88, 136, 255]
            } else if checker {
                [155, 192, 240, 255]
            } else {
                [224, 235, 250, 255]
            };
            out[offset..offset + 4].copy_from_slice(&color);
        }
    }
    out
}

fn tile_key(index: u32) -> TileKey {
    TileKey {
        tx: (index % 10) as i32,
        ty: (index / 10) as i32,
    }
}

#[wasm_bindgen]
impl Spike {
    #[wasm_bindgen(js_name = create)]
    pub async fn create(canvas: web_sys::OffscreenCanvas) -> Result<Spike, JsValue> {
        let width = canvas.width().max(1);
        let height = canvas.height().max(1);
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::OffscreenCanvas(canvas))
            .map_err(js_error)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .map_err(js_error)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .map_err(js_error)?;
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
            .or_else(|| capabilities.formats.first().copied())
            .ok_or_else(|| js_error("Surface has no supported formats"))?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let tiles = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("100 content tiles"),
            size: wgpu::Extent3d {
                width: TILE,
                height: TILE,
                depth_or_array_layers: COUNT,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let mut engine = Engine::new();
        let document = engine
            .create_document(DocumentMeta::srgb_u8(TILE * 10, TILE * 10))
            .map_err(js_error)?;
        let root = engine.document(document).map_err(js_error)?.root;
        let raster = Node::raster("M0タイル");
        let layer = raster.id;
        engine
            .transact(
                document,
                "M0 layer",
                vec![Op::InsertNode {
                    node: raster,
                    parent: root,
                    index: 0,
                }],
                0,
            )
            .map_err(js_error)?;
        let mut changes = Vec::with_capacity(COUNT as usize);
        for index in 0..COUNT {
            let pixels = tile_pixels(index, false);
            let hash = engine
                .put_tile(
                    TileData::new(PixelFormat::Rgba8, pixels.clone())
                        .map_err(|_| js_error("Invalid M0 tile"))?,
                )
                .ok_or_else(|| js_error("Empty M0 tile"))?;
            changes.push(TileChange {
                key: tile_key(index),
                before: None,
                after: Some(hash),
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tiles,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: index,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(TILE * 4),
                    rows_per_image: Some(TILE),
                },
                wgpu::Extent3d {
                    width: TILE,
                    height: TILE,
                    depth_or_array_layers: 1,
                },
            );
        }
        engine
            .transact(
                document,
                "M0 content",
                vec![Op::SetTiles { layer, changes }],
                1,
            )
            .map_err(js_error)?;
        let view_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("view uniform"),
            contents: &[0_u8; 32],
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let texture_view = tiles.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tile and view layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tile and view"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: view_buffer.as_entire_binding(),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tiles WGSL"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/tiles.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tile pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tile pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Ok(Spike {
            device,
            queue,
            surface,
            config,
            tiles,
            view_buffer,
            bind_group,
            pipeline,
            engine,
            document,
            layer,
            edit_indices: Vec::new(),
            redo_indices: Vec::new(),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    pub fn render(&mut self, pan_x: f32, pan_y: f32, zoom: f32) -> Result<(), JsValue> {
        let mut uniform = [0_f32; 8];
        uniform[..4].copy_from_slice(&[pan_x, pan_y, zoom.max(0.02), 0.0]);
        uniform[4..].copy_from_slice(&[
            self.config.width as f32,
            self.config.height as f32,
            0.0,
            0.0,
        ]);
        let bytes = unsafe { std::slice::from_raw_parts(uniform.as_ptr().cast::<u8>(), 32) };
        self.queue.write_buffer(&self.view_buffer, 0, bytes);
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            state => return Err(js_error(format!("Surface unavailable: {state:?}"))),
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tile frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tiles"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.13,
                            g: 0.15,
                            b: 0.20,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..6, 0..COUNT);
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        Ok(())
    }

    #[wasm_bindgen(js_name = rewriteAndReadTile)]
    pub async fn rewrite_and_read_tile(&mut self, index: u32) -> Result<f64, JsValue> {
        if index >= COUNT {
            return Err(js_error("Tile index out of range"));
        }
        let start = now()?;
        let expected = tile_pixels(index, true);
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.tiles,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: index,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &expected,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(TILE * 4),
                rows_per_image: Some(TILE),
            },
            wgpu::Extent3d {
                width: TILE,
                height: TILE,
                depth_or_array_layers: 1,
            },
        );
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback tile"),
            size: (TILE * TILE * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.tiles,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: index,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(TILE * 4),
                    rows_per_image: Some(TILE),
                },
            },
            wgpu::Extent3d {
                width: TILE,
                height: TILE,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));
        let (sender, receiver) = futures_channel::oneshot::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        receiver.await.map_err(js_error)?.map_err(js_error)?;
        let mapped = buffer.slice(..).get_mapped_range().map_err(js_error)?;
        let equal = &mapped[..] == expected.as_slice();
        let result_pixels = mapped.to_vec();
        drop(mapped);
        buffer.unmap();
        if !equal {
            return Err(js_error("Readback pixels did not match uploaded tile"));
        }
        let elapsed = now()? - start;
        let key = tile_key(index);
        let before = self.current_tile_hash(key)?;
        let after = self
            .engine
            .put_tile(
                TileData::new(PixelFormat::Rgba8, result_pixels)
                    .map_err(|_| js_error("Invalid readback tile"))?,
            )
            .ok_or_else(|| js_error("Readback tile was empty"))?;
        let revision = self
            .engine
            .document(self.document)
            .map_err(js_error)?
            .revision;
        self.engine
            .transact(
                self.document,
                "M0 tile readback",
                vec![Op::SetTiles {
                    layer: self.layer,
                    changes: vec![TileChange {
                        key,
                        before,
                        after: Some(after),
                    }],
                }],
                revision,
            )
            .map_err(js_error)?;
        self.edit_indices.push(index);
        self.redo_indices.clear();
        Ok(elapsed)
    }
}

impl Spike {
    fn current_tile_hash(&self, key: TileKey) -> Result<Option<alto_core::ContentHash>, JsValue> {
        let node = self
            .engine
            .document(self.document)
            .map_err(js_error)?
            .node(self.layer)
            .map_err(js_error)?;
        let NodeKind::Raster { tiles } = &node.kind else {
            return Err(js_error("M0 layer is not raster"));
        };
        Ok(tiles.tiles.get(&key).copied())
    }

    fn sync_tile_from_document(&self, index: u32) -> Result<(), JsValue> {
        let data = match self.current_tile_hash(tile_key(index))? {
            Some(hash) => self
                .engine
                .require_tile(&hash)
                .map_err(js_error)?
                .pixels
                .to_vec(),
            None => vec![0; (TILE * TILE * 4) as usize],
        };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.tiles,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: index,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(TILE * 4),
                rows_per_image: Some(TILE),
            },
            wgpu::Extent3d {
                width: TILE,
                height: TILE,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }
}

#[wasm_bindgen]
impl Spike {
    #[wasm_bindgen(js_name = canUndo)]
    pub fn can_undo(&self) -> bool {
        !self.edit_indices.is_empty()
    }

    #[wasm_bindgen(js_name = canRedo)]
    pub fn can_redo(&self) -> bool {
        !self.redo_indices.is_empty()
    }

    pub fn undo(&mut self) -> Result<bool, JsValue> {
        let Some(index) = self.edit_indices.last().copied() else {
            return Ok(false);
        };
        self.engine.undo(self.document).map_err(js_error)?;
        self.edit_indices.pop();
        self.redo_indices.push(index);
        self.sync_tile_from_document(index)?;
        Ok(true)
    }

    pub fn redo(&mut self) -> Result<bool, JsValue> {
        let Some(index) = self.redo_indices.last().copied() else {
            return Ok(false);
        };
        self.engine.redo(self.document).map_err(js_error)?;
        self.redo_indices.pop();
        self.edit_indices.push(index);
        self.sync_tile_from_document(index)?;
        Ok(true)
    }

    #[wasm_bindgen(js_name = snapshotJson)]
    pub fn snapshot_json(&self) -> Result<String, JsValue> {
        String::from_utf8(self.engine.snapshot(self.document).map_err(js_error)?).map_err(js_error)
    }
}
