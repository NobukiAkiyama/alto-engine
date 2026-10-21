//! Lyon meshes -> 4x MSAA R8 coverage -> premultiplied color, including stroke unions.
//! Text adds the glyph atlas for runs whose em is small on the display level.
use super::*;
use crate::glyph_atlas::{
    ATLAS_MAX_EM_PX, ATLAS_SIDE, AtlasStats, GlyphAtlas, max_scale, place, rasterize,
};
use alto_vector::{LyonRasterizer, StyledPath, TextRun, VectorRasterizer};
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
struct Coverage {
    multi: wgpu::Texture,
    resolved: wgpu::Texture,
    w: u32,
    h: u32,
}
pub struct VectorResources {
    geometry: wgpu::RenderPipeline,
    paint: wgpu::RenderPipeline,
    meshes: HashMap<(ContentHash, i32), CachedMesh>,
    coverage: Option<Coverage>,
    tessellator: LyonRasterizer,
    glyph: wgpu::RenderPipeline,
    atlas: Option<Atlas>,
    glyph_stats: AtlasStats,
    pub hits: u64,
    pub misses: u64,
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
            geometry: g,
            paint: p,
            meshes: HashMap::new(),
            coverage: None,
            tessellator: LyonRasterizer::default(),
            glyph,
            atlas: None,
            glyph_stats: AtlasStats::default(),
            hits: 0,
            misses: 0,
        }
    }
    pub fn mesh_bytes(&self) -> u64 {
        self.meshes.values().map(|m| m.bytes).sum()
    }
    /// Drops tessellation caches. Coverage and the glyph atlas stay; they belong to the frame.
    pub fn discard_meshes(&mut self) {
        self.meshes.clear();
    }
    pub fn bytes(&self) -> u64 {
        self.mesh_bytes()
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
        self.render(device, encoder, target, &vector, world, roi, frame, budget)?;
        let Some(atlas) = self.atlas.as_ref().filter(|_| !instances.is_empty()) else {
            return Ok(());
        };
        let uniform = buffer(
            device,
            "glyph atlas config",
            &[[w as f32, h as f32, 0.0, 0.0]],
            wgpu::BufferUsages::UNIFORM,
        );
        let glyphs = buffer(
            device,
            "glyph instances",
            &instances,
            wgpu::BufferUsages::VERTEX,
        );
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
        let mesh_limit = 32 * 1024 * 1024u64;
        // Frobenius norm bounds the maximum singular value, including shear and nonuniform scale.
        let transform_scale =
            (world[0] * world[0] + world[1] * world[1] + world[2] * world[2] + world[3] * world[3])
                .sqrt();
        if !transform_scale.is_finite() || scale <= 0.0 {
            return Err("INVALID_ARGUMENT: vector transform".into());
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
                .map_err(|e| e.to_string())?;
            let bytes = meshes.iter().map(|m| m.bytes() as u64).sum::<u64>();
            if bytes > mesh_limit || bytes + coverage_bytes > budget {
                return Err("INVALID_ARGUMENT: vector mesh exceeds GPU budget".into());
            }
            while self.meshes.values().map(|m| m.bytes).sum::<u64>() + bytes
                > mesh_limit.min(budget - coverage_bytes)
            {
                let old = self
                    .meshes
                    .iter()
                    .filter(|(_, m)| m.used != frame)
                    .min_by_key(|(_, m)| m.used)
                    .map(|(k, _)| *k)
                    .ok_or("INVALID_ARGUMENT: visible vector meshes exceed GPU budget")?;
                self.meshes.remove(&old);
            }
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
                        "vector vertices",
                        &m.vertices,
                        wgpu::BufferUsages::VERTEX,
                    ),
                    indices: buffer(
                        device,
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
        while self.meshes.values().map(|m| m.bytes).sum::<u64>() + coverage_bytes > budget {
            let old = self
                .meshes
                .iter()
                .filter(|(_, m)| m.used != frame)
                .min_by_key(|(_, m)| m.used)
                .map(|(k, _)| *k)
                .ok_or("INVALID_ARGUMENT: vector coverage exceeds GPU budget")?;
            self.meshes.remove(&old);
        }
        if self.coverage.as_ref().is_none_or(|c| c.w != w || c.h != h) {
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
            self.coverage = Some(Coverage {
                multi: make(4, wgpu::TextureUsages::RENDER_ATTACHMENT),
                resolved: make(
                    1,
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                ),
                w,
                h,
            });
        }
        let coverage = self.coverage.as_ref().ok_or("INTERNAL: vector coverage")?;
        let multi = coverage.multi.create_view(&Default::default());
        let resolved = coverage.resolved.create_view(&Default::default());
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
                let uniform = buffer(
                    device,
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
