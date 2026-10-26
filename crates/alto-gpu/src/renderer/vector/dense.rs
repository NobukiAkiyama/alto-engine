//! Isolate eight overlapping batches in MRT coverage, then paint in original order.
//! No atlas translation, coverage mixing, or change to the RGBA16F blend sequence.
use super::*;

const LANES: usize = 8;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(super) struct DenseUniform {
    base: Uniform,
    slot: [u32; 4],
}
pub(super) struct Dense {
    geometry: [wgpu::RenderPipeline; LANES],
    paint: wgpu::RenderPipeline,
    geometry_layout: wgpu::BindGroupLayout,
    paint_layout: wgpu::BindGroupLayout,
    pub uniforms: arena::Arena<DenseUniform>,
    coverage: Vec<Coverage>,
}
impl Dense {
    pub fn supported(device: &wgpu::Device) -> bool {
        let limits = device.limits();
        limits.max_color_attachments >= LANES as u32
            && limits.max_color_attachment_bytes_per_sample >= LANES as u32
            && limits.max_sampled_textures_per_shader_stage >= LANES as u32
    }
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let uniform = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<DenseUniform>() as u64),
            },
            count: None,
        };
        let geometry_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dense vector geometry"),
            entries: &[uniform],
        });
        let mut entries = vec![uniform];
        entries.extend((1..=LANES).map(|i| wgpu::BindGroupLayoutEntry {
            binding: i as u32,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        }));
        let paint_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dense vector paint"),
            entries: &entries,
        });
        let geometry_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dense vector geometry"),
            bind_group_layouts: &[Some(&geometry_layout)],
            immediate_size: 0,
        });
        let paint_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dense vector paint"),
            bind_group_layouts: &[Some(&paint_layout)],
            immediate_size: 0,
        });
        let geometry_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dense vector coverage"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("../../../shaders/vector.wgsl"),
                    "\n",
                    include_str!("../../../shaders/vector-dense-coverage.wgsl")
                )
                .into(),
            ),
        });
        let geometry = std::array::from_fn(|lane| {
            let targets: Vec<_> = (0..LANES)
                .map(|i| {
                    Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::R8Unorm,
                        blend: None,
                        write_mask: if i == lane {
                            wgpu::ColorWrites::RED
                        } else {
                            wgpu::ColorWrites::empty()
                        },
                    })
                })
                .collect();
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("dense vector isolated coverage"),
                layout: Some(&geometry_pl),
                vertex: wgpu::VertexState {
                    module: &geometry_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: 8,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0=>Float32x2],
                    })],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &geometry_shader,
                    entry_point: Some("dense_coverage"),
                    compilation_options: Default::default(),
                    targets: &targets,
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState {
                    count: 4,
                    ..Default::default()
                },
                multiview_mask: None,
                cache: None,
            })
        });
        let paint_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dense vector ordered paint"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../../../shaders/vector-dense-paint.wgsl").into(),
            ),
        });
        let paint = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dense vector ordered paint"),
            layout: Some(&paint_pl),
            vertex: wgpu::VertexState {
                module: &paint_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &paint_shader,
                entry_point: Some("paint"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            geometry,
            paint,
            geometry_layout,
            paint_layout,
            uniforms: Default::default(),
            coverage: Vec::new(),
        }
    }
    pub fn bytes(&self) -> u64 {
        self.uniforms.bytes()
            + self
                .coverage
                .iter()
                .map(|c| c.w as u64 * c.h as u64 * 5)
                .sum::<u64>()
    }
    pub fn retire(self, retired: &mut super::super::Retired) {
        self.uniforms.retire(retired);
        for c in self.coverage {
            retired.texture(c.multi);
            retired.texture(c.resolved);
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        first: &Coverage,
        meshes: &HashMap<(ContentHash, i32), CachedMesh>,
        draws: &[PreparedDraw],
        budget: u64,
        counters: &mut Counters,
        retired: &mut super::super::Retired,
    ) -> bool {
        let Some(arena_bytes) = self.uniforms.required_bytes(device, draws.len()) else {
            return false;
        };
        let bytes = first.w as u64 * first.h as u64 * 5 * (LANES - 1) as u64;
        if bytes + arena_bytes > budget {
            return false;
        }
        if self
            .coverage
            .first()
            .is_none_or(|c| c.w != first.w || c.h != first.h)
        {
            for c in self.coverage.drain(..) {
                retired.texture(c.multi);
                retired.texture(c.resolved);
            }
            self.uniforms.invalidate_paint();
            for _ in 1..LANES {
                self.coverage.push(Coverage::new(device, first.w, first.h));
            }
        }
        let views: Vec<_> = std::iter::once(first).chain(self.coverage.iter()).collect();
        let resolved: Vec<_> = views.iter().map(|c| &c.resolved_view).collect();
        let values: Vec<_> = draws
            .iter()
            .enumerate()
            .map(|(i, d)| DenseUniform {
                base: d.uniform,
                slot: [(i % LANES) as u32, 0, 0, 0],
            })
            .collect();
        let Some(start) = self.uniforms.upload(
            device,
            queue,
            &self.geometry_layout,
            &self.paint_layout,
            &resolved,
            &values,
            budget - bytes,
            counters,
        ) else {
            return false;
        };
        for (chunk_index, chunk) in draws.chunks(LANES).enumerate() {
            counters.passes += 2;
            counters.coverage += 1;
            counters.paint += 1;
            counters.dense_groups += 1;
            counters.clear_pixels += first.w as u64 * first.h as u64 * LANES as u64;
            counters.resolve_pixels += first.w as u64 * first.h as u64 * LANES as u64;
            let attachments: Vec<_> = views
                .iter()
                .map(|c| {
                    Some(wgpu::RenderPassColorAttachment {
                        view: &c.multi_view,
                        resolve_target: Some(&c.resolved_view),
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Discard,
                        },
                    })
                })
                .collect();
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("dense vector coverage"),
                    color_attachments: &attachments,
                    ..Default::default()
                });
                for (lane, d) in chunk.iter().enumerate() {
                    let b = &meshes[&d.key].batches[d.batch];
                    let [x0, y0, x1, y1] = d.rect.0;
                    let (page, offset) = self.uniforms.offset(start + chunk_index * LANES + lane);
                    pass.set_pipeline(&self.geometry[lane]);
                    pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
                    pass.set_bind_group(0, &self.uniforms.pages[page].geometry, &[offset]);
                    pass.set_vertex_buffer(0, b.vertices.slice(..));
                    pass.set_index_buffer(b.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..b.count, 0, 0..1);
                    counters.geometry_draws += 1;
                }
            }
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("dense vector ordered paint"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(&self.paint);
                for (lane, d) in chunk.iter().enumerate() {
                    let [x0, y0, x1, y1] = d.rect.0;
                    let (page, offset) = self.uniforms.offset(start + chunk_index * LANES + lane);
                    pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
                    pass.set_bind_group(
                        0,
                        self.uniforms.pages[page].paint.as_ref().unwrap(),
                        &[offset],
                    );
                    pass.draw(0..3, 0..1);
                    counters.paint_draws += 1;
                }
            }
        }
        true
    }
}
