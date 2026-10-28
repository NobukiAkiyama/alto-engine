//! General M5 tile renderer. Back-propagated ROIs provide the halo for every blur stage.
//! Temporary targets are pooled and reused in queue order, including between display tiles.
//! Render version 3 uses `effects-q14.wgsl`: every target is premultiplied Q14 in
//! `rgba16uint` and the resolve writes `rgba8uint` bytes. The host has already
//! refused every v3 scene that needs a stage without an integer definition.
use super::*;
use std::sync::Weak;
const POOL_IDLE_FRAMES: u64 = 8;
fn bucket(n: u32) -> u32 {
    n.max(1).next_multiple_of(64)
}
use crate::{REffect, REffectKind, RMask, RNode};
use alto_vector::{FillRule, StyledPath};
#[derive(Clone, Copy, Debug)]
struct Roi {
    x: f32,
    y: f32,
    scale: f32,
    w: u32,
    h: u32,
}
impl Roi {
    fn coarsen(self, minimum: f32) -> Self {
        let scale = self
            .scale
            .max(2f32.powf(minimum.max(0.000001).log2().ceil()));
        if scale <= self.scale {
            return self;
        }
        // Anchor every ROI to the same document grid; add interpolation samples at both ends.
        let x = (self.x / scale).floor() * scale - scale;
        let y = (self.y / scale).floor() * scale - scale;
        Self {
            x,
            y,
            scale,
            w: ((self.x + self.w as f32 * self.scale - x) / scale).ceil() as u32 + 1,
            h: ((self.y + self.h as f32 * self.scale - y) / scale).ceil() as u32 + 1,
        }
    }
    fn expand(self, x: u32, y: u32) -> Self {
        Self {
            x: self.x - x as f32 * self.scale,
            y: self.y - y as f32 * self.scale,
            w: self.w + 2 * x,
            h: self.h + 2 * y,
            ..self
        }
    }
    fn uniform(self) -> [f32; 4] {
        [self.x, self.y, self.scale, 0.0]
    }
    fn region(self) -> [f32; 3] {
        [self.x, self.y, self.scale]
    }
    fn bounds(self) -> [f64; 4] {
        [
            self.x as f64,
            self.y as f64,
            (self.x + self.w as f32 * self.scale) as f64,
            (self.y + self.h as f32 * self.scale) as f64,
        ]
    }
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    out_region: [f32; 4],
    region_a: [f32; 4],
    region_b: [f32; 4],
    inv0: [f32; 4],
    inv1: [f32; 4],
    settings: [f32; 4],
    info: [u32; 4],
    extra: [f32; 4],
    curve: [f32; 64],
    sizes: [u32; 4],
    output_clip: [u32; 4],
}
/// Versions 1 and 2 keep f16 targets. Version 3 stores premultiplied Q14 integers.
fn target_format(exact: bool) -> wgpu::TextureFormat {
    if exact {
        wgpu::TextureFormat::Rgba16Uint
    } else {
        wgpu::TextureFormat::Rgba16Float
    }
}
/// Q14 integers in the f32 bit patterns of a stage uniform.
fn q14_settings(values: [i32; 4]) -> [f32; 4] {
    values.map(|v| f32::from_bits(v as u32))
}
#[derive(Clone)]
struct Target {
    index: usize,
    roi: Roi,
    _lease: Arc<()>,
}
struct Pooled {
    texture: wgpu::Texture,
    w: u32,
    h: u32,
    lease: Weak<()>,
    used: u64,
}
pub(super) struct Resources {
    /// Render version 3: Q14 `rgba16uint` targets and the integer stages.
    exact: bool,
    pipeline: wgpu::ComputePipeline,
    resolve: wgpu::ComputePipeline,
    pool: Vec<Pooled>,
    pub(super) allocations: u64,
    pub(super) reuses: u64,
}
impl Resources {
    fn new(device: &wgpu::Device, exact: bool) -> Result<Self> {
        let (pipeline, resolve) = if exact {
            (
                compute(
                    device,
                    "M5 integer stages",
                    &crate::integer_effects_shader(),
                ),
                compute(
                    device,
                    "M5 integer resolve",
                    include_str!("../../shaders/effects-resolve-q14.wgsl"),
                ),
            )
        } else {
            (
                compute(
                    device,
                    "M5 effects",
                    &crate::shader_with_blend(include_str!("../../shaders/effects.wgsl")),
                ),
                compute(
                    device,
                    "M5 resolve",
                    include_str!("../../shaders/effects-resolve.wgsl"),
                ),
            )
        };
        Ok(Self {
            exact,
            allocations: 0,
            reuses: 0,
            pipeline,
            resolve,
            pool: vec![Pooled {
                texture: memory::create_texture(
                    device,
                    &wgpu::TextureDescriptor {
                        label: Some("M5 transparent input"),
                        size: wgpu::Extent3d {
                            width: 1,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: target_format(exact),
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::STORAGE_BINDING,
                        view_formats: &[],
                    },
                    Class::Effects,
                )?,
                w: 1,
                h: 1,
                lease: Weak::new(),
                used: 0,
            }],
        })
    }
    pub(super) fn exact(&self) -> bool {
        self.exact
    }
    pub(super) fn expire(&mut self, frame: u64, retired: &mut Retired) {
        let mut kept = Vec::with_capacity(self.pool.len());
        for (i, p) in self.pool.drain(..).enumerate() {
            if i != 0
                && p.lease.strong_count() == 0
                && frame.saturating_sub(p.used) >= POOL_IDLE_FRAMES
            {
                retired.effect_texture(p.texture);
            } else {
                kept.push(p);
            }
        }
        self.pool = kept;
    }
    /// Hands every target to `retired`, for when these resources are discarded.
    pub(super) fn retire(self, retired: &mut super::Retired) {
        for p in self.pool {
            retired.effect_texture(p.texture);
        }
    }
}
#[derive(Clone, Copy)]
enum StackInput<'a> {
    Node {
        index: usize,
        world: [f64; 6],
        depth: usize,
    },
    Siblings {
        children: &'a [usize],
        world: [f64; 6],
        depth: usize,
    },
}
struct Context<'a> {
    r: &'a mut Renderer,
    resources: &'a mut Resources,
    scene: &'a RenderScene,
    store: &'a dyn TileStore,
    encoder: wgpu::CommandEncoder,
    pinned: HashSet<u32>,
    previews: HashMap<ContentHash, wgpu::Texture>,
    overrides: BTreeMap<(NodeId, TileKey), ContentHash>,
    mask_preview: bool,
}
impl Context<'_> {
    fn dummy(&self) -> Target {
        Target {
            index: 0,
            _lease: Arc::new(()),
            roi: Roi {
                x: 0.0,
                y: 0.0,
                scale: 1.0,
                w: 1,
                h: 1,
            },
        }
    }
    fn zero(&mut self, roi: Roi) -> Result<Target> {
        let out = self.alloc(roi)?;
        let d = self.dummy();
        self.dispatch(
            &out,
            &d,
            &d,
            &d,
            Self::params(6, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
            &[],
        )?;
        Ok(out)
    }
    fn crop(&mut self, input: Target, roi: Roi) -> Result<Target> {
        if input.roi.x == roi.x
            && input.roi.y == roi.y
            && input.roi.w == roi.w
            && input.roi.h == roi.h
            && input.roi.scale == roi.scale
        {
            return Ok(input);
        }
        let out = self.alloc(roi)?;
        self.dispatch(
            &out,
            &input,
            &input,
            &input,
            Self::params(8, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
            &[],
        )?;
        Ok(out)
    }
    fn alloc(&mut self, roi: Roi) -> Result<Target> {
        if roi.w == 0
            || roi.h == 0
            || roi.w > self.r.device.limits().max_texture_dimension_2d
            || roi.h > self.r.device.limits().max_texture_dimension_2d
        {
            return Err(RenderError::InvalidArgument(
                "effect ROI exceeds texture limits".into(),
            ));
        }
        let w = bucket(roi.w);
        let h = bucket(roi.h);
        if w > self.r.device.limits().max_texture_dimension_2d
            || h > self.r.device.limits().max_texture_dimension_2d
        {
            return Err(RenderError::InvalidArgument(
                "effect bucket exceeds texture limits".into(),
            ));
        }
        let lease = Arc::new(());
        // Weak leases track every retained ancestor and clipping backdrop. A slot is
        // reused only after its last CPU owner has encoded its last read.
        let reuse = self
            .resources
            .pool
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, p)| p.w == w && p.h == h && p.lease.strong_count() == 0)
            .map(|(i, _)| i);
        let index = if let Some(index) = reuse {
            self.resources.reuses += 1;
            index
        } else {
            let need = w as u64 * h as u64 * 8;
            let used = self.r.gpu_bytes().budget_used;
            if used + need > self.r.budget_bytes {
                // No premature destroy: unsubmitted targets are retained until the
                // encoder is submitted or discarded, and still count in the ledger.
                return Err(RenderError::Budget(
                    "effects exceed GPU memory budget (including retired allocations)".into(),
                ));
            }
            self.resources.allocations += 1;
            self.resources.pool.push(Pooled {
                texture: memory::create_texture(
                    &self.r.device,
                    &wgpu::TextureDescriptor {
                        label: Some("M5 bucket target"),
                        size: wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: target_format(self.resources.exact),
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::STORAGE_BINDING
                            | wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    },
                    Class::Effects,
                )?,
                w,
                h,
                lease: Weak::new(),
                used: self.r.frame,
            });
            self.resources.pool.len() - 1
        };
        let pooled = &mut self.resources.pool[index];
        pooled.lease = Arc::downgrade(&lease);
        pooled.used = self.r.frame;
        let view = pooled.texture.create_view(&Default::default());
        // This GPU clear follows all old reads in this encoder. Queue writes here
        // would run ahead of those reads and corrupt the old owner.
        drop(self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear whole M5 bucket"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        }));
        Ok(Target {
            index,
            roi,
            _lease: lease,
        })
    }

    fn dispatch(
        &mut self,
        output: &Target,
        a: &Target,
        b: &Target,
        clip: &Target,
        mut params: Params,
        entries: &[Entry],
    ) -> Result<()> {
        let additional = (entries.len().max(1) * std::mem::size_of::<Entry>()
            + std::mem::size_of::<Params>()) as u64;
        if self.r.gpu_bytes().budget_used + additional > self.r.budget_bytes {
            return Err(RenderError::Budget(
                "effect command buffers exceed GPU memory budget".into(),
            ));
        }
        params.out_region = output.roi.uniform();
        params.region_a = a.roi.uniform();
        params.region_b = b.roi.uniform();
        params.info[2] = entries.len() as u32;
        params.sizes = [a.roi.w, a.roi.h, b.roi.w, b.roi.h];
        params.output_clip = [output.roi.w, output.roi.h, clip.roi.w, clip.roi.h];
        debug_assert!(
            output.index != a.index && output.index != b.index && output.index != clip.index
        );
        let av = self.resources.pool[a.index]
            .texture
            .create_view(&Default::default());
        let bv = self.resources.pool[b.index]
            .texture
            .create_view(&Default::default());
        let cv = self.resources.pool[clip.index]
            .texture
            .create_view(&Default::default());
        let ov = self.resources.pool[output.index]
            .texture
            .create_view(&Default::default());
        let tiles = buffer(
            &self.r.device,
            &self.r.queue,
            "M5 sparse tiles",
            entries,
            wgpu::BufferUsages::STORAGE,
        )?;
        let uniform = buffer(
            &self.r.device,
            &self.r.queue,
            "M5 ROI",
            &[params],
            wgpu::BufferUsages::UNIFORM,
        )?;
        self.r
            .retired
            .classified_buffer(&tiles, MemoryClass::Effects);
        self.r
            .retired
            .classified_buffer(&uniform, MemoryClass::Effects);
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&av),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&bv),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&self.r.atlas_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&ov),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: tiles.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: uniform.as_entire_binding(),
            },
        ];
        // The integer stages never read the clip base (clipping is refused in
        // version 3), so the derived layout has no binding 2.
        if !self.resources.exact {
            entries.push(wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&cv),
            });
        }
        let bg = self.r.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("M5 stage"),
            layout: &self.resources.pipeline.get_bind_group_layout(0),
            entries: &entries,
        });
        let mut pass = self.encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&self.resources.pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(output.roi.w.div_ceil(16), output.roi.h.div_ceil(16), 1);
        Ok(())
    }
    fn entries(
        &mut self,
        map: &[crate::RenderTile],
        world: [f64; 6],
        roi: Roi,
        layer: Option<NodeId>,
    ) -> Result<Vec<Entry>> {
        let Some(inv) = inverse(world) else {
            return Ok(vec![]);
        };
        let b = roi.bounds();
        let corners =
            [[b[0], b[1]], [b[2], b[1]], [b[0], b[3]], [b[2], b[3]]].map(|p| point(inv, p));
        let minx = corners.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
        let maxx = corners
            .iter()
            .map(|p| p[0])
            .fold(f64::NEG_INFINITY, f64::max);
        let miny = corners.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
        let maxy = corners
            .iter()
            .map(|p| p[1])
            .fold(f64::NEG_INFINITY, f64::max);
        let mut map: BTreeMap<_, _> = map.iter().map(|t| (t.key, t.hash)).collect();
        if let Some(layer) = layer {
            for ((l, key), hash) in &self.overrides {
                if *l == layer {
                    map.insert(*key, *hash);
                }
            }
        }
        let mut entries = vec![];
        for (key, hash) in map {
            if key.tx as f64 * 256.0 >= maxx
                || (key.tx + 1) as f64 * 256.0 <= minx
                || key.ty as f64 * 256.0 >= maxy
                || (key.ty + 1) as f64 * 256.0 <= miny
            {
                continue;
            }
            let slot = self.r.upload(
                hash,
                self.store,
                &mut self.pinned,
                &mut self.encoder,
                &self.previews,
            )?;
            entries.push(Entry {
                xy: [key.tx, key.ty],
                slot,
                pad: 0,
            });
        }
        Ok(entries)
    }
    fn params(mode: u32, world: [f64; 6]) -> Params {
        let inv = inverse(world).unwrap_or([0.0; 6]);
        Params {
            inv0: [inv[0] as f32, inv[1] as f32, inv[2] as f32, inv[3] as f32],
            inv1: [inv[4] as f32, inv[5] as f32, 0.0, 0.0],
            info: [mode, 0, 0, 0],
            ..Params::zeroed()
        }
    }
    /// A version 3 mask stage (modes 21 to 23) writing `out` from inputs `a` and
    /// `b`. Every input texel is placed from the integer pixel index
    /// (`input_map`), and the mask, if any, is read at `out` through
    /// `mask_sampler`: the Q16 lookup only. An input region or a mask without an
    /// integer definition is refused (DQ3-4), never sampled in f32. `settings`
    /// are Q14 integers. All of it is in `Config.grid` (`mask_grid`).
    fn mask_params(
        mode: u32,
        transform: Option<[f64; 6]>,
        out: Roi,
        a: Roi,
        b: Roi,
        settings: [i32; 4],
    ) -> Result<Params> {
        let sampler = transform
            .map(|t| {
                super::mask_sampler(t, [out.x, out.y], out.scale).ok_or_else(|| {
                    RenderError::GpuUnsupported(
                        "v3 mask transform or level has no integer lookup".into(),
                    )
                })
            })
            .transpose()?;
        let map = |input: Roi| {
            super::input_map(out.region(), [out.w, out.h], input.region()).ok_or_else(|| {
                RenderError::GpuUnsupported(
                    "v3 mask stage input region has no integer texel map".into(),
                )
            })
        };
        Ok(Params {
            curve: super::mask_grid(settings, map(a)?, map(b)?, sampler.as_ref()),
            info: [mode, 0, 0, crate::sample::SAMPLE_INTEGER],
            ..Params::zeroed()
        })
    }
    fn mask_entries(
        &mut self,
        mask: &RMask,
        world: [f64; 6],
        roi: Roi,
        layer: Option<NodeId>,
    ) -> Result<(Vec<Entry>, [f64; 6])> {
        let transform = if mask.linked {
            multiply(world, mask.transform)
        } else {
            mask.transform
        };
        Ok((self.entries(&mask.tiles, transform, roi, layer)?, transform))
    }
    fn vector(
        &mut self,
        content: VectorContent,
        world: [f64; 6],
        roi: Roi,
        batch: bool,
    ) -> Result<Target> {
        if self.resources.exact {
            return Err(RenderError::Internal(
                "v3 vector content reached the M5 renderer".into(),
            ));
        }
        let out = self.alloc(roi)?;
        let target = self.resources.pool[out.index].texture.clone();
        let mut resources = self.r.vector.take().unwrap_or_else(|| {
            super::vector::VectorResources::new(&self.r.device, wgpu::TextureFormat::Rgba16Float)
        });
        let usage = self.r.gpu_bytes();
        let budget = self
            .r
            .budget_bytes
            .saturating_sub(usage.budget_used - usage.vector);
        let roi = [
            roi.x as f64,
            roi.y as f64,
            roi.scale as f64,
            roi.w as f64,
            roi.h as f64,
        ];
        let result = match content {
            VectorContent::Paths(paths) if batch => resources.render(
                &self.r.device,
                &self.r.queue,
                &mut self.encoder,
                &target,
                &paths.iter().collect::<Vec<_>>(),
                world,
                roi,
                self.r.frame,
                budget,
            ),
            VectorContent::Paths(paths) => resources.render_legacy(
                &self.r.device,
                &self.r.queue,
                &mut self.encoder,
                &target,
                &paths.iter().collect::<Vec<_>>(),
                world,
                roi,
                self.r.frame,
                budget,
            ),
            VectorContent::Text(runs) => resources.render_text(
                &self.r.device,
                &self.r.queue,
                &mut self.encoder,
                &target,
                runs,
                world,
                roi,
                self.r.frame,
                budget,
                self.r.glyph_atlas,
            ),
        };
        self.r.retired.take(&mut resources.retired);
        self.r.vector = Some(resources);
        result?;
        Ok(out)
    }
    fn path_coverage(
        &mut self,
        path: &alto_vector::PathData,
        rule: FillRule,
        world: [f64; 6],
        roi: Roi,
        feather: f32,
    ) -> Result<Target> {
        let sample = roi.coarsen(feather / 64.0);
        let effect = REffect {
            kind: REffectKind::GaussianBlur { radius: feather },
            selection: None,
        };
        let input = Self::input_roi(std::slice::from_ref(&effect), sample);
        let coverage = self.vector(
            VectorContent::Paths(&[StyledPath {
                data: path.clone(),
                fill: Some([1.0; 4]),
                stroke: None,
                fill_rule: rule,
            }]),
            world,
            input,
            false,
        )?;
        let output = self.effects(coverage, &[effect], sample, world)?;
        self.crop(output, roi)
    }
    fn vector_coverage(
        &mut self,
        node: &RNode,
        world: [f64; 6],
        roi: Roi,
    ) -> Result<Option<Target>> {
        let mut result = None;
        if let Some(mask) = &node.features.vector_mask {
            result = Some(self.path_coverage(
                &mask.path,
                mask.fill_rule,
                multiply(world, mask.transform.0),
                roi,
                mask.feather,
            )?);
        }
        if let Some(path) = &node.features.clip_path {
            let coverage = self.path_coverage(
                &path.path,
                path.fill_rule,
                multiply(world, path.transform.0),
                roi,
                0.0,
            )?;
            if let Some(mask) = result {
                let out = self.alloc(roi)?;
                self.dispatch(&out, &mask, &coverage, &mask, Self::params(11, world), &[])?;
                result = Some(out);
            } else {
                result = Some(coverage);
            }
        }
        Ok(result)
    }
    fn apply_props(&mut self, source: Target, node: &RNode, world: [f64; 6]) -> Result<Target> {
        // Version 3 has no vector masks or clip paths (`v3_gap`). Without a pixel
        // mask, `blend` applies the opacity. With one, mode 21 applies the mask and
        // the opacity once, as one Q28 factor, and `blend` uses opacity 1.
        if self.resources.exact {
            let Some(mask) = &node.features.mask else {
                return Ok(source);
            };
            let out = self.alloc(source.roi)?;
            let (entries, transform) = self.mask_entries(
                mask,
                world,
                source.roi,
                if self.mask_preview {
                    Some(node.key)
                } else {
                    None
                },
            )?;
            let p = Self::mask_params(
                21,
                Some(transform),
                source.roi,
                source.roi,
                source.roi,
                [crate::composite_opacity_q14(node.opacity), 1, 0, 0],
            )?;
            self.dispatch(&out, &source, &source, &source, p, &entries)?;
            return Ok(out);
        }
        if node.features.mask.is_none()
            && node.features.vector_mask.is_none()
            && node.features.clip_path.is_none()
            && node.opacity == 1.0
        {
            return Ok(source);
        }
        let out = self.alloc(source.roi)?;
        let mut p = Self::params(4, world);
        p.settings = [node.opacity, 0.0, 0.0, 0.0];
        let mut entries = vec![];
        if let Some(mask) = &node.features.mask {
            let (e, transform) = self.mask_entries(
                mask,
                world,
                source.roi,
                if self.mask_preview {
                    Some(node.key)
                } else {
                    None
                },
            )?;
            entries = e;
            p = Self::params(4, transform);
            p.settings = [node.opacity, 1.0, 0.0, 0.0];
        }
        self.dispatch(&out, &source, &source, &source, p, &entries)?;
        if let Some(mask) = self.vector_coverage(node, world, source.roi)? {
            let masked = self.alloc(source.roi)?;
            self.dispatch(&masked, &out, &mask, &out, Self::params(11, world), &[])?;
            Ok(masked)
        } else {
            Ok(out)
        }
    }
    fn blend(
        &mut self,
        dst: Target,
        src: Target,
        node: &RNode,
        clip: Option<Target>,
    ) -> Result<Target> {
        let out = self.alloc(dst.roi)?;
        let mut p;
        if self.resources.exact {
            // Version 3 mode 19: the Q14 layer composite with the host-quantized
            // opacity. Clipping is refused. A masked node's opacity was consumed
            // by `apply_props` (mode 21), so it composites at opacity 1.
            p = Self::params(19, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            let opacity = if node.features.mask.is_some() {
                1.0
            } else {
                node.opacity
            };
            p.settings = q14_settings([
                crate::composite_opacity_q14(opacity),
                node.blend as i32,
                0,
                0,
            ]);
        } else {
            p = Self::params(1, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            p.settings = [1.0, mode(node.blend), f32::from(clip.is_some()), 0.0];
        }
        self.dispatch(&out, &dst, &src, clip.as_ref().unwrap_or(&dst), p, &[])?;
        Ok(out)
    }
    fn clip_blend(&mut self, base: Target, source: Target, node: &RNode) -> Result<Target> {
        let out = self.alloc(base.roi)?;
        let mut p = Self::params(9, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        p.settings[1] = mode(node.blend);
        self.dispatch(&out, &base, &source, &base, p, &[])?;
        Ok(out)
    }
    fn stage(
        &mut self,
        input: Target,
        effect: &REffect,
        roi: Roi,
        world: [f64; 6],
    ) -> Result<Target> {
        // A selection on a color-adjust is mode 22 below.
        if self.resources.exact && !matches!(effect.kind, REffectKind::ColorAdjust { .. }) {
            return Err(RenderError::Internal(
                "v3 effect without an integer stage reached the M5 renderer".into(),
            ));
        }
        let filtered = match effect.kind {
            REffectKind::GaussianBlur { radius } => {
                let r = (radius / roi.scale).ceil() as u32;
                if r == 0 {
                    input.clone()
                } else {
                    let middle = self.alloc(roi.expand(0, r))?;
                    let mut p = Self::params(2, world);
                    p.settings = [(radius / roi.scale / 3.0).max(0.333333), 0.0, 0.0, r as f32];
                    self.dispatch(&middle, &input, &input, &input, p, &[])?;
                    let out = self.alloc(roi)?;
                    p.info[3] = 1;
                    self.dispatch(&out, &middle, &middle, &middle, p, &[])?;
                    out
                }
            }
            REffectKind::ColorAdjust {
                brightness,
                contrast,
                saturation,
            } => {
                let out = self.alloc(roi)?;
                // Version 1 and 2 keep the float formula in mode 3. Version 3
                // uses the Q14 formula in mode 17. The uniform carries the Q14
                // integers in the f32 bit patterns.
                let exact = self.resources.exact;
                let mut p = Self::params(if exact { 17 } else { 3 }, world);
                p.settings = if exact {
                    q14_settings([
                        super::color_adjust_q14(brightness),
                        super::color_adjust_q14(contrast),
                        super::color_adjust_q14(saturation),
                        0,
                    ])
                } else {
                    [brightness, contrast, saturation, 0.0]
                };
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
            REffectKind::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
            } => {
                let out = self.alloc(roi)?;
                let mut p = Self::params(13, world);
                p.settings = [in_black, in_white, gamma, out_black];
                p.extra[0] = out_white;
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
            REffectKind::HueSaturation {
                hue,
                saturation,
                lightness,
            } => {
                let out = self.alloc(roi)?;
                let mut p = Self::params(15, world);
                p.settings = [hue, saturation, lightness, 0.0];
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
            REffectKind::Invert => {
                let out = self.alloc(roi)?;
                let p = Self::params(14, world);
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
            REffectKind::Curves { ref points } => {
                let out = self.alloc(roi)?;
                let mut p = Self::params(16, world);
                for (i, (x, y)) in points.iter().take(16).enumerate() {
                    p.curve[i * 4] = *x;
                    p.curve[i * 4 + 1] = *y;
                }
                p.extra[0] = points.len().min(16) as f32;
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
        };
        if let Some(mask) = &effect.selection {
            let out = self.alloc(roi)?;
            let (entries, transform) = self.mask_entries(mask, world, roi, None)?;
            let p = if self.resources.exact {
                Self::mask_params(22, Some(transform), roi, input.roi, filtered.roi, [0; 4])?
            } else {
                Self::params(5, transform)
            };
            self.dispatch(&out, &input, &filtered, &input, p, &entries)?;
            Ok(out)
        } else {
            Ok(filtered)
        }
    }
    fn effects(
        &mut self,
        input: Target,
        effects: &[REffect],
        desired: Roi,
        world: [f64; 6],
    ) -> Result<Target> {
        let mut outputs = vec![desired; effects.len()];
        let mut roi = desired;
        for (i, e) in effects.iter().enumerate().rev() {
            outputs[i] = roi;
            if let REffectKind::GaussianBlur { radius } = e.kind {
                let r = (radius / roi.scale).ceil() as u32;
                roi = roi.expand(r, r);
            }
        }
        let mut source = input;
        for (e, roi) in effects.iter().zip(outputs) {
            source = self.stage(source, e, roi, world)?;
        }
        Ok(source)
    }
    fn stack(
        &mut self,
        input: StackInput<'_>,
        effects: &[REffect],
        desired: Roi,
        world: [f64; 6],
    ) -> Result<Target> {
        let Some((effect, previous)) = effects.split_last() else {
            return match input {
                StackInput::Node {
                    index,
                    world,
                    depth,
                } => self.raw_node(index, desired, world, depth),
                StackInput::Siblings {
                    children,
                    world,
                    depth,
                } => {
                    let blank = self.zero(desired)?;
                    self.children(children, blank, world, depth)
                }
            };
        };
        if let REffectKind::GaussianBlur { radius } = effect.kind {
            let sample = desired.coarsen(radius / 64.0);
            let source = self.stack(
                input,
                previous,
                Self::input_roi(std::slice::from_ref(effect), sample),
                world,
            )?;
            let mut plain = effect.clone();
            plain.selection = None;
            let filtered = self.stage(source, &plain, sample, world)?;
            let filtered = self.crop(filtered, desired)?;
            if let Some(mask) = &effect.selection {
                let original = self.stack(input, previous, desired, world)?;
                let out = self.alloc(desired)?;
                let (entries, transform) = self.mask_entries(mask, world, desired, None)?;
                self.dispatch(
                    &out,
                    &original,
                    &filtered,
                    &original,
                    Self::params(5, transform),
                    &entries,
                )?;
                Ok(out)
            } else {
                Ok(filtered)
            }
        } else {
            let source = self.stack(input, previous, desired, world)?;
            self.stage(source, effect, desired, world)
        }
    }
    fn input_roi(effects: &[REffect], mut roi: Roi) -> Roi {
        for e in effects.iter().rev() {
            if let REffectKind::GaussianBlur { radius } = e.kind {
                let r = (radius / roi.scale).ceil() as u32;
                roi = roi.expand(r, r);
            }
        }
        roi
    }
    fn children(
        &mut self,
        children: &[usize],
        mut accum: Target,
        world: [f64; 6],
        depth: usize,
    ) -> Result<Target> {
        let mut base = None;
        let mut base_background = None;
        let mut base_node: Option<RNode> = None;
        let mut base_position = 0;
        for (position, &index) in children.iter().enumerate() {
            let node = self.scene.nodes[index].clone();
            if !node.visible {
                if !node.features.clip {
                    base = None;
                }
                continue;
            }
            let transform = multiply(world, node.transform);
            if let RContent::Adjustment(effect) = &node.content {
                if self.resources.exact {
                    v3_adjustment_blend(node.features.mask.is_some(), node.blend)?;
                }
                let mut stack = vec![effect.clone()];
                stack.extend(node.features.effects.clone());
                // A backdrop blur needs extra input from the same siblings. The caller renders that halo below.
                let input = if node.features.clip {
                    let Some(base) = base else {
                        continue;
                    };
                    base
                } else {
                    accum
                };
                let desired = input.roi;
                let filtered = if stack
                    .iter()
                    .any(|e| matches!(e.kind,REffectKind::GaussianBlur {radius} if radius>0.0))
                {
                    let prefix = if node.features.clip {
                        &children[base_position..position]
                    } else {
                        &children[..position]
                    };
                    self.stack(
                        StackInput::Siblings {
                            children: prefix,
                            world,
                            depth: depth + 1,
                        },
                        &stack,
                        desired,
                        transform,
                    )?
                } else {
                    self.effects(input.clone(), &stack, desired, transform)?
                };
                let mut mixed = filtered.clone();
                if self.resources.exact {
                    // Version 3 adjustment layers are unclipped (`v3_gap`). A pixel
                    // mask takes mode 23 (mask, opacity and blend in one Q28 mix).
                    // Otherwise only the opacity mixes, in Q14 (mode 20).
                    if node.features.mask.is_some() {
                        let out = self.alloc(desired)?;
                        let (entries, mask_transform, has_mask) = match &node.features.mask {
                            Some(mask) => {
                                let (e, t) = self.mask_entries(
                                    mask,
                                    transform,
                                    desired,
                                    if self.mask_preview {
                                        Some(node.key)
                                    } else {
                                        None
                                    },
                                )?;
                                (e, Some(t), 1)
                            }
                            None => (vec![], None, 0),
                        };
                        let p = Self::mask_params(
                            23,
                            mask_transform,
                            desired,
                            input.roi,
                            filtered.roi,
                            [
                                crate::composite_opacity_q14(node.opacity),
                                has_mask,
                                node.blend as i32,
                                0,
                            ],
                        )?;
                        self.dispatch(&out, &input, &filtered, &input, p, &entries)?;
                        mixed = out;
                    } else if node.opacity != 1.0 {
                        let out = self.alloc(desired)?;
                        let mut p = Self::params(20, transform);
                        p.settings =
                            q14_settings([crate::composite_opacity_q14(node.opacity), 0, 0, 0]);
                        self.dispatch(&out, &input, &filtered, &input, p, &[])?;
                        mixed = out;
                    }
                } else if node.features.mask.is_some()
                    || node.opacity != 1.0
                    || node.features.clip
                    || node.blend != RenderBlend::Normal
                {
                    let out = self.alloc(desired)?;
                    let mut p = Self::params(7, transform);
                    p.settings = [node.opacity, 0.0, f32::from(node.features.clip), 0.0];
                    let mut entries = vec![];
                    if let Some(mask) = &node.features.mask {
                        let (e, t) = self.mask_entries(
                            mask,
                            transform,
                            desired,
                            if self.mask_preview {
                                Some(node.key)
                            } else {
                                None
                            },
                        )?;
                        entries = e;
                        p = Self::params(7, t);
                        p.settings = [node.opacity, 1.0, f32::from(node.features.clip), 0.0];
                    }
                    p.settings[2] = 0.0;
                    p.settings[3] = mode(node.blend);
                    self.dispatch(&out, &input, &filtered, &input, p, &entries)?;
                    mixed = out;
                }
                if let Some(mask) = self.vector_coverage(&node, transform, desired)? {
                    let out = self.alloc(desired)?;
                    self.dispatch(
                        &out,
                        &input,
                        &mixed,
                        &mask,
                        Self::params(12, transform),
                        &[],
                    )?;
                    mixed = out;
                }
                if node.features.clip {
                    let out = self.alloc(desired)?;
                    self.dispatch(
                        &out,
                        &input,
                        &mixed,
                        &input,
                        Self::params(10, transform),
                        &[],
                    )?;
                    base = Some(out.clone());
                    accum = self.blend(
                        base_background
                            .clone()
                            .ok_or(RenderError::Internal("clipping backdrop".into()))?,
                        out,
                        base_node
                            .as_ref()
                            .ok_or(RenderError::Internal("clipping base".into()))?,
                        None,
                    )?;
                } else {
                    accum = mixed;
                    base = None;
                    base_node = None;
                    base_background = None;
                }
                continue;
            }
            let pass = matches!(
                &node.content,
                RContent::Group {
                    isolated: false,
                    ..
                }
            ) && node.blend == RenderBlend::Normal
                && node.opacity == 1.0
                && node.features.mask.is_none()
                && node.features.vector_mask.is_none()
                && node.features.clip_path.is_none()
                && node.features.effects.is_empty()
                && !node.features.clip
                && !children
                    .get(position + 1)
                    .is_some_and(|i| self.scene.nodes[*i].features.clip);
            if pass
                && let RContent::Group { children, .. } = &node.content
                && !children
                    .iter()
                    .any(|i| matches!(self.scene.nodes[*i].content, RContent::Adjustment(_)))
            {
                accum = self.children(children, accum, transform, depth + 1)?;
                base = None;
                continue;
            }
            let src = self.node(index, accum.roi, world, depth + 1)?;
            if node.features.clip {
                if let Some(current) = base {
                    let clipped = self.clip_blend(current, src, &node)?;
                    base = Some(clipped.clone());
                    accum = self.blend(
                        base_background
                            .clone()
                            .ok_or(RenderError::Internal("clipping backdrop".into()))?,
                        clipped,
                        base_node
                            .as_ref()
                            .ok_or(RenderError::Internal("clipping base".into()))?,
                        None,
                    )?;
                }
            } else {
                base_position = position;
                base_background = Some(accum.clone());
                base_node = Some(node.clone());
                base = Some(src.clone());
                accum = self.blend(accum, src, &node, None)?;
            }
        }
        Ok(accum)
    }
    fn node(
        &mut self,
        index: usize,
        desired: Roi,
        parent: [f64; 6],
        depth: usize,
    ) -> Result<Target> {
        if depth > alto_core::MAX_TREE_DEPTH as usize {
            return Err(RenderError::InvalidArgument("effect graph too deep".into()));
        }
        let node = self.scene.nodes[index].clone();
        let world = multiply(parent, node.transform);
        let source = self.stack(
            StackInput::Node {
                index,
                world,
                depth,
            },
            &node.features.effects,
            desired,
            world,
        )?;
        self.apply_props(source, &node, world)
    }
    fn raw_node(
        &mut self,
        index: usize,
        roi: Roi,
        world: [f64; 6],
        depth: usize,
    ) -> Result<Target> {
        let node = self.scene.nodes[index].clone();
        let source = match &node.content {
            RContent::Tiles(tiles) => {
                let out = self.alloc(roi)?;
                let entries = self.entries(
                    tiles,
                    world,
                    roi,
                    if self.mask_preview {
                        None
                    } else {
                        Some(node.key)
                    },
                )?;
                // Version 3 premultiplies recovered unorm bytes into Q14. Version 1
                // and 2 keep the float product in mode 0.
                let p = Self::params(if self.resources.exact { 18 } else { 0 }, world);
                let dummy = self.dummy();
                self.dispatch(&out, &dummy, &dummy, &dummy, p, &entries)?;
                out
            }
            RContent::Mesh(paths) => self.vector(VectorContent::Paths(paths), world, roi, true)?,
            RContent::Text(runs) => self.vector(VectorContent::Text(runs), world, roi, false)?,
            RContent::Group { children, .. } => {
                let blank = self.zero(roi)?;
                self.children(children, blank, world, depth)?
            }
            _ => self.zero(roi)?,
        };
        Ok(source)
    }
}
enum VectorContent<'a> {
    Paths(&'a [StyledPath]),
    Text(&'a [alto_vector::TextRun]),
}
impl Renderer {
    pub(super) fn compose_m5(
        &mut self,
        scene: &RenderScene,
        key: CacheKey,
        slot: u32,
        store: &dyn TileStore,
    ) -> Result<()> {
        let exact = scene.render_version >= alto_core::RENDER_VERSION_EXACT;
        let mut resources = match self.m5.take() {
            Some(r) if r.exact() == exact => r,
            other => {
                if let Some(old) = other {
                    old.retire(&mut self.retired);
                }
                Resources::new(&self.device, exact)?
            }
        };
        resources.expire(self.frame, &mut self.retired);
        let mut previews = HashMap::new();
        let mut overrides = BTreeMap::new();
        if let Some(stroke) = &self.stroke {
            for (k, t) in &stroke.tiles {
                let hash = ContentHash::of(
                    format!(
                        "preview:{:?}:{},{}:{}",
                        stroke.layer, k.tx, k.ty, stroke.epoch
                    )
                    .as_bytes(),
                );
                previews.insert(hash, t.output.clone());
                overrides.insert((stroke.layer, *k), hash);
            }
        }
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("M5 display tile"),
            });
        let result = (|| {
            let mask_preview = self.stroke.as_ref().is_some_and(|s| s.mask);
            let mut cx = Context {
                r: self,
                resources: &mut resources,
                scene,
                store,
                encoder,
                pinned: HashSet::new(),
                previews,
                overrides,
                mask_preview,
            };
            let scale = 2f32.powi(key.0);
            let roi = Roi {
                x: key.1 as f32 * 256.0 * scale,
                y: key.2 as f32 * 256.0 * scale,
                scale,
                w: 256,
                h: 256,
            };
            let source = cx.node(scene.root, roi, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0)?;
            let view = cx.resources.pool[source.index]
                .texture
                .create_view(&Default::default());
            let uniform = buffer(
                &cx.r.device,
                &cx.r.queue,
                "M5 resolve config",
                &[Config {
                    origin_scale: [roi.x, roi.y, roi.scale, scene.canvas.x1 as f32],
                    info: [slot, scene.canvas.y1 as u32, 0, 0],
                }],
                wgpu::BufferUsages::UNIFORM,
            )?;
            cx.r.retired
                .classified_buffer(&uniform, MemoryClass::Effects);
            let bg = cx.r.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("M5 resolve"),
                layout: &cx.resources.resolve.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&cx.r.cache_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniform.as_entire_binding(),
                    },
                ],
            });
            {
                let mut pass = cx.encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&cx.resources.resolve);
                pass.set_bind_group(0, &bg, &[]);
                pass.dispatch_workgroups(16, 16, 1);
            }
            cx.r.queue.submit(Some(cx.encoder.finish()));
            cx.r.release_retired();
            if let Some(vector) = &mut cx.r.vector {
                vector.after_submit();
            }
            Ok(())
        })();
        if result.is_err() {
            self.queue.submit(std::iter::empty());
            if let Some(vector) = &mut self.vector {
                self.retired.take(&mut vector.retired);
                vector.after_submit();
            }
            self.release_retired();
        }
        self.m5 = Some(resources);
        result
    }
}

/// Version 3 refuses a non-normal adjustment-layer blend without a pixel mask
/// (`v3_gap`) before any stage runs: its inputs would come through stages that
/// still take f32 coordinates. It is never drawn in float (DQ3-4).
fn v3_adjustment_blend(masked: bool, blend: RenderBlend) -> Result<()> {
    if masked || blend == RenderBlend::Normal {
        return Ok(());
    }
    Err(RenderError::GpuUnsupported(
        "v3 adjustment layer blend needs a pixel mask".into(),
    ))
}
#[cfg(test)]
mod pool_tests {
    use super::*;
    #[test]
    fn buckets_bound_padding_and_leases_hold_ancestors() {
        assert_eq!(bucket(256), 256);
        assert_eq!(bucket(280), 320);
        assert_eq!(bucket(1), 64);
        let owner = Arc::new(());
        let lease = Arc::downgrade(&owner);
        let ancestor = owner.clone();
        drop(owner);
        assert_eq!(lease.strong_count(), 1);
        drop(ancestor);
        assert_eq!(lease.strong_count(), 0);
    }
    #[test]
    fn shader_bounds_every_logical_input_and_output() {
        let s = include_str!("../../shaders/effects.wgsl");
        assert!(s.contains("vec2i(c.sizes.xy)"));
        assert!(s.contains("vec2i(c.sizes.zw)"));
        assert!(s.contains("id.xy>=c.output_clip.xy"));
        assert!(s.contains("vec2i(c.output_clip.zw)"));
        assert!(!s.contains("textureDimensions("));
    }
    fn roi(x: f32, y: f32, scale: f32) -> Roi {
        Roi {
            x,
            y,
            scale,
            w: 256,
            h: 256,
        }
    }
    #[test]
    fn v3_unmasked_adjustment_blends_are_refused() {
        for blend in [
            RenderBlend::Multiply,
            RenderBlend::Screen,
            RenderBlend::Overlay,
        ] {
            assert!(matches!(
                v3_adjustment_blend(false, blend),
                Err(RenderError::GpuUnsupported(_))
            ));
            assert!(v3_adjustment_blend(true, blend).is_ok());
        }
        assert!(v3_adjustment_blend(false, RenderBlend::Normal).is_ok());
        assert!(v3_adjustment_blend(true, RenderBlend::Normal).is_ok());
    }
    /// Every input region and mask a v3 mask stage cannot place in integers is
    /// refused with `GpuUnsupported` (DQ3-4); equal regions need no integers.
    #[test]
    fn v3_mask_stages_refuse_regions_without_an_integer_map() {
        let map = super::super::input_map;
        // Equal regions are the pixel index itself, fractional or not.
        assert_eq!(
            map([0.5, 3.25, 0.5], [256, 256], [0.5, 3.25, 0.5]),
            Some([0; 4])
        );
        // 2 (out - in) + out_scale, 2 out_scale, 2 in_scale.
        assert_eq!(
            map([-300.0, 37.0, 1.0], [256, 256], [-295.0, 34.0, 1.0]),
            Some([-9, 7, 2, 2])
        );
        assert_eq!(
            map([-256.0, -512.0, 2.0], [256, 256], [-260.0, -520.0, 4.0]),
            Some([10, 18, 4, 8])
        );
        for (out, input) in [
            ([0.5, 0.0, 1.0], [0.0, 0.0, 1.0]),
            ([0.0, 0.0, 1.0], [0.0, -0.25, 1.0]),
            ([0.0, 0.0, 0.5], [0.0, 0.0, 1.0]),
            ([0.0, 0.0, 1.0], [0.0, 0.0, 0.5]),
            // A numerator past i32.
            ([1_073_741_824.0, 0.0, 1.0], [-1_073_741_824.0, 0.0, 1.0]),
            // The step times the last pixel past i32.
            ([0.0, 0.0, 16_777_216.0], [0.0, 0.0, 1.0]),
        ] {
            assert_eq!(map(out, [256, 256], input), None, "{out:?} from {input:?}");
        }
        let unsupported = |r: Result<Params>| matches!(r, Err(RenderError::GpuUnsupported(_)));
        let out = roi(-300.0, 37.0, 1.0);
        for mode in [21, 22, 23] {
            // Either input off the integer grid.
            assert!(unsupported(Context::mask_params(
                mode,
                None,
                out,
                roi(-299.5, 37.0, 1.0),
                out,
                [16384, 0, 0, 0]
            )));
            assert!(unsupported(Context::mask_params(
                mode,
                None,
                out,
                out,
                roi(-300.0, 37.0, 0.5),
                [16384, 0, 0, 0]
            )));
            // A mask at a level below 1 or a fractional origin.
            for at in [roi(0.0, 0.0, 0.5), roi(0.5, 0.0, 1.0)] {
                assert!(unsupported(Context::mask_params(
                    mode,
                    Some([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
                    at,
                    at,
                    at,
                    [16384, 1, 0, 0]
                )));
            }
        }
        // An unmasked mode 23 at a fractional level with equal regions draws.
        let zoomed = roi(64.0, 32.0, 0.5);
        let p = Context::mask_params(23, None, zoomed, zoomed, zoomed, [8192, 0, 2, 0]).unwrap();
        assert_eq!(
            p.curve[0..12]
                .iter()
                .map(|v| v.to_bits() as i32)
                .collect::<Vec<_>>(),
            [8192, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert!(p.out_region == [0.0; 4] && p.inv0 == [0.0; 4] && p.extra == [0.0; 4]);
    }
    /// Removes `//` comments.
    fn strip_comments(src: &str) -> String {
        src.lines()
            .map(|l| l.split("//").next().unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }
    /// Every `fn` of a WGSL source and its body, braces included.
    fn wgsl_functions(src: &str) -> HashMap<String, String> {
        let mut out = HashMap::new();
        let mut rest = src;
        while let Some(at) = rest.find("fn ") {
            let boundary = at == 0 || !rest.as_bytes()[at - 1].is_ascii_alphanumeric();
            let after = &rest[at + 3..];
            let name: String = after
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let Some(open) = after.find('{') else {
                break;
            };
            if !boundary || name.is_empty() {
                rest = after;
                continue;
            }
            let mut depth = 0;
            let mut end = open;
            for (i, ch) in after[open..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            out.insert(name, after[open..end].to_string());
            rest = &after[end..];
        }
        out
    }
    /// The v3 mask stages (modes 21 to 23) reach none of the f32 coordinate
    /// helpers (`load_a`, `load_b`, `sparse_bytes`) and read no float region,
    /// position or uniform: everything they call is integer, except
    /// `unorm_byte`, which turns the `Rgba8Unorm` atlas `.r` back into its byte.
    #[test]
    fn v3_mask_stages_call_no_f32_coordinate_helper() {
        let shader = strip_comments(&crate::integer_effects_shader());
        let functions = wgsl_functions(&shader);
        for helper in ["load_a", "load_b", "sparse_bytes", "mask_stage", "main"] {
            assert!(functions.contains_key(helper), "{helper} not found");
        }
        let mut reached = vec!["mask_stage".to_string()];
        let mut i = 0;
        while i < reached.len() {
            let body = &functions[&reached[i]];
            for name in functions.keys() {
                if !reached.contains(name)
                    && body.match_indices(&format!("{name}(")).any(|(at, _)| {
                        at == 0 || {
                            let c = body.as_bytes()[at - 1];
                            !(c.is_ascii_alphanumeric() || c == b'_')
                        }
                    })
                {
                    reached.push(name.clone());
                }
            }
            i += 1;
        }
        for needed in [
            "texel_a",
            "texel_b",
            "input_texel",
            "floor_div",
            "mask_q",
            "mask_byte",
            "mask_texel",
            "pixel_q8",
            "coord_q8",
            "div28",
            "unorm_byte",
        ] {
            assert!(
                reached.iter().any(|r| r == needed),
                "mask_stage does not reach {needed}"
            );
        }
        for helper in ["load_a", "load_b", "sparse_bytes", "main"] {
            assert!(
                !reached.iter().any(|r| r == helper),
                "mask_stage reaches {helper}"
            );
        }
        let float = [
            "f32",
            "vec2f",
            "vec3f",
            "vec4f",
            "floor(",
            "fract(",
            "round(",
            "c.out_region",
            "c.region_a",
            "c.region_b",
            "c.inv0",
            "c.inv1",
            "c.extra",
            "c.settings",
        ];
        for name in reached.iter().filter(|n| *n != "unorm_byte") {
            for token in float {
                assert!(
                    !functions[name].contains(token),
                    "{name} (reached from mask_stage) uses {token}"
                );
            }
        }
        // `main` sends modes 21 to 23 to `mask_stage` before it forms `p`.
        let main = &functions["main"];
        let branch = main.find("c.info.x>=21u&&c.info.x<=23u").unwrap();
        let call = main.find("mask_stage(vec2i(id.xy))").unwrap();
        let p = main.find("let p=").unwrap();
        assert!(branch < call && call < p);
        assert!(main[branch..p].contains("return;"));
        assert!(!main[branch..p].contains("load_"));
        // The integer uniform is declared integer.
        assert!(shader.contains("grid: array<vec4i, 16>"));
    }
}
