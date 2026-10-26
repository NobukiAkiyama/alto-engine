//! Lyon meshes -> 4x MSAA R8 coverage -> premultiplied color, including stroke unions.
//! Text adds the glyph atlas for runs whose em is small on the display level.
use super::*;
use crate::glyph_atlas::{
    ATLAS_MAX_EM_PX, ATLAS_SIDE, AtlasStats, GlyphAtlas, max_scale, place, rasterize,
};
use alto_vector::{LyonRasterizer, StyledPath, TextRun, VectorRasterizer};
mod arena;
mod batch;
mod dense;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorMode {
    Legacy,
    Pooled,
    Disjoint,
    Dense,
    Batched,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub passes: u64,
    pub coverage: u64,
    pub paint: u64,
    pub geometry_draws: u64,
    pub paint_draws: u64,
    pub uniform_buffers: u64,
    pub bind_groups: u64,
    pub upload_bytes: u64,
    pub clear_pixels: u64,
    pub resolve_pixels: u64,
    pub paths: u64,
    pub groups: u64,
    pub comparisons: u64,
    pub barriers: u64,
    pub dense_groups: u64,
}
struct PreparedDraw {
    key: (ContentHash, i32),
    batch: usize,
    rect: batch::Rect,
    uniform: Uniform,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlyphInstance {
    dst: [i32; 4],
    src: [u32; 2],
    color: [f32; 4],
}
struct Atlas {
    alloc: GlyphAtlas,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniform {
    matrix: [f32; 4],
    offset_size: [f32; 4],
    color: [f32; 4],
}
struct Batch {
    origin: [f64; 2],
    bounds: [f32; 4],
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
    color: [f32; 4],
}
struct CachedMesh {
    batches: Vec<Batch>,
    bytes: u64,
    used: u64,
}
impl CachedMesh {
    fn retire(self, retired: &mut super::Retired) {
        for b in self.batches {
            retired.buffer(&b.vertices);
            retired.buffer(&b.indices);
        }
    }
}
struct Coverage {
    multi: wgpu::Texture,
    resolved: wgpu::Texture,
    multi_view: wgpu::TextureView,
    resolved_view: wgpu::TextureView,
    w: u32,
    h: u32,
}
impl Coverage {
    fn new(device: &wgpu::Device, w: u32, h: u32) -> Self {
        let make = |samples, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("R8 vector coverage"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage,
                view_formats: &[],
            })
        };
        let multi = make(4, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let resolved = make(
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        Self {
            multi_view: multi.create_view(&Default::default()),
            resolved_view: resolved.create_view(&Default::default()),
            multi,
            resolved,
            w,
            h,
        }
    }
}
fn dense_candidate(paths: usize, groups: usize) -> bool {
    // Only pay for eight coverage images when almost every path is an overlap barrier.
    paths >= 8 && groups >= paths - paths / 5
}
pub struct VectorResources {
    format: wgpu::TextureFormat,
    dense: Option<dense::Dense>,
    geometry: wgpu::RenderPipeline,
    pooled_geometry: wgpu::RenderPipeline,
    pooled_paint: wgpu::RenderPipeline,
    geometry_layout: wgpu::BindGroupLayout,
    paint_layout: wgpu::BindGroupLayout,
    uniforms: arena::Arena,
    mode: VectorMode,
    counters: Counters,
    paint: wgpu::RenderPipeline,
    meshes: HashMap<(ContentHash, i32), CachedMesh>,
    coverage: Option<Coverage>,
    tessellator: LyonRasterizer,
    glyph: wgpu::RenderPipeline,
    atlas: Option<Atlas>,
    glyph_stats: AtlasStats,
    pub hits: u64,
    pub misses: u64,
    /// Per-draw buffers and replaced textures; the renderer destroys them after it
    /// submits the commands that use them.
    pub(crate) retired: super::Retired,
}
impl VectorResources {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let geometry = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vector coverage"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../../shaders/vector.wgsl").into()),
        });
        let paint = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vector paint"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../../shaders/vector-paint.wgsl").into(),
            ),
        });
        let g = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("4x MSAA R8 vector coverage"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &geometry,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 8,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x2],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &geometry,
                entry_point: Some("coverage"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 4,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });
        let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vector coverage paint"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &paint,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &paint,
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
        let uniform_entry = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Uniform>() as u64),
            },
            count: None,
        };
        let geometry_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vector dynamic geometry"),
            entries: &[uniform_entry],
        });
        let paint_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vector dynamic paint"),
            entries: &[
                uniform_entry,
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let geometry_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vector dynamic geometry"),
            bind_group_layouts: &[Some(&geometry_layout)],
            immediate_size: 0,
        });
        let paint_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vector dynamic paint"),
            bind_group_layouts: &[Some(&paint_layout)],
            immediate_size: 0,
        });
        let pooled_geometry = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("4x MSAA R8 vector coverage"),
            layout: Some(&geometry_pl),
            vertex: wgpu::VertexState {
                module: &geometry,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 8,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0=>Float32x2],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &geometry,
                entry_point: Some("coverage"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 4,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });
        let pooled_paint = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vector coverage paint"),
            layout: Some(&paint_pl),
            vertex: wgpu::VertexState {
                module: &paint,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &paint,
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
        let glyph = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glyph atlas"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../../shaders/glyph-atlas.wgsl").into()),
        });
        let glyph = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glyph atlas paint"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &glyph,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<GlyphInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0=>Sint32x4, 1=>Uint32x2, 2=>Float32x4],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &glyph,
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
            format,
            dense: None,
            geometry: g,
            pooled_geometry,
            pooled_paint,
            geometry_layout,
            paint_layout,
            uniforms: arena::Arena::default(),
            mode: VectorMode::Batched,
            counters: Counters::default(),
            paint: p,
            meshes: HashMap::new(),
            coverage: None,
            tessellator: LyonRasterizer::default(),
            glyph,
            atlas: None,
            glyph_stats: AtlasStats::default(),
            hits: 0,
            retired: super::Retired::default(),
            misses: 0,
        }
    }
    pub fn supports_dense(device: &wgpu::Device) -> bool {
        dense::Dense::supported(device)
    }
    pub fn set_mode(&mut self, mode: VectorMode) {
        self.mode = mode;
    }
    pub fn take_counters(&mut self) -> Counters {
        std::mem::take(&mut self.counters)
    }
    /// Called only after submitting every command encoded with this resource set.
    pub fn after_submit(&mut self) {
        self.uniforms.reset();
        if let Some(dense) = &mut self.dense {
            dense.uniforms.reset();
        }
        self.retired.release();
    }
    /// Standalone probes call this after submitting all work, before dropping resources.
    pub fn destroy_after_submit(self) {
        let mut retired = super::Retired::default();
        self.retire(&mut retired);
        retired.release();
    }
    fn check_allocation(&self, additional: u64, budget: u64) -> Result<()> {
        if self
            .bytes()
            .saturating_add(self.retired.bytes().total())
            .saturating_add(additional)
            > budget
        {
            return Err(RenderError::Budget(
                "vector allocation exceeds GPU budget (including retired allocations)".into(),
            ));
        }
        Ok(())
    }
    pub fn arena_bytes(&self) -> u64 {
        self.uniforms.bytes() + self.dense.as_ref().map_or(0, |d| d.uniforms.bytes())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn render_legacy(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Texture,
        paths: &[&StyledPath],
        world: [f64; 6],
        roi: [f64; 5],
        frame: u64,
        budget: u64,
    ) -> Result<()> {
        let mode = self.mode;
        self.mode = VectorMode::Legacy;
        let result = self.render(
            device, queue, encoder, target, paths, world, roi, frame, budget,
        );
        self.mode = mode;
        result
    }
    pub fn mesh_bytes(&self) -> u64 {
        self.meshes.values().map(|m| m.bytes).sum()
    }
    /// Drops tessellation caches. Coverage and the glyph atlas stay; they belong to the frame.
    pub fn discard_meshes(&mut self) {
        for (_, mesh) in self.meshes.drain() {
            mesh.retire(&mut self.retired);
        }
    }
    /// Hands every GPU object to `retired`, for when these resources are discarded.
    pub(crate) fn retire(mut self, retired: &mut super::Retired) {
        self.discard_meshes();
        std::mem::take(&mut self.uniforms).retire(retired);
        if let Some(dense) = self.dense.take() {
            dense.retire(retired);
        }
        retired.take(&mut self.retired);
        if let Some(c) = self.coverage.take() {
            retired.texture(c.multi);
            retired.texture(c.resolved);
        }
        if let Some(a) = self.atlas.take() {
            retired.texture(a.texture);
        }
    }
    pub fn bytes(&self) -> u64 {
        self.mesh_bytes()
            + self.uniforms.bytes()
            + self.dense.as_ref().map_or(0, |d| d.bytes())
            + self
                .coverage
                .as_ref()
                .map_or(0, |c| c.w as u64 * c.h as u64 * 5)
            + self.atlas.as_ref().map_or(0, |a| {
                let (w, h) = a.alloc.size();
                w as u64 * h as u64
            })
    }
    pub fn counts(&self) -> (u64, u64, usize) {
        (self.hits, self.misses, self.meshes.len())
    }
    pub fn glyph_stats(&self) -> AtlasStats {
        let mut s = self.glyph_stats;
        if let Some(a) = &self.atlas {
            let live = a.alloc.stats();
            s.entries = live.entries;
            s.shelves = live.shelves;
            s.rasterized += live.rasterized;
            s.hits += live.hits;
            s.evicted += live.evicted;
            s.resets += live.resets;
        }
        s
    }
    /// Text: runs whose em is below `ATLAS_MAX_EM_PX` output pixels on this level copy
    /// glyph coverage from the atlas; the rest (and glyphs the atlas cannot hold) take
    /// the vector path.
    #[allow(clippy::too_many_arguments)]
    pub fn render_text(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Texture,
        runs: &[TextRun],
        world: [f64; 6],
        roi: [f64; 5],
        frame: u64,
        budget: u64,
        use_atlas: bool,
    ) -> Result<()> {
        let [x, y, scale, w, h] = roi;
        let px_per_unit = max_scale([world[0], world[1], world[2], world[3]]) / scale;
        let mut vector: Vec<&StyledPath> = vec![];
        let mut fallback = vec![];
        let mut instances: Vec<GlyphInstance> = vec![];
        for run in runs {
            let em_px = run.em * px_per_unit;
            if !use_atlas || !em_px.is_finite() || em_px >= ATLAS_MAX_EM_PX {
                vector.extend(run.paths.iter());
                self.glyph_stats.vector_runs += 1;
                continue;
            }
            if self.atlas.is_none() {
                self.check_allocation(ATLAS_SIDE as u64 * ATLAS_SIDE as u64, budget)?;
            }
            let atlas = self.atlas.get_or_insert_with(|| {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("R8 glyph atlas"),
                    size: wgpu::Extent3d {
                        width: ATLAS_SIDE,
                        height: ATLAS_SIDE,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = texture.create_view(&Default::default());
                Atlas {
                    alloc: GlyphAtlas::new(ATLAS_SIDE, ATLAS_SIDE),
                    texture,
                    view,
                }
            });
            atlas.alloc.begin(frame);
            for g in &run.glyphs {
                if g.outline.is_empty() {
                    continue;
                }
                let t = multiply(world, g.transform);
                let m = [
                    t[0] / scale,
                    t[1] / scale,
                    t[2] / scale,
                    t[3] / scale,
                    (t[4] - x) / scale,
                    (t[5] - y) / scale,
                ];
                let Some(p) = place(&g.outline, m) else {
                    fallback.push(g.to_path());
                    continue;
                };
                if p.dst[0] >= w as i64
                    || p.dst[1] >= h as i64
                    || p.dst[0] + p.w as i64 <= 0
                    || p.dst[1] + p.h as i64 <= 0
                {
                    continue;
                }
                let rect = match atlas.alloc.get(&p.key) {
                    Some(r) => r,
                    None => match atlas.alloc.insert(p.key, p.w, p.h) {
                        Some(r) => {
                            queue.write_texture(
                                wgpu::TexelCopyTextureInfo {
                                    texture: &atlas.texture,
                                    mip_level: 0,
                                    origin: wgpu::Origin3d {
                                        x: r.x,
                                        y: r.y,
                                        z: 0,
                                    },
                                    aspect: wgpu::TextureAspect::All,
                                },
                                &rasterize(&g.outline, p.raster, p.w, p.h),
                                wgpu::TexelCopyBufferLayout {
                                    offset: 0,
                                    bytes_per_row: Some(p.w),
                                    rows_per_image: Some(p.h),
                                },
                                wgpu::Extent3d {
                                    width: p.w,
                                    height: p.h,
                                    depth_or_array_layers: 1,
                                },
                            );
                            r
                        }
                        None => {
                            fallback.push(g.to_path());
                            continue;
                        }
                    },
                };
                instances.push(GlyphInstance {
                    dst: [p.dst[0] as i32, p.dst[1] as i32, p.w as i32, p.h as i32],
                    src: [rect.x, rect.y],
                    color: g.color,
                });
            }
        }
        self.glyph_stats.atlas_glyphs += instances.len() as u64;
        self.glyph_stats.fallback_glyphs += fallback.len() as u64;
        vector.extend(fallback.iter());
        self.render_legacy(
            device, queue, encoder, target, &vector, world, roi, frame, budget,
        )?;
        let Some(atlas) = self.atlas.as_ref().filter(|_| !instances.is_empty()) else {
            return Ok(());
        };
        self.check_allocation(
            16 + (instances.len().max(1) * std::mem::size_of::<GlyphInstance>()) as u64,
            budget,
        )?;
        let uniform = buffer(
            device,
            queue,
            "glyph atlas config",
            &[[w as f32, h as f32, 0.0, 0.0]],
            wgpu::BufferUsages::UNIFORM,
        );
        let glyphs = buffer(
            device,
            queue,
            "glyph instances",
            &instances,
            wgpu::BufferUsages::VERTEX,
        );
        self.retired.buffer(&uniform);
        self.retired.buffer(&glyphs);
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glyph atlas"),
            layout: &self.glyph.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas.view),
                },
            ],
        });
        let view = target.create_view(&Default::default());
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("paint glyph atlas"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.glyph);
        pass.set_bind_group(0, &bg, &[]);
        pass.set_vertex_buffer(0, glyphs.slice(..));
        pass.draw(0..6, 0..instances.len() as u32);
        Ok(())
    }
    /// ROI is [origin x, origin y, document pixels per output pixel, width, height].
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Texture,
        paths: &[&StyledPath],
        world: [f64; 6],
        roi: [f64; 5],
        frame: u64,
        budget: u64,
    ) -> Result<()> {
        let [x, y, scale, w, h] = roi;
        let (w, h) = (w as u32, h as u32);
        let coverage_bytes = w as u64 * h as u64 * 5;
        // Optional scratch must not make an otherwise renderable call exceed its budget.
        let regular_fixed = coverage_bytes
            + self.uniforms.bytes()
            + self.atlas.as_ref().map_or(0, |a| {
                let (w, h) = a.alloc.size();
                w as u64 * h as u64
            });
        if self.dense.is_some()
            && regular_fixed + self.mesh_bytes() + self.dense.as_ref().map_or(0, |d| d.bytes())
                > budget
            && let Some(dense) = self.dense.take()
        {
            dense.retire(&mut self.retired);
        }
        let fixed_bytes = regular_fixed + self.dense.as_ref().map_or(0, |d| d.bytes());
        let mesh_limit = 32 * 1024 * 1024u64;
        // Frobenius norm bounds the maximum singular value, including shear and nonuniform scale.
        let transform_scale =
            (world[0] * world[0] + world[1] * world[1] + world[2] * world[2] + world[3] * world[3])
                .sqrt();
        if !transform_scale.is_finite() || scale <= 0.0 {
            return Err(RenderError::InvalidArgument("vector transform".into()));
        }
        let band = (scale / transform_scale.max(1e-12))
            .log2()
            .floor()
            .clamp(-24.0, 24.0) as i32;
        let tolerance = (0.15 * 2f64.powi(band)).clamp(0.000001, 1024.0) as f32;
        let mut keys = vec![];
        for p in paths {
            let b = p.data.bounds();
            let inflate = p
                .stroke
                .as_ref()
                .map_or(0.0, |s| s.width as f64 * s.miter_limit as f64);
            let corners = [
                [b[0] - inflate, b[1] - inflate],
                [b[2] + inflate, b[1] - inflate],
                [b[0] - inflate, b[3] + inflate],
                [b[2] + inflate, b[3] + inflate],
            ]
            .map(|q| point(world, q));
            if corners.iter().all(|q| q[0] < x)
                || corners.iter().all(|q| q[1] < y)
                || corners.iter().all(|q| q[0] > x + w as f64 * scale)
                || corners.iter().all(|q| q[1] > y + h as f64 * scale)
            {
                continue;
            }
            let key = (p.fingerprint(), band);
            if let Some(c) = self.meshes.get_mut(&key) {
                c.used = frame;
                self.hits += 1;
                keys.push(key);
                continue;
            }
            let meshes = self
                .tessellator
                .prepare(std::slice::from_ref(*p), tolerance)
                .map_err(|e| RenderError::Internal(e.to_string()))?;
            let bytes = meshes.iter().map(|m| m.bytes() as u64).sum::<u64>();
            if bytes > mesh_limit || bytes + fixed_bytes > budget {
                return Err(RenderError::InvalidArgument(
                    "vector mesh exceeds GPU budget".into(),
                ));
            }
            while self.meshes.values().map(|m| m.bytes).sum::<u64>() + bytes
                > mesh_limit.min(budget - fixed_bytes)
            {
                let old = self
                    .meshes
                    .iter()
                    .filter(|(_, m)| m.used != frame)
                    .min_by_key(|(_, m)| m.used)
                    .map(|(k, _)| *k)
                    .ok_or(RenderError::InvalidArgument(
                        "visible vector meshes exceed GPU budget".into(),
                    ))?;
                if let Some(mesh) = self.meshes.remove(&old) {
                    mesh.retire(&mut self.retired);
                }
            }
            // Evicted objects still occupy memory until this encoder is submitted
            // and its completion callback reclaims their ledger entries.
            self.check_allocation(bytes, budget)?;
            let batches = meshes
                .into_iter()
                .filter(|m| !m.indices.is_empty())
                .map(|m| Batch {
                    origin: m.origin,
                    bounds: m.vertices.iter().fold(
                        [
                            f32::INFINITY,
                            f32::INFINITY,
                            f32::NEG_INFINITY,
                            f32::NEG_INFINITY,
                        ],
                        |a, p| {
                            [
                                a[0].min(p[0]),
                                a[1].min(p[1]),
                                a[2].max(p[0]),
                                a[3].max(p[1]),
                            ]
                        },
                    ),
                    vertices: buffer(
                        device,
                        queue,
                        "vector vertices",
                        &m.vertices,
                        wgpu::BufferUsages::VERTEX,
                    ),
                    indices: buffer(
                        device,
                        queue,
                        "vector indices",
                        &m.indices,
                        wgpu::BufferUsages::INDEX,
                    ),
                    count: m.indices.len() as u32,
                    color: m.color,
                })
                .collect();
            self.meshes.insert(
                key,
                CachedMesh {
                    batches,
                    bytes,
                    used: frame,
                },
            );
            self.misses += 1;
            keys.push(key);
        }
        while self.meshes.values().map(|m| m.bytes).sum::<u64>() + fixed_bytes > budget {
            let old = self
                .meshes
                .iter()
                .filter(|(_, m)| m.used != frame)
                .min_by_key(|(_, m)| m.used)
                .map(|(k, _)| *k)
                .ok_or(RenderError::InvalidArgument(
                    "vector coverage exceeds GPU budget".into(),
                ))?;
            if let Some(mesh) = self.meshes.remove(&old) {
                mesh.retire(&mut self.retired);
            }
        }
        if self.coverage.as_ref().is_none_or(|c| c.w != w || c.h != h) {
            self.check_allocation(coverage_bytes, budget)?;
            if let Some(old) = self.coverage.take() {
                self.retired.texture(old.multi);
                self.retired.texture(old.resolved);
            }
            self.uniforms.invalidate_paint();
            if let Some(dense) = &mut self.dense {
                dense.uniforms.invalidate_paint();
            }
            self.coverage = Some(Coverage::new(device, w, h));
        }
        let coverage = self
            .coverage
            .as_ref()
            .ok_or(RenderError::Internal("vector coverage".into()))?;
        let multi = coverage.multi_view.clone();
        let resolved = coverage.resolved_view.clone();
        let target = target.create_view(&Default::default());
        {
            let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear vector content"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        self.counters.passes += 1;
        self.counters.paths += keys.len() as u64;
        if self.mode != VectorMode::Legacy
            && self.render_pooled(device, queue, encoder, &target, &keys, world, roi, budget)?
        {
            return Ok(());
        }
        for key in keys {
            for b in &self.meshes[&key].batches {
                let origin = point(world, b.origin);
                let corners = [
                    [b.bounds[0], b.bounds[1]],
                    [b.bounds[2], b.bounds[1]],
                    [b.bounds[0], b.bounds[3]],
                    [b.bounds[2], b.bounds[3]],
                ]
                .map(|p| {
                    point(
                        world,
                        [p[0] as f64 + b.origin[0], p[1] as f64 + b.origin[1]],
                    )
                });
                let x0 = (((corners.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min) - x)
                    / scale)
                    .floor()
                    - 1.0)
                    .clamp(0.0, w as f64) as u32;
                let y0 = (((corners.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min) - y)
                    / scale)
                    .floor()
                    - 1.0)
                    .clamp(0.0, h as f64) as u32;
                let x1 = (((corners
                    .iter()
                    .map(|p| p[0])
                    .fold(f64::NEG_INFINITY, f64::max)
                    - x)
                    / scale)
                    .ceil()
                    + 1.0)
                    .clamp(0.0, w as f64) as u32;
                let y1 = (((corners
                    .iter()
                    .map(|p| p[1])
                    .fold(f64::NEG_INFINITY, f64::max)
                    - y)
                    / scale)
                    .ceil()
                    + 1.0)
                    .clamp(0.0, h as f64) as u32;
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                self.counters.passes += 2;
                self.counters.coverage += 1;
                self.counters.paint += 1;
                self.counters.geometry_draws += 1;
                self.counters.paint_draws += 1;
                self.counters.uniform_buffers += 1;
                self.counters.bind_groups += 2;
                self.counters.upload_bytes += std::mem::size_of::<Uniform>() as u64;
                self.counters.clear_pixels += w as u64 * h as u64;
                self.counters.resolve_pixels += w as u64 * h as u64;
                self.check_allocation(std::mem::size_of::<Uniform>() as u64, budget)?;
                let uniform = buffer(
                    device,
                    queue,
                    "vector ROI uniform",
                    &[Uniform {
                        matrix: [
                            (world[0] / scale) as f32,
                            (world[1] / scale) as f32,
                            (world[2] / scale) as f32,
                            (world[3] / scale) as f32,
                        ],
                        offset_size: [
                            ((origin[0] - x) / scale) as f32,
                            ((origin[1] - y) / scale) as f32,
                            w as f32,
                            h as f32,
                        ],
                        color: b.color,
                    }],
                    wgpu::BufferUsages::UNIFORM,
                );
                self.retired.buffer(&uniform);
                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("vector geometry"),
                    layout: &self.geometry.get_bind_group_layout(0),
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    }],
                });
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("4x MSAA vector fill/stroke coverage"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &multi,
                            resolve_target: Some(&resolved),
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Discard,
                            },
                        })],
                        ..Default::default()
                    });
                    pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
                    pass.set_pipeline(&self.geometry);
                    pass.set_bind_group(0, &bg, &[]);
                    pass.set_vertex_buffer(0, b.vertices.slice(..));
                    pass.set_index_buffer(b.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..b.count, 0, 0..1);
                }
                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("vector paint"),
                    layout: &self.paint.get_bind_group_layout(0),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: uniform.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&resolved),
                        },
                    ],
                });
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("paint vector coverage"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &target,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    });
                    pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
                    pass.set_pipeline(&self.paint);
                    pass.set_bind_group(0, &bg, &[]);
                    pass.draw(0..3, 0..1);
                }
            }
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn render_pooled(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        keys: &[(ContentHash, i32)],
        world: [f64; 6],
        roi: [f64; 5],
        budget: u64,
    ) -> Result<bool> {
        let [x, y, scale, w, h] = roi;
        let mut draws = Vec::new();
        let mut paths = Vec::new();
        let mut rects = Vec::new();
        for &key in keys {
            let start = draws.len();
            let mut rect: Option<batch::Rect> = None;
            for (index, b) in self.meshes[&key].batches.iter().enumerate() {
                let origin = point(world, b.origin);
                let corners = [
                    [b.bounds[0], b.bounds[1]],
                    [b.bounds[2], b.bounds[1]],
                    [b.bounds[0], b.bounds[3]],
                    [b.bounds[2], b.bounds[3]],
                ]
                .map(|p| {
                    point(
                        world,
                        [p[0] as f64 + b.origin[0], p[1] as f64 + b.origin[1]],
                    )
                });
                if corners.iter().flatten().any(|v| !v.is_finite()) {
                    return Ok(false);
                }
                let x0 = (((corners.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min) - x)
                    / scale)
                    .floor()
                    - 1.0)
                    .clamp(0.0, w) as u32;
                let y0 = (((corners.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min) - y)
                    / scale)
                    .floor()
                    - 1.0)
                    .clamp(0.0, h) as u32;
                let x1 = (((corners
                    .iter()
                    .map(|p| p[0])
                    .fold(f64::NEG_INFINITY, f64::max)
                    - x)
                    / scale)
                    .ceil()
                    + 1.0)
                    .clamp(0.0, w) as u32;
                let y1 = (((corners
                    .iter()
                    .map(|p| p[1])
                    .fold(f64::NEG_INFINITY, f64::max)
                    - y)
                    / scale)
                    .ceil()
                    + 1.0)
                    .clamp(0.0, h) as u32;
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                let r = batch::Rect([x0, y0, x1, y1]);
                rect = Some(rect.map_or(r, |a| a.union(r)));
                draws.push(PreparedDraw {
                    key,
                    batch: index,
                    rect: r,
                    uniform: Uniform {
                        matrix: [
                            (world[0] / scale) as f32,
                            (world[1] / scale) as f32,
                            (world[2] / scale) as f32,
                            (world[3] / scale) as f32,
                        ],
                        offset_size: [
                            ((origin[0] - x) / scale) as f32,
                            ((origin[1] - y) / scale) as f32,
                            w as f32,
                            h as f32,
                        ],
                        color: b.color,
                    },
                });
            }
            paths.push(start..draws.len());
            rects.push(rect);
        }
        let coverage = self.coverage.as_ref().unwrap();
        let fixed = self.retired.bytes().total()
            + self.mesh_bytes()
            + w as u64 * h as u64 * 5
            + self.atlas.as_ref().map_or(0, |a| {
                let (w, h) = a.alloc.size();
                w as u64 * h as u64
            });
        let plan = if matches!(self.mode, VectorMode::Batched | VectorMode::Disjoint) {
            batch::plan(&rects)
        } else {
            batch::Plan {
                groups: (0..paths.len()).map(|i| i..i + 1).collect(),
                ..Default::default()
            }
        };
        let use_dense = self.mode == VectorMode::Dense
            || (self.mode == VectorMode::Batched
                && dense_candidate(paths.len(), plan.groups.len()));
        if use_dense && !draws.is_empty() && dense::Dense::supported(device) {
            let dense = self
                .dense
                .get_or_insert_with(|| dense::Dense::new(device, self.format));
            if dense.render(
                device,
                queue,
                encoder,
                target,
                coverage,
                &self.meshes,
                &draws,
                budget.saturating_sub(fixed + self.uniforms.bytes()),
                &mut self.counters,
                &mut self.retired,
            ) {
                self.counters.comparisons += plan.comparisons;
                self.counters.barriers += plan.barriers;
                return Ok(true);
            }
        }
        let values: Vec<_> = draws.iter().map(|d| d.uniform).collect();
        let Some(start) = self.uniforms.upload(
            device,
            queue,
            &self.geometry_layout,
            &self.paint_layout,
            &[&coverage.resolved_view],
            &values,
            budget.saturating_sub(fixed + self.dense.as_ref().map_or(0, |d| d.bytes())),
            &mut self.counters,
        ) else {
            return Ok(false);
        };
        self.counters.groups += plan.groups.len() as u64;
        self.counters.comparisons += plan.comparisons;
        self.counters.barriers += plan.barriers;
        for group in plan.groups {
            let phases = group.clone().map(|i| paths[i].len()).max().unwrap_or(0);
            for phase in 0..phases {
                let indices: Vec<_> = group
                    .clone()
                    .filter_map(|i| {
                        let r = &paths[i];
                        (phase < r.len()).then_some(r.start + phase)
                    })
                    .collect();
                self.counters.passes += 2;
                self.counters.coverage += 1;
                self.counters.paint += 1;
                self.counters.clear_pixels += w as u64 * h as u64;
                self.counters.resolve_pixels += w as u64 * h as u64;
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("batched vector coverage"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &coverage.multi_view,
                            resolve_target: Some(&coverage.resolved_view),
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Discard,
                            },
                        })],
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.pooled_geometry);
                    for &i in &indices {
                        let d = &draws[i];
                        let b = &self.meshes[&d.key].batches[d.batch];
                        let [x0, y0, x1, y1] = d.rect.0;
                        let (page, offset) = self.uniforms.offset(start + i);
                        pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
                        pass.set_bind_group(0, &self.uniforms.pages[page].geometry, &[offset]);
                        pass.set_vertex_buffer(0, b.vertices.slice(..));
                        pass.set_index_buffer(b.indices.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..b.count, 0, 0..1);
                        self.counters.geometry_draws += 1;
                    }
                }
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("batched vector paint"),
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
                    pass.set_pipeline(&self.pooled_paint);
                    for &i in &indices {
                        let [x0, y0, x1, y1] = draws[i].rect.0;
                        let (page, offset) = self.uniforms.offset(start + i);
                        pass.set_scissor_rect(x0, y0, x1 - x0, y1 - y0);
                        pass.set_bind_group(
                            0,
                            self.uniforms.pages[page].paint.as_ref().unwrap(),
                            &[offset],
                        );
                        pass.draw(0..3, 0..1);
                        self.counters.paint_draws += 1;
                    }
                }
            }
        }
        Ok(true)
    }
}
impl Renderer {
    pub(super) fn vector_bytes(&self) -> u64 {
        self.vector.as_ref().map_or(0, |v| v.bytes())
    }
    pub(super) fn vector_mesh_bytes(&self) -> u64 {
        self.vector.as_ref().map_or(0, |v| v.mesh_bytes())
    }
    pub(super) fn vector_fixed_bytes(&self) -> u64 {
        self.vector_bytes().saturating_sub(self.vector_mesh_bytes())
    }
}

#[cfg(test)]
mod dense_selection_tests {
    use super::dense_candidate;
    #[test]
    fn dense_selection_leaves_sparse_and_small_calls_on_the_disjoint_path() {
        assert!(!dense_candidate(100, 1));
        assert!(!dense_candidate(7, 7));
        assert!(!dense_candidate(100, 79));
        assert!(dense_candidate(8, 8));
        assert!(dense_candidate(11, 11));
        assert!(dense_candidate(100, 80));
    }
}
