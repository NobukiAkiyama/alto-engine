//! General M5 tile renderer. Back-propagated ROIs provide the halo for every blur stage.
//! Temporary targets are pooled and reused in queue order, including between display tiles.
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
    pipeline: wgpu::ComputePipeline,
    resolve: wgpu::ComputePipeline,
    pool: Vec<Pooled>,
    pub(super) allocations: u64,
    pub(super) reuses: u64,
}
impl Resources {
    fn new(device: &wgpu::Device) -> Result<Self> {
        Ok(Self {
            allocations: 0,
            reuses: 0,
            pipeline: compute(
                device,
                "M5 effects",
                &crate::shader_with_blend(include_str!("../../shaders/effects.wgsl")),
            ),
            resolve: compute(
                device,
                "M5 resolve",
                include_str!("../../shaders/effects-resolve.wgsl"),
            ),
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
                        format: wgpu::TextureFormat::Rgba16Float,
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
                        format: wgpu::TextureFormat::Rgba16Float,
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
        let bg = self.r.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("M5 stage"),
            layout: &self.resources.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&av),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&bv),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&cv),
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
            ],
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
        let mut p = Self::params(1, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        p.settings = [1.0, mode(node.blend), f32::from(clip.is_some()), 0.0];
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
                let mut p = Self::params(3, world);
                p.settings = [brightness, contrast, saturation, 0.0];
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
            let p = Self::params(5, transform);
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
                if node.features.mask.is_some()
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
                let p = Self::params(0, world);
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
        let mut resources = match self.m5.take() {
            Some(r) => r,
            None => Resources::new(&self.device)?,
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
}
