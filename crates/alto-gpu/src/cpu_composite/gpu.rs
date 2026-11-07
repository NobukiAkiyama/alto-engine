//! The headless GPU harness that runs the parity shaders.
use super::*;

pub(super) fn scene(content: RContent, features: RFeatures) -> RenderScene {
    RenderScene {
        canvas: IRect {
            x0: 0,
            y0: 0,
            x1: 256,
            y1: 256,
        },
        root: 0,
        render_version: RENDER_VERSION,
        nodes: vec![RNode {
            key: NodeId::new(),
            version: 0,
            visible: true,
            transform: ident(),
            opacity: 1.0,
            blend: RenderBlend::Normal,
            effects: features.effects.len(),
            features,
            content,
        }],
    }
}

/// The numeric parity host refuses v3 modes 17 and up before any command is
/// built, as the renderer does in `prepare_scene` (DQ3-3: no new variant).
pub(super) fn has_unspecified_v3_mode(ops: &[Op]) -> bool {
    ops.iter().any(|op| match op {
        Op::Pop { mode, .. } | Op::Draw { mode, .. } => *mode >= 17,
        _ => false,
    })
}

/// Version 3 has exactly three classes (`renderer::v3_gap`): `None` (integer tile
/// compositor), `v3-m5-integer` (integer M5 stages) and `v3-unsupported` (refused
/// by the host). Versions 1 and 2 keep their first-gap names.
pub(super) fn gap(scene: &RenderScene) -> Option<&'static str> {
    if scene.render_version == 3 {
        if crate::renderer::v3_gap(scene).is_some() {
            return Some("v3-unsupported");
        }
        return (!plain_scene(scene)).then_some("v3-m5-integer");
    }
    for n in &scene.nodes {
        if n.features.mask.is_some() {
            return Some("mask");
        }
        if n.features.vector_mask.is_some() {
            return Some("vector mask");
        }
        if n.features.clip_path.is_some() {
            return Some("clip path");
        }
        if n.features.clip {
            return Some("clip");
        }
        if !n.features.effects.is_empty() {
            return Some("effects");
        }
        match &n.content {
            RContent::Mesh(_) => return Some("vector"),
            RContent::Text(_) | RContent::TextBake(_) => return Some("text"),
            RContent::Adjustment(_) => return Some("adjustment"),
            RContent::Unsupported => return Some("unsupported"),
            RContent::Group { .. } | RContent::Tiles(_) => {}
        }
    }
    None
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct GpuCommand {
    pub(super) inv0: [f32; 4],
    pub(super) inv1: [f32; 4],
    pub(super) info: [u32; 4],
    pub(super) trans: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct GpuTile {
    pub(super) xy: [i32; 2],
    pub(super) slot: u32,
    pub(super) pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct GpuConfig {
    pub(super) origin_scale: [f32; 4],
    pub(super) info: [u32; 4],
}

/// Parity-harness shader. The production `composite.wgsl` source is unchanged.
/// `pre_quant` copies `accum` before the same `textureStore` into `rgba8unorm`.
pub(super) fn parity_shader() -> String {
    let src = shader_with_blend(include_str!("../../shaders/composite.wgsl"));
    let decl = "@group(0) @binding(4) var<uniform> config: Config;";
    let with_decl = format!(
        "{decl}\n@group(0) @binding(5) var<storage, read_write> pre_quant: array<vec4<f32>>;"
    );
    let src = src.replacen(decl, &with_decl, 1);
    let store = "textureStore(output,vec2i(id.xy),i32(config.info.y),accum);";
    let patched = "pre_quant[id.y * 256u + id.x] = accum;\n  textureStore(output,vec2i(id.xy),i32(config.info.y),accum);";
    let src = src.replacen(store, patched, 1);
    assert!(
        src.contains("var<storage, read_write> pre_quant") && src.contains(store),
        "parity shader did not keep the production textureStore"
    );
    src
}

pub(super) fn integer_parity_shader() -> String {
    let source = crate::integer_composite_shader();
    let marker = "  output[id.y*256u+id.x]=";
    assert_eq!(source.matches(marker).count(), 1);
    source.replace("@compute @workgroup_size(16,16)",
        "@group(0) @binding(5) var<storage,read_write> pre_quant:array<vec4f>;\n@compute @workgroup_size(16,16)")
        .replace(marker, &format!("  pre_quant[id.y*256u+id.x]=vec4f(accum)/16384.0;\n{marker}"))
}

/// `effects.wgsl` uniform. Field order matches the WGSL `Config` in uniform address space.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct EffectConfig {
    pub(super) out_region: [f32; 4],
    pub(super) region_a: [f32; 4],
    pub(super) region_b: [f32; 4],
    pub(super) inv0: [f32; 4],
    pub(super) inv1: [f32; 4],
    pub(super) settings: [f32; 4],
    pub(super) info: [u32; 4],
    pub(super) extra: [f32; 4],
    pub(super) curve: [f32; 64],
    pub(super) sizes: [u32; 4],
    pub(super) output_clip: [u32; 4],
}

pub(super) struct Gpu {
    pub(super) device: wgpu::Device,
    pub(super) queue: wgpu::Queue,
    pub(super) pipeline: wgpu::ComputePipeline,
    pub(super) adapter_name: String,
}

impl Gpu {
    pub(super) fn new() -> Result<Option<Self>, String> {
        pollster::block_on(async {
            let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
            descriptor.backends =
                wgpu::Backends::METAL | wgpu::Backends::VULKAN | wgpu::Backends::DX12;
            let instance = wgpu::Instance::new(descriptor);
            let adapter = match instance
                .request_adapter(&wgpu::RequestAdapterOptions::default())
                .await
            {
                Ok(adapter) => adapter,
                Err(_) => return Ok(None),
            };
            let info = adapter.get_info();
            println!(
                "gpu pixel parity adapter: {} ({:?})",
                info.name, info.backend
            );
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .map_err(|e| format!("request_device: {e}"))?;
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("composite parity"),
                source: wgpu::ShaderSource::Wgsl(parity_shader().into()),
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("composite parity"),
                layout: None,
                module: &shader,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
            Ok(Some(Self {
                device,
                queue,
                pipeline,
                adapter_name: info.name,
            }))
        })
    }

    /// Upload all 256 byte values to an `Rgba8Unorm` or `R8Unorm` texture and read
    /// them back with `textureLoad` + `unorm_byte`. Returns every channel that does
    /// not round-trip (`R8Unorm`: the r channel only). Enforced: the caller
    /// records it and fails on any mismatch.
    pub(super) fn atlas_read(&self, r8: bool) -> Result<AtlasRead, String> {
        let (format, label, texel_bytes) = if r8 {
            (wgpu::TextureFormat::R8Unorm, "R8Unorm", 1)
        } else {
            (wgpu::TextureFormat::Rgba8Unorm, "Rgba8Unorm", 4)
        };
        let device = &self.device;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("atlas read"),
            source: wgpu::ShaderSource::Wgsl(atlas_read_shader().into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("atlas read"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas read"),
            size: wgpu::Extent3d {
                width: 256,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let data: Vec<u8> = (0..256)
            .flat_map(|i| atlas_read_texel(i)[..texel_bytes].to_vec())
            .collect();
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256 * texel_bytes as u32),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 256,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atlas read out"),
            size: 4096,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("atlas read readback"),
            size: 4096,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let view = texture.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas read"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: out.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(4, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&out, 0, &read, 0, 4096);
        self.queue.submit(Some(encoder.finish()));
        map_readback(device, &read)?;
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(format!("validation: {err}"));
        }
        let raw = read
            .slice(..)
            .get_mapped_range()
            .map_err(|e| e.to_string())?
            .to_vec();
        read.unmap();
        let got: &[u32] = bytemuck::cast_slice(&raw);
        let mut mismatches = Vec::new();
        for texel in 0..256 {
            let want = atlas_read_texel(texel);
            for ch in 0..texel_bytes {
                let value = got[texel * 4 + ch];
                if value != u32::from(want[ch]) {
                    mismatches.push((texel, ch, want[ch], value));
                }
            }
        }
        Ok(AtlasRead {
            format: label,
            mismatches,
        })
    }

    pub(super) fn render(&self, case: &Case) -> Result<(Vec<u8>, Vec<f32>), String> {
        self.render_version(case, 2)
    }

    pub(super) fn downsample_q14(&self, children: &[Vec<u8>; 4]) -> Result<Vec<u8>, String> {
        let device = &self.device;
        let input = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("integer pyramid parity input"),
            size: wgpu::Extent3d {
                width: 512,
                height: 512,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (i, child) in children.iter().enumerate() {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &input,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: (i as u32 % 2) * 256,
                        y: (i as u32 / 2) * 256,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                child,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(1024),
                    rows_per_image: Some(256),
                },
                wgpu::Extent3d {
                    width: 256,
                    height: 256,
                    depth_or_array_layers: 1,
                },
            );
        }
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("integer pyramid parity output"),
            size: wgpu::Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Uint,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("integer pyramid parity"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../../shaders/downsample-q14.wgsl").into(),
            ),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let config = upload_buffer(
            device,
            &self.queue,
            &[[0u32; 4]],
            wgpu::BufferUsages::UNIFORM,
        );
        let input_view = input.create_view(&Default::default());
        let output_view = output.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: config.as_entire_binding(),
                },
            ],
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * 1024,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(16, 16, 1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &read,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(1024),
                    rows_per_image: Some(256),
                },
            },
            wgpu::Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));
        map_readback(device, &read)?;
        let bytes = read
            .slice(..)
            .get_mapped_range()
            .map_err(|e| e.to_string())?
            .to_vec();
        read.unmap();
        Ok(bytes)
    }
    pub(super) fn render_pyramid(&self, case: &Case) -> Result<Vec<u8>, String> {
        if case.level <= 0 {
            return self.render_version(case, 3).map(|p| p.0);
        }
        let children = pyramid_children(case).map(|c| self.render_pyramid(&c));
        let [a, b, c, d] = children;
        self.downsample_q14(&[a?, b?, c?, d?])
    }

    pub(super) fn render_version(
        &self,
        case: &Case,
        version: u32,
    ) -> Result<(Vec<u8>, Vec<f32>), String> {
        let integer = version == 3;
        let mut ops = Vec::new();
        for node in &case.nodes {
            if integer {
                walk_v3(node, alto_core::FixedAffine::IDENTITY, &mut ops);
            } else {
                walk(node, ident(), &mut ops);
            }
        }
        if integer && has_unspecified_v3_mode(&ops) {
            return Err("v3 unspecified blend mode".into());
        }
        let mut commands = Vec::new();
        let mut entries = Vec::new();
        let mut atlas_tiles: Vec<Vec<u8>> = Vec::new();
        for op in &ops {
            match op {
                Op::Push => commands.push(GpuCommand {
                    inv0: [0.0; 4],
                    inv1: [0.0; 4],
                    info: [1, 0, 0, 0],
                    trans: [0; 4],
                }),
                Op::Pop { opacity, mode } => commands.push(GpuCommand {
                    inv0: [0.0; 4],
                    inv1: [0.0, 0.0, *opacity, *mode as f32],
                    info: [2, 0, 0, 0],
                    trans: [0; 4],
                }),
                Op::Draw {
                    inv,
                    q,
                    opacity,
                    mode,
                    tiles,
                } => {
                    let start = entries.len() as u32;
                    for tile in tiles {
                        let slot = atlas_tiles.len() as u32;
                        atlas_tiles.push(tile.pixels.clone());
                        entries.push(GpuTile {
                            xy: [tile.tx, tile.ty],
                            slot,
                            pad: 0,
                        });
                    }
                    // Version 3 takes its draw from the host (`plain_draw_v3`).
                    if integer {
                        let d = crate::renderer::plain_draw_v3(
                            q.expect("v3 draw inverse"),
                            (case.level, case.tx, case.ty),
                            [case.w as i32, case.h as i32],
                            tiles
                                .iter()
                                .map(|t| alto_core::TileKey { tx: t.tx, ty: t.ty }),
                        )
                        .map_err(|e| format!("{}: {e}", case.name))?;
                        // A draw that reaches none of its tiles is skipped, as the planners do.
                        if d.keys.is_none() {
                            continue;
                        }
                        commands.push(GpuCommand {
                            inv0: d.inverse.m.map(|v| f32::from_bits(v.cast_unsigned())),
                            inv1: [0.0, 0.0, *opacity, *mode as f32],
                            info: [0, start, entries.len() as u32 - start, d.sample],
                            trans: crate::sample::pack_wide(d.inverse.tx, d.inverse.ty),
                        });
                        continue;
                    }
                    let packed = crate::sample::pack_inverse(*inv, version);
                    commands.push(GpuCommand {
                        inv0: packed.inv0,
                        inv1: [
                            packed.translation[0],
                            packed.translation[1],
                            *opacity,
                            *mode as f32,
                        ],
                        info: [0, start, entries.len() as u32 - start, packed.sample_mode],
                        trans: packed.trans,
                    });
                }
            }
        }
        if integer {
            for c in &mut commands {
                c.inv1[2] = f32::from_bits(crate::composite_opacity_q14(c.inv1[2]) as u32);
                c.inv1[3] = f32::from_bits(c.inv1[3] as u32);
            }
        }
        let layers = (atlas_tiles.len() as u32).max(1);
        let device = &self.device;
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("parity atlas"),
            size: wgpu::Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (slot, pixels) in atlas_tiles.iter().enumerate() {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: slot as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(1024),
                    rows_per_image: Some(256),
                },
                wgpu::Extent3d {
                    width: 256,
                    height: 256,
                    depth_or_array_layers: 1,
                },
            );
        }
        let integer_output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("integer parity bytes"),
            size: 256 * 256 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let integer_pipeline = if integer {
            let src = integer_parity_shader();
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("integer parity"),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            });
            Some(
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("integer parity"),
                    layout: None,
                    module: &module,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    cache: None,
                }),
            )
        } else {
            None
        };
        let pipeline = integer_pipeline.as_ref().unwrap_or(&self.pipeline);
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("parity output"),
            size: wgpu::Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let scale = 2f32.powi(case.level);
        let config = [GpuConfig {
            origin_scale: [
                case.tx as f32 * 256.0 * scale,
                case.ty as f32 * 256.0 * scale,
                scale,
                case.w as f32,
            ],
            info: [commands.len() as u32, 0, case.h, 0],
        }];
        let commands_buf =
            upload_buffer(device, &self.queue, &commands, wgpu::BufferUsages::STORAGE);
        let tiles_buf = upload_buffer(device, &self.queue, &entries, wgpu::BufferUsages::STORAGE);
        let config_buf = if integer {
            // The host's integer uniform: origin, command count, on-canvas span.
            let origin = [case.tx, case.ty].map(|k| i64::from(k) * 256);
            upload_buffer(
                device,
                &self.queue,
                &[crate::renderer::IntegerConfig {
                    origin_scale: [origin[0] as i32, origin[1] as i32, 1, 0],
                    info: [commands.len() as u32, 0, 0, 0],
                    span: crate::renderer::resolve_span(
                        origin,
                        1,
                        [i64::from(case.w), i64::from(case.h)],
                    ),
                }],
                wgpu::BufferUsages::UNIFORM,
            )
        } else {
            upload_buffer(device, &self.queue, &config, wgpu::BufferUsages::UNIFORM)
        };
        let pre_bytes = 256 * 256 * 16;
        let pre_quant = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("parity prequant"),
            size: pre_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let atlas_view = atlas.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let output_view = output.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("parity composite"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: if integer {
                        integer_output.as_entire_binding()
                    } else {
                        wgpu::BindingResource::TextureView(&output_view)
                    },
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: commands_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: tiles_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: config_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: pre_quant.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("parity composite"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("parity composite"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(16, 16, 1);
        }
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("parity readback"),
            size: 256 * 1024,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        if integer {
            encoder.copy_buffer_to_buffer(&integer_output, 0, &read, 0, 256 * 1024);
        } else {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &output,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &read,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(1024),
                        rows_per_image: Some(256),
                    },
                },
                wgpu::Extent3d {
                    width: 256,
                    height: 256,
                    depth_or_array_layers: 1,
                },
            );
        }
        let pre_read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("parity prequant readback"),
            size: pre_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_buffer_to_buffer(&pre_quant, 0, &pre_read, 0, pre_bytes);
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        self.queue.submit(Some(encoder.finish()));
        map_readback(device, &read)?;
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(format!("validation: {err}"));
        }
        let bytes = read
            .slice(..)
            .get_mapped_range()
            .map_err(|e| e.to_string())?
            .to_vec();
        read.unmap();
        map_readback(device, &pre_read)?;
        let pre_raw = pre_read
            .slice(..)
            .get_mapped_range()
            .map_err(|e| e.to_string())?
            .to_vec();
        pre_read.unmap();
        let prequant = bytemuck::cast_slice(&pre_raw).to_vec();
        Ok((bytes, prequant))
    }

    /// Runs Q14 stage programs through the production `effects-q14.wgsl` and
    /// `effects-resolve-q14.wgsl`. Targets are `rgba16uint` and start at zero, so
    /// an unwritten target is the empty backdrop. Each program's result target is
    /// resolved to `rgba8uint` and read back.
    pub(super) fn render_q14(
        &self,
        images: &[Vec<u8>],
        programs: &[Q14Program],
    ) -> Result<Vec<Vec<u8>>, String> {
        let device = &self.device;
        let compile = |label: &str, source: String| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: None,
                module: &device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(label),
                    source: wgpu::ShaderSource::Wgsl(source.into()),
                }),
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let effects = compile("q14 stages", crate::integer_effects_shader());
        let resolve = compile(
            "q14 resolve",
            include_str!("../../shaders/effects-resolve-q14.wgsl").to_string(),
        );
        let size = |layers: usize| wgpu::Extent3d {
            width: 256,
            height: 256,
            depth_or_array_layers: layers as u32,
        };
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("q14 atlas"),
            size: size(images.len()),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (layer, pixels) in images.iter().enumerate() {
            if pixels.len() != 256 * 256 * 4 {
                return Err(format!("q14 image {layer} is {} bytes", pixels.len()));
            }
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(1024),
                    rows_per_image: Some(256),
                },
                size(1),
            );
        }
        let atlas_v = atlas.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("q14 resolve"),
            size: size(1),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Uint,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_v = output.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        // The programs have no canvas: the resolve span is the whole tile, and
        // the slot is 0 (`effects-resolve-q14.wgsl` `span`, `info`).
        let resolve_config = upload_buffer(
            device,
            &self.queue,
            &[[0u32, 0, 256, 256], [0; 4]],
            wgpu::BufferUsages::UNIFORM,
        );
        let mut tiles_out = Vec::with_capacity(programs.len());
        for program in programs {
            let targets: Vec<_> = (0..program.targets)
                .map(|_| {
                    device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("q14 target"),
                        size: size(1),
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba16Uint,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::STORAGE_BINDING,
                        view_formats: &[],
                    })
                })
                .collect();
            let views: Vec<_> = targets
                .iter()
                .map(|t| t.create_view(&Default::default()))
                .collect();
            let mut encoder = device.create_command_encoder(&Default::default());
            let mut keep = Vec::new();
            for stage in &program.stages {
                if stage.out == stage.a || stage.out == stage.b {
                    return Err(format!("{}: stage reads its own output", program.name));
                }
                let tile = [GpuTile {
                    xy: [0, 0],
                    slot: stage.slot,
                    pad: 0,
                }];
                let tiles = upload_buffer(device, &self.queue, &tile, wgpu::BufferUsages::STORAGE);
                // Every stage takes its whole uniform from the host's
                // `mask_sampler`, `input_map` and `mask_grid`, as
                // `Context::q14_params` builds it; the float slots stay zero.
                let f32_region = |t: usize| program.region(t).map(|v| v as f32);
                let out_region = f32_region(stage.out);
                let sampler = stage
                    .lookup_transform(program.region(stage.out))
                    .map(|t| {
                        crate::renderer::fixed_mask_sampler(
                            &alto_core::FixedAffine::from_affine(Affine2(t)).ok_or_else(|| {
                                format!("{}: v3 transform out of range", program.name)
                            })?,
                            program.region(stage.out),
                        )
                        .ok_or_else(|| format!("{}: v3 refuses the lookup", program.name))
                    })
                    .transpose()?;
                let map = |t: usize, read: bool| {
                    if !read {
                        return Ok([0; 4]);
                    }
                    crate::renderer::input_map(out_region, [256, 256], f32_region(t))
                        .ok_or_else(|| format!("{}: v3 refuses the region", program.name))
                };
                let [read_a, read_b] = stage.reads();
                let curve = crate::renderer::mask_grid(
                    stage.settings,
                    map(stage.a, read_a)?,
                    map(stage.b, read_b)?,
                    sampler.as_ref(),
                );
                let uniform = upload_buffer(
                    device,
                    &self.queue,
                    &[EffectConfig {
                        info: [
                            stage.mode,
                            0,
                            u32::from(sampler.is_some()),
                            crate::sample::SAMPLE_INTEGER,
                        ],
                        curve,
                        sizes: [256; 4],
                        output_clip: [256; 4],
                        ..EffectConfig::zeroed()
                    }],
                    wgpu::BufferUsages::UNIFORM,
                );
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("q14 stage"),
                    layout: &effects.get_bind_group_layout(0),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&views[stage.a]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&views[stage.b]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&views[stage.a]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(&atlas_v),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: wgpu::BindingResource::TextureView(&views[stage.out]),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: tiles.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 6,
                            resource: uniform.as_entire_binding(),
                        },
                    ],
                });
                {
                    let mut pass = encoder.begin_compute_pass(&Default::default());
                    pass.set_pipeline(&effects);
                    pass.set_bind_group(0, &bind, &[]);
                    pass.dispatch_workgroups(16, 16, 1);
                }
                keep.push((tiles, uniform));
            }
            let resolve_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("q14 resolve"),
                layout: &resolve.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&views[program.result]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&output_v),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: resolve_config.as_entire_binding(),
                    },
                ],
            });
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&resolve);
                pass.set_bind_group(0, &resolve_bind, &[]);
                pass.dispatch_workgroups(16, 16, 1);
            }
            let read = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("q14 readback"),
                size: 256 * 1024,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &output,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &read,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(1024),
                        rows_per_image: Some(256),
                    },
                },
                size(1),
            );
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            self.queue.submit(Some(encoder.finish()));
            map_readback(device, &read)?;
            if let Some(err) = pollster::block_on(scope.pop()) {
                return Err(format!("{}: validation: {err}", program.name));
            }
            let bytes = read
                .slice(..)
                .get_mapped_range()
                .map_err(|e| e.to_string())?
                .to_vec();
            read.unmap();
            drop(keep);
            tiles_out.push(bytes);
        }
        Ok(tiles_out)
    }
}

pub(super) fn upload_buffer<T: Pod + Zeroable>(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    data: &[T],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let zero = T::zeroed();
    let bytes: &[u8] = if data.is_empty() {
        bytemuck::bytes_of(&zero)
    } else {
        bytemuck::cast_slice(data)
    };
    let size = (bytes.len() as u64).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("parity storage"),
        size,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut padded = bytes.to_vec();
    padded.resize(size as usize, 0);
    queue.write_buffer(&buffer, 0, &padded);
    buffer
}

pub(super) fn map_readback(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    for _ in 0..1000 {
        match rx.try_recv() {
            Ok(result) => return result.map_err(|e| format!("map: {e}")),
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .map_err(|e| format!("poll: {e}"))?;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err("map callback dropped".into());
            }
        }
    }
    Err("GPU readback did not complete".into())
}
