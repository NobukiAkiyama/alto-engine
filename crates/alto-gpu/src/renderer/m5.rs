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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RoiQ {
    level: i32,
    ox: i64,
    oy: i64,
    w: u32,
    h: u32,
}
impl RoiQ {
    fn from_roi(roi: Roi) -> Result<Self> {
        let [ox, oy, level] =
            crate::raster_q8::roi_grid(roi.x, roi.y, roi.scale).ok_or_else(|| {
                RenderError::GpuUnsupported("v3 ROI is not on the integer grid".into())
            })?;
        let level = i32::try_from(level)
            .map_err(|_| RenderError::InvalidArgument("v3 ROI level".into()))?;
        if !(-6..=12).contains(&level) {
            return Err(RenderError::InvalidArgument(
                "v3 ROI level out of range".into(),
            ));
        }
        Ok(Self {
            level,
            ox,
            oy,
            w: roi.w,
            h: roi.h,
        })
    }
    fn to_roi(self) -> Result<Roi> {
        fn coordinate(value: i64, level: i32) -> Option<f32> {
            if value == 0 {
                return Some(0.0);
            }
            let magnitude = value.unsigned_abs();
            let bit = 63 - magnitude.leading_zeros();
            let exponent = i32::try_from(bit).ok()? + level + 127;
            if !(1..255).contains(&exponent) {
                return None;
            }
            let significant = if bit > 23 {
                let shift = bit - 23;
                if magnitude & ((1u64 << shift) - 1) != 0 {
                    return None;
                }
                magnitude >> shift
            } else {
                magnitude << (23 - bit)
            };
            Some(f32::from_bits(
                (u32::from(value < 0) << 31)
                    | ((u32::try_from(exponent).ok()?) << 23)
                    | (u32::try_from(significant).ok()? & 0x7f_ffff),
            ))
        }
        Ok(Roi {
            x: coordinate(self.ox, self.level)
                .ok_or_else(|| RenderError::GpuUnsupported("v3 ROI x is not f32-exact".into()))?,
            y: coordinate(self.oy, self.level)
                .ok_or_else(|| RenderError::GpuUnsupported("v3 ROI y is not f32-exact".into()))?,
            scale: f32::from_bits(((self.level + 127) as u32) << 23),
            w: self.w,
            h: self.h,
        })
    }
    fn expand(self, x: u32, y: u32) -> Result<Self> {
        Ok(Self {
            ox: self
                .ox
                .checked_sub(i64::from(x))
                .ok_or_else(|| RenderError::InvalidArgument("v3 ROI x overflow".into()))?,
            oy: self
                .oy
                .checked_sub(i64::from(y))
                .ok_or_else(|| RenderError::InvalidArgument("v3 ROI y overflow".into()))?,
            w: self
                .w
                .checked_add(2 * x)
                .ok_or_else(|| RenderError::InvalidArgument("v3 ROI width overflow".into()))?,
            h: self
                .h
                .checked_add(2 * y)
                .ok_or_else(|| RenderError::InvalidArgument("v3 ROI height overflow".into()))?,
            ..self
        })
    }
    /// v3 blur works at levels -6..=12 only; checked before any shift.
    fn check_level(level: i32) -> Result<()> {
        if (-6..=12).contains(&level) {
            Ok(())
        } else {
            Err(RenderError::InvalidArgument(
                "v3 blur level out of range".into(),
            ))
        }
    }
    fn coarsen(self, target_level: i32) -> Result<Self> {
        Self::check_level(self.level)?;
        let shift = target_level
            .checked_sub(self.level)
            .and_then(|m| u32::try_from(m).ok())
            .ok_or_else(|| RenderError::InvalidArgument("v3 coarsen level".into()))?;
        if shift > 7 {
            return Err(RenderError::InvalidArgument(
                "v3 coarsen ratio exceeds 128".into(),
            ));
        }
        if shift == 0 {
            return Ok(self);
        }
        let factor = 1i64 << shift;
        let axis = |origin: i64, span: u32| -> Result<(i64, u32)> {
            let next = origin
                .div_euclid(factor)
                .checked_sub(1)
                .ok_or_else(|| RenderError::InvalidArgument("v3 coarsen origin".into()))?;
            let end = origin
                .checked_add(i64::from(span))
                .ok_or_else(|| RenderError::InvalidArgument("v3 coarsen end".into()))?;
            let width = end
                .checked_sub(
                    next.checked_mul(factor)
                        .ok_or_else(|| RenderError::InvalidArgument("v3 coarsen product".into()))?,
                )
                .ok_or_else(|| RenderError::InvalidArgument("v3 coarsen width".into()))?
                .div_euclid(factor)
                + i64::from(end.rem_euclid(factor) != 0)
                + 1;
            Ok((
                next,
                u32::try_from(width)
                    .map_err(|_| RenderError::InvalidArgument("v3 coarsen span".into()))?,
            ))
        };
        let (ox, w) = axis(self.ox, self.w)?;
        let (oy, h) = axis(self.oy, self.h)?;
        Ok(Self {
            level: target_level,
            ox,
            oy,
            w,
            h,
        })
    }
    fn taps(self, radius_q8: u32) -> Result<(u32, u32)> {
        Self::check_level(self.level)?;
        if radius_q8 > 16384 {
            return Err(RenderError::InvalidArgument(
                "v3 blur radius out of range".into(),
            ));
        }
        let (radius, rho8) = if self.level >= 0 {
            let factor = 1u32 << self.level;
            let divisor = 256u32 << self.level;
            (
                radius_q8.div_ceil(divisor),
                (radius_q8 + factor / 2) / factor,
            )
        } else {
            let shift = -self.level;
            let scaled = radius_q8 << shift;
            (scaled.div_ceil(256), scaled)
        };
        if radius > 64 || rho8 > 16384 {
            return Err(RenderError::InvalidArgument(
                "v3 blur exceeds 64 taps".into(),
            ));
        }
        Ok((radius, rho8))
    }
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
/// The uniform of the version 3 resolve (`effects-resolve-q14.wgsl`): the
/// on-canvas pixel rectangle (`resolve_span`) and the cache slot.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct IntegerResolve {
    span: [u32; 4],
    info: [u32; 4],
}
/// One v3 glyph instance of `glyph-q14.wgsl`: its crop's ROI box `[x, y, w, h]`,
/// the crop's top-left in the scratch, and its straight Q14 colour.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlyphQ14 {
    dst: [i32; 4],
    src: [u32; 4],
    color: [u32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GlyphQ14Params {
    count: u32,
    w: u32,
    h: u32,
    pad: u32,
}
/// Glyph instances per `glyph-q14.wgsl` dispatch (memo `GLYPH_Q14_CHUNK`).
const GLYPH_Q14_CHUNK: usize = 1024;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VectorQ14Params {
    color: [u32; 4],
    size: [u32; 4],
}
/// Verify the stored f32 ROI against the tile key before any version 3 stage.
fn integer_origin(key: CacheKey, roi: Roi) -> Result<RoiQ> {
    let grid = RoiQ::from_roi(roi)?;
    if grid.level != key.0 || grid.ox != i64::from(key.1) * 256 || grid.oy != i64::from(key.2) * 256
    {
        return Err(RenderError::GpuUnsupported(
            "v3 M5 tile has no integer origin".into(),
        ));
    }
    Ok(grid)
}
fn resolve_span_q(grid: RoiQ, canvas: [i32; 2]) -> [u32; 4] {
    if grid.level >= 0 {
        let scale = 1i64 << grid.level;
        return super::resolve_span(
            [grid.ox * scale, grid.oy * scale],
            scale,
            canvas.map(i64::from),
        );
    }
    let factor = 1i128 << -grid.level;
    let axis = |origin: i64, end: i32| {
        let start = (-i128::from(origin)).clamp(0, 256) as u32;
        let stop = (i128::from(end) * factor - i128::from(origin)).clamp(0, 256) as u32;
        [start.min(stop), stop]
    };
    let x = axis(grid.ox, canvas[0]);
    let y = axis(grid.oy, canvas[1]);
    [x[0], y[0], x[1], y[1]]
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
    vector_q14: Option<wgpu::ComputePipeline>,
    glyph_q14: Option<wgpu::ComputePipeline>,
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
            vector_q14: exact.then(|| {
                compute(
                    device,
                    "M5 integer vector paint",
                    &format!(
                        "{}\n{}",
                        include_str!("../../shaders/q14.wgsl"),
                        include_str!("../../shaders/vector-q14.wgsl")
                    ),
                )
            }),
            glyph_q14: exact.then(|| {
                compute(
                    device,
                    "M5 integer glyph paint",
                    &format!(
                        "{}\n{}",
                        include_str!("../../shaders/q14.wgsl"),
                        include_str!("../../shaders/glyph-q14.wgsl")
                    ),
                )
            }),
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
        world: WorldTransform,
        depth: usize,
    },
    Siblings {
        children: &'a [usize],
        world: WorldTransform,
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
        // Version 3 has no mode 6 and writes zero for it; it reads nothing.
        let p = if self.resources.exact {
            Self::q14_params(6, roi, [None, None], [0; 4], None)?
        } else {
            Self::params(6, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
        };
        self.dispatch(&out, &d, &d, &d, p, &[])?;
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
        let p =
            if self.resources.exact {
                let destination = RoiQ::from_roi(roi)?;
                let source = RoiQ::from_roi(input.roi)?;
                let m = source.level - destination.level;
                if !(0..=7).contains(&m) {
                    return Err(RenderError::InvalidArgument(
                        "v3 resample level ratio".into(),
                    ));
                }
                let factor = 1i64 << m;
                let dx = destination
                    .ox
                    .checked_sub(source.ox.checked_mul(factor).ok_or_else(|| {
                        RenderError::InvalidArgument("v3 resample x product".into())
                    })?)
                    .and_then(|v| i32::try_from(v).ok())
                    .ok_or_else(|| RenderError::InvalidArgument("v3 resample x offset".into()))?;
                let dy = destination
                    .oy
                    .checked_sub(source.oy.checked_mul(factor).ok_or_else(|| {
                        RenderError::InvalidArgument("v3 resample y product".into())
                    })?)
                    .and_then(|v| i32::try_from(v).ok())
                    .ok_or_else(|| RenderError::InvalidArgument("v3 resample y offset".into()))?;
                Params {
                    curve: super::mask_grid([dx, dy, m, 0], [0; 4], [0; 4], None),
                    info: [25, 0, 0, 0],
                    ..Params::zeroed()
                }
            } else {
                Self::params(8, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
            };
        self.dispatch(&out, &input, &input, &input, p, &[])?;
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
        // Version 3 reads no float slot (`effects-q14.wgsl` `Config.unused`): its
        // regions are in `grid` (`q14_params`), and these stay zero.
        if !self.resources.exact {
            params.out_region = output.roi.uniform();
            params.region_a = a.roi.uniform();
            params.region_b = b.roi.uniform();
        }
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
        entries.push(wgpu::BindGroupEntry {
            binding: 2,
            resource: wgpu::BindingResource::TextureView(&cv),
        });
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
        self.upload_entries(map, layer, |key| {
            !(key.tx as f64 * 256.0 >= maxx
                || (key.tx + 1) as f64 * 256.0 <= minx
                || key.ty as f64 * 256.0 >= maxy
                || (key.ty + 1) as f64 * 256.0 <= miny)
        })
    }
    /// Uploads the tiles of `map` (with the stroke overrides of `layer`) that
    /// `keep` selects, in key order.
    fn upload_entries(
        &mut self,
        map: &[crate::RenderTile],
        layer: Option<NodeId>,
        keep: impl Fn(TileKey) -> bool,
    ) -> Result<Vec<Entry>> {
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
            if !keep(key) {
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
    /// The uniform of a version 3 stage (`effects-q14.wgsl`) writing `out`.
    /// `inputs` are the ROIs of the inputs the mode reads (`a`, `b`); each texel
    /// is placed from the integer pixel index (`input_map`). `lookup` is the atlas
    /// read of mode 18 (the raster) or modes 21 to 23 (the mask), the Q16
    /// `mask_sampler`. `settings` are Q14 integers. All of it is in `Config.grid`
    /// (`mask_grid`); the float slots stay zero. An input region without an
    /// integer definition is refused (DQ3-4), never placed in f32.
    fn q14_params(
        mode: u32,
        out: Roi,
        inputs: [Option<Roi>; 2],
        settings: [i32; 4],
        lookup: Option<&super::MaskSampler>,
    ) -> Result<Params> {
        let map = |input: Option<Roi>| match input {
            None => Ok([0; 4]),
            Some(input) => super::input_map(out.region(), [out.w, out.h], input.region())
                .ok_or_else(|| {
                    RenderError::GpuUnsupported(
                        "v3 stage input region has no integer texel map".into(),
                    )
                }),
        };
        Ok(Params {
            curve: super::mask_grid(settings, map(inputs[0])?, map(inputs[1])?, lookup),
            info: [mode, 0, 0, crate::sample::SAMPLE_INTEGER],
            ..Params::zeroed()
        })
    }
    /// A version 3 mask stage (modes 21 to 23) writing `out` from inputs `a` and
    /// `b`, with the mask, if any, read at `out` through `mask_sampler`: the Q16
    /// lookup only. A mask without an integer definition is refused (DQ3-4),
    /// never sampled in f32. See `q14_params`.
    fn mask_params(
        mode: u32,
        transform: Option<WorldTransform>,
        out: Roi,
        a: Roi,
        b: Roi,
        settings: [i32; 4],
    ) -> Result<Params> {
        let sampler = transform
            .map(|t| {
                let fixed = t.fixed()?;
                super::fixed_mask_sampler(&fixed, Self::integer_roi(out)?).ok_or_else(|| {
                    RenderError::GpuUnsupported(
                        "v3 mask transform or level has no integer lookup".into(),
                    )
                })
            })
            .transpose()?;
        Self::q14_params(mode, out, [Some(a), Some(b)], settings, sampler.as_ref())
    }
    fn blur_params(
        out: Roi,
        input: Roi,
        radius: u32,
        weights: &[u32],
        axis: u32,
    ) -> Result<Params> {
        let output = RoiQ::from_roi(out)?;
        let source = RoiQ::from_roi(input)?;
        if output.level != source.level {
            return Err(RenderError::InvalidArgument(
                "v3 blur input level mismatch".into(),
            ));
        }
        let dx = i32::try_from(output.ox - source.ox)
            .map_err(|_| RenderError::InvalidArgument("v3 blur x offset".into()))?;
        let dy = i32::try_from(output.oy - source.oy)
            .map_err(|_| RenderError::InvalidArgument("v3 blur y offset".into()))?;
        let mut params = Params {
            curve: super::mask_grid(
                [i32::try_from(radius).unwrap(), dx, dy, 0],
                [0; 4],
                [0; 4],
                None,
            ),
            info: [24, 0, 0, axis],
            ..Params::zeroed()
        };
        let words: &mut [u32] = bytemuck::cast_slice_mut(&mut params.curve);
        words[4..37].fill(0);
        for (index, &weight) in weights.iter().enumerate() {
            let word = 4 + index / 2;
            words[word] |= weight << ((index % 2) * 16);
        }
        Ok(params)
    }
    fn integer_roi(roi: Roi) -> Result<[i32; 3]> {
        let invalid = || {
            RenderError::GpuUnsupported(format!(
                "v3 ROI has no integer origin or scale: ({}, {}, {})",
                roi.x, roi.y, roi.scale
            ))
        };
        let grid = RoiQ::from_roi(roi)?;
        if grid.level >= 0 {
            Ok([
                super::exact_i32(roi.x).ok_or_else(invalid)?,
                super::exact_i32(roi.y).ok_or_else(invalid)?,
                super::exact_i32(roi.scale).ok_or_else(invalid)?,
            ])
        } else {
            let denominator = 1_i32
                .checked_shl(u32::try_from(-grid.level).map_err(|_| invalid())?)
                .ok_or_else(invalid)?;
            Ok([
                i32::try_from(grid.ox).map_err(|_| invalid())?,
                i32::try_from(grid.oy).map_err(|_| invalid())?,
                -denominator,
            ])
        }
    }
    /// Version 3: the sampler of `tiles` drawn with document-to-pixel `transform`
    /// at `roi`, and the tiles it can read, uploaded. The tiles are selected with
    /// the sampler's own Q16 inverse and Q8 pixel centers (`v3_reach`), not with
    /// the f64 inverse: the two differ by the quantization error, which grows
    /// with the distance from the origin. A source that no pixel reaches uploads
    /// nothing. Refused (`GpuUnsupported`, naming `what`) when the lookup has no
    /// Q16 value or pixels that reach a tile have no exact shader value (DQ3-4).
    fn lookup_entries(
        &mut self,
        tiles: &[crate::RenderTile],
        transform: WorldTransform,
        roi: Roi,
        layer: Option<NodeId>,
        what: &str,
    ) -> Result<(Vec<Entry>, super::MaskSampler)> {
        let unsupported =
            || RenderError::GpuUnsupported(format!("v3 {what} ROI has no integer tile range"));
        let sampler = super::fixed_mask_sampler(&transform.fixed()?, Self::integer_roi(roi)?)
            .ok_or_else(unsupported)?;
        // The source's tiles, the stroke overrides of `layer` included.
        let keys = tiles.iter().map(|t| t.key).chain(
            self.overrides
                .keys()
                .filter(|(l, _)| Some(*l) == layer)
                .map(|(_, k)| *k),
        );
        let [x, y, scale] = sampler.origin.map(i64::from);
        let span = [0, 0, i64::from(roi.w), i64::from(roi.h)];
        let inverse = super::sampler_inverse(&sampler);
        let reach = if scale > 0 {
            super::v3_reach(&inverse, [x, y], scale, span, 0, keys)
        } else {
            let step = i128::from(256 / -scale);
            super::v3_reach_q8(
                &inverse,
                [i128::from(x) * step, i128::from(y) * step],
                step,
                span,
                0,
                keys,
            )
        }
        .map_err(|()| unsupported())?;
        // Nothing reached: no tile is read (every pixel is transparent or byte 0).
        let Some(range) = reach else {
            return Ok((vec![], sampler));
        };
        let entries = self.upload_entries(tiles, layer, |key| {
            (range[0]..=range[2]).contains(&key.tx) && (range[1]..=range[3]).contains(&key.ty)
        })?;
        Ok((entries, sampler))
    }
    fn mask_entries(
        &mut self,
        mask: &RMask,
        world: WorldTransform,
        roi: Roi,
        layer: Option<NodeId>,
    ) -> Result<(Vec<Entry>, WorldTransform)> {
        let transform = if mask.linked {
            world.concat(mask.transform)?
        } else {
            WorldTransform::root(self.scene.render_version).concat(mask.transform)?
        };
        if !self.resources.exact {
            return Ok((
                self.entries(&mask.tiles, transform.legacy()?, roi, layer)?,
                transform,
            ));
        }
        // Version 3 reads the mask through `mask_sampler` (modes 21 to 23).
        let (entries, _) = self.lookup_entries(&mask.tiles, transform, roi, layer, "mask")?;
        Ok((entries, transform))
    }
    fn vector_q14(
        &mut self,
        paths: &[StyledPath],
        world: alto_core::FixedAffine,
        roi: Roi,
    ) -> Result<Target> {
        let [ox, oy, level] = crate::raster_q8::roi_grid(roi.x, roi.y, roi.scale)
            .ok_or_else(|| RenderError::GpuUnsupported("v3 vector ROI is not integer".into()))?;
        let level = i32::try_from(level)
            .ok()
            .filter(|l| (-6..=12).contains(l))
            .ok_or_else(|| RenderError::InvalidArgument("v3 vector level out of range".into()))?;
        // VECTOR_TRANSFORM_RANGE: a world that puts a point of the node outside
        // the Q8 grid's integer range makes the whole node transparent (not the
        // tile refused), like TEXT_TRANSFORM_RANGE. It depends on the node and
        // level only, so every tile agrees.
        let mut mapped = Vec::with_capacity(paths.len());
        for path in paths {
            match crate::raster_q8::map_path(&path.data, world, level) {
                Some(commands) => mapped.push(commands),
                None => return self.zero(roi),
            }
        }
        let mut current = self.zero(roi)?;
        for (path, commands) in paths.iter().zip(mapped) {
            // Fill precedes stroke for every styled path, including when one
            // paint has no visible alpha or exceeds the curve range.
            for paint in 0..2 {
                let paint_color = if paint == 0 {
                    path.fill
                } else {
                    path.stroke
                        .as_ref()
                        .filter(|stroke| crate::raster_q8::stroke_visible(stroke))
                        .map(|stroke| stroke.color)
                };
                let Some(paint_color) = paint_color else {
                    continue;
                };
                let color = crate::raster_q8::color_q14(paint_color).ok_or_else(|| {
                    RenderError::GpuUnsupported("v3 vector color out of range".into())
                })?;
                if color[3] == 0 {
                    continue;
                }
                let window = [ox, oy, i64::from(roi.w), i64::from(roi.h)];
                let coverage = if paint == 0 {
                    let rule = match path.fill_rule {
                        FillRule::NonZero => crate::raster_q8::FillRule::NonZero,
                        FillRule::EvenOdd => crate::raster_q8::FillRule::EvenOdd,
                    };
                    crate::raster_q8::rasterize(
                        &commands,
                        rule,
                        window,
                        crate::raster_q8::CURVE_LIMIT,
                    )
                } else {
                    crate::stroke_q8::rasterize_stroke(
                        &path.data,
                        path.stroke.as_ref().expect("visible stroke"),
                        world,
                        level,
                        window,
                    )
                };
                // A path whose integer subdivision budget is exceeded is
                // skipped identically on every tile.
                let coverage = match coverage {
                    Ok(coverage) => coverage,
                    Err(crate::raster_q8::RasterError::CurveRange) => continue,
                    Err(crate::raster_q8::RasterError::Range) => {
                        return Err(RenderError::GpuUnsupported(
                            "v3 vector window out of range".into(),
                        ));
                    }
                };
                if coverage.iter().all(|&v| v == 0) {
                    continue;
                }
                let need = u64::from(roi.w) * u64::from(roi.h) + 64;
                if self.r.gpu_bytes().budget_used.saturating_add(need) > self.r.budget_bytes {
                    return Err(RenderError::Budget(
                        "v3 vector coverage exceeds GPU budget".into(),
                    ));
                }
                // Every fallible step comes before the coverage texture, so an
                // error never drops a registered texture without destroying it.
                let output = self.alloc(roi)?;
                if self.resources.vector_q14.is_none() {
                    return Err(RenderError::Internal(
                        "v3 vector pipeline was not created".into(),
                    ));
                }
                let params = buffer(
                    &self.r.device,
                    &self.r.queue,
                    "v3 vector paint params",
                    &[VectorQ14Params {
                        color: color.map(u32::from),
                        size: [roi.w, roi.h, 0, 0],
                    }],
                    wgpu::BufferUsages::UNIFORM,
                )?;
                let created = memory::create_texture(
                    &self.r.device,
                    &wgpu::TextureDescriptor {
                        label: Some("v3 vector coverage"),
                        size: wgpu::Extent3d {
                            width: roi.w,
                            height: roi.h,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::R8Unorm,
                        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                    Class::Effects,
                );
                let texture = match created {
                    Ok(texture) => texture,
                    Err(error) => {
                        memory::destroy_buffer(&params, true);
                        return Err(error);
                    }
                };
                self.r.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &coverage,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(roi.w),
                        rows_per_image: Some(roi.h),
                    },
                    wgpu::Extent3d {
                        width: roi.w,
                        height: roi.h,
                        depth_or_array_layers: 1,
                    },
                );
                let pipeline = self.resources.vector_q14.as_ref().expect("checked above");
                let input = self.resources.pool[current.index]
                    .texture
                    .create_view(&Default::default());
                let mask = texture.create_view(&Default::default());
                let target = self.resources.pool[output.index]
                    .texture
                    .create_view(&Default::default());
                let group = self.r.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("v3 vector paint"),
                    layout: &pipeline.get_bind_group_layout(0),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&input),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&mask),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&target),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: params.as_entire_binding(),
                        },
                    ],
                });
                {
                    let mut pass = self.encoder.begin_compute_pass(&Default::default());
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, &group, &[]);
                    pass.dispatch_workgroups(roi.w.div_ceil(16), roi.h.div_ceil(16), 1);
                }
                self.r.retired.effect_texture(texture);
                self.r
                    .retired
                    .classified_buffer(&params, MemoryClass::Effects);
                current = output;
            }
        }
        Ok(current)
    }

    /// Render version 3 text from its bake only: every glyph's coverage
    /// rasterized on the CPU and cropped to the ROI (`glyph_q8::glyph_bitmaps`),
    /// shelf-packed into fresh `R8Unorm` scratch textures, and painted in order
    /// by `glyph-q14.wgsl` in chunks that each read the previous chunk's Q14.
    /// There is no persistent atlas, so cache state never changes a pixel. An
    /// unbaked node is transparent (`TEXT_UNBAKED`).
    fn text_q14(
        &mut self,
        bake: Option<&alto_scene::RTextBake>,
        world: alto_core::FixedAffine,
        roi: Roi,
    ) -> Result<Target> {
        let grid = RoiQ::from_roi(roi)?;
        let mut current = self.zero(roi)?;
        let Some(bake) = bake else {
            return Ok(current);
        };
        let crops = crate::glyph_q8::glyph_bitmaps(
            &bake.bytes,
            world,
            grid.level,
            [grid.ox, grid.oy, i64::from(roi.w), i64::from(roi.h)],
        );
        let (sw, sh) = (bucket(roi.w), bucket(roi.h));
        let mut next = 0;
        while next < crops.len() {
            // Pack as many crops as fit (each is at most the ROI) into one scratch.
            let mut packer = crate::glyph_atlas::GlyphAtlas::new(sw, sh);
            packer.begin(1);
            let mut batch = vec![];
            while next < crops.len() && batch.len() < GLYPH_Q14_CHUNK {
                let crop = &crops[next];
                let key = crate::glyph_atlas::GlyphKey {
                    raster: 1,
                    outline_hash: [0; 32],
                    outline: next as u64,
                    matrix: [0; 4],
                    phase: [0; 2],
                };
                let Some(rect) = packer.insert(key, crop.w, crop.h) else {
                    break;
                };
                batch.push((crop, rect));
                next += 1;
            }
            if batch.is_empty() {
                return Err(RenderError::Internal(
                    "v3 glyph crop larger than its scratch".into(),
                ));
            }
            let need = u64::from(sw) * u64::from(sh) + 48 * batch.len() as u64 + 16;
            if self.r.gpu_bytes().budget_used.saturating_add(need) > self.r.budget_bytes {
                return Err(RenderError::Budget(
                    "v3 glyph scratch exceeds GPU budget".into(),
                ));
            }
            let instances: Vec<GlyphQ14> = batch
                .iter()
                .map(|(crop, rect)| GlyphQ14 {
                    dst: [
                        crop.dst[0] as i32,
                        crop.dst[1] as i32,
                        crop.w as i32,
                        crop.h as i32,
                    ],
                    src: [rect.x, rect.y, 0, 0],
                    color: crop.color.map(u32::from),
                })
                .collect();
            // Every fallible step comes before the scratch texture, and the
            // buffers made before a failure are destroyed: an error never drops
            // a registered resource without releasing its budget.
            let output = self.alloc(roi)?;
            if self.resources.glyph_q14.is_none() {
                return Err(RenderError::Internal(
                    "v3 glyph pipeline was not created".into(),
                ));
            }
            let glyphs = buffer(
                &self.r.device,
                &self.r.queue,
                "glyph q14 instances",
                &instances,
                wgpu::BufferUsages::STORAGE,
            )?;
            let params = match buffer(
                &self.r.device,
                &self.r.queue,
                "glyph q14 params",
                &[GlyphQ14Params {
                    count: instances.len() as u32,
                    w: roi.w,
                    h: roi.h,
                    pad: 0,
                }],
                wgpu::BufferUsages::UNIFORM,
            ) {
                Ok(params) => params,
                Err(error) => {
                    memory::destroy_buffer(&glyphs, true);
                    return Err(error);
                }
            };
            let created = memory::create_texture(
                &self.r.device,
                &wgpu::TextureDescriptor {
                    label: Some("v3 glyph scratch"),
                    size: wgpu::Extent3d {
                        width: sw,
                        height: sh,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                Class::Vector,
            );
            let scratch = match created {
                Ok(scratch) => scratch,
                Err(error) => {
                    memory::destroy_buffer(&glyphs, true);
                    memory::destroy_buffer(&params, true);
                    return Err(error);
                }
            };
            for (crop, rect) in &batch {
                self.r.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &scratch,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: rect.x,
                            y: rect.y,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &crop.bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(crop.w),
                        rows_per_image: Some(crop.h),
                    },
                    wgpu::Extent3d {
                        width: crop.w,
                        height: crop.h,
                        depth_or_array_layers: 1,
                    },
                );
            }
            let pipeline = self.resources.glyph_q14.as_ref().expect("checked above");
            let scratch_view = scratch.create_view(&Default::default());
            let previous = self.resources.pool[current.index]
                .texture
                .create_view(&Default::default());
            let target = self.resources.pool[output.index]
                .texture
                .create_view(&Default::default());
            let group = self.r.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("v3 glyph paint"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: glyphs.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&scratch_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&previous),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&target),
                    },
                ],
            });
            {
                let mut pass = self.encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &group, &[]);
                pass.dispatch_workgroups(roi.w.div_ceil(8), roi.h.div_ceil(8), 1);
            }
            self.r.retired.effect_texture(scratch);
            self.r
                .retired
                .classified_buffer(&glyphs, MemoryClass::Vector);
            self.r
                .retired
                .classified_buffer(&params, MemoryClass::Vector);
            current = output;
        }
        Ok(current)
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
            self.r.pipelines_built = true;
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
            kind: REffectKind::GaussianBlur {
                radius: feather,
                radius_q8: alto_core::blur::blur_radius_q8_f32(feather)
                    .expect("validated vector feather"),
            },
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
        let output = self.effects(coverage, &[effect], sample, WorldTransform::Legacy(world))?;
        self.crop(output, roi)
    }
    fn path_coverage_q14(
        &mut self,
        path: &alto_vector::PathData,
        rule: FillRule,
        world: alto_core::FixedAffine,
        roi: Roi,
        feather: f32,
    ) -> Result<Target> {
        let radius_q8 = alto_core::blur::blur_radius_q8_f32(feather)
            .ok_or_else(|| RenderError::InvalidArgument("v3 vector feather radius".into()))?;
        let sample = if radius_q8 == 0 {
            roi
        } else {
            let grid = RoiQ::from_roi(roi)?;
            let k = alto_core::blur::blur_coarsen_k(feather)
                .ok_or_else(|| RenderError::InvalidArgument("v3 vector feather coarsen".into()))?;
            grid.coarsen(k.max(grid.level))?.to_roi()?
        };
        let effect = REffect {
            kind: REffectKind::GaussianBlur {
                radius: feather,
                radius_q8,
            },
            selection: None,
        };
        let input = if radius_q8 == 0 {
            sample
        } else {
            Self::input_roi_v3(std::slice::from_ref(&effect), sample)?
        };
        let coverage = self.vector_q14(
            &[StyledPath {
                data: path.clone(),
                fill: Some([1.0; 4]),
                stroke: None,
                fill_rule: rule,
            }],
            world,
            input,
        )?;
        if radius_q8 == 0 {
            return Ok(coverage);
        }
        let blurred = self.stage(coverage, &effect, sample, WorldTransform::V3(world))?;
        self.crop(blurred, roi)
    }
    fn vector_coverage(
        &mut self,
        node: &RNode,
        world: WorldTransform,
        roi: Roi,
    ) -> Result<Option<Target>> {
        if self.resources.exact {
            let mut result = None;
            if let Some(mask) = &node.features.vector_mask {
                let local =
                    alto_core::FixedAffine::from_affine(mask.transform).ok_or_else(|| {
                        RenderError::GpuUnsupported("v3 vector mask transform out of range".into())
                    })?;
                let transform = world.fixed()?.concat(local).ok_or_else(|| {
                    RenderError::GpuUnsupported("v3 vector mask world out of range".into())
                })?;
                result = Some(self.path_coverage_q14(
                    &mask.path,
                    mask.fill_rule,
                    transform,
                    roi,
                    mask.feather,
                )?);
            }
            if let Some(path) = &node.features.clip_path {
                let local =
                    alto_core::FixedAffine::from_affine(path.transform).ok_or_else(|| {
                        RenderError::GpuUnsupported("v3 clip path transform out of range".into())
                    })?;
                let transform = world.fixed()?.concat(local).ok_or_else(|| {
                    RenderError::GpuUnsupported("v3 clip path world out of range".into())
                })?;
                let coverage =
                    self.path_coverage_q14(&path.path, path.fill_rule, transform, roi, 0.0)?;
                result = Some(if let Some(mask) = result {
                    let out = self.alloc(roi)?;
                    let params = Self::q14_params(
                        26,
                        roi,
                        [Some(mask.roi), Some(coverage.roi)],
                        [0; 4],
                        None,
                    )?;
                    self.dispatch(&out, &mask, &coverage, &mask, params, &[])?;
                    out
                } else {
                    coverage
                });
            }
            return Ok(result);
        }
        let world = world.legacy()?;
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
    fn apply_props(
        &mut self,
        source: Target,
        node: &RNode,
        world: WorldTransform,
    ) -> Result<Target> {
        // Pixel-mask/opacity is applied first, then one vector coverage product.
        if self.resources.exact {
            if node.features.mask.is_none()
                && node.features.vector_mask.is_none()
                && node.features.clip_path.is_none()
            {
                return Ok(source);
            }
            let out = self.alloc(source.roi)?;
            let (entries, transform) = if let Some(mask) = &node.features.mask {
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
                (entries, Some(transform))
            } else {
                (vec![], None)
            };
            let p = Self::mask_params(
                21,
                transform,
                source.roi,
                source.roi,
                source.roi,
                [
                    crate::composite_opacity_q14(node.opacity),
                    i32::from(node.features.mask.is_some()),
                    0,
                    0,
                ],
            )?;
            self.dispatch(&out, &source, &source, &source, p, &entries)?;
            if let Some(coverage) = self.vector_coverage(node, world, source.roi)? {
                let masked = self.alloc(source.roi)?;
                let p = Self::q14_params(
                    26,
                    source.roi,
                    [Some(out.roi), Some(coverage.roi)],
                    [0; 4],
                    None,
                )?;
                self.dispatch(&masked, &out, &coverage, &out, p, &[])?;
                return Ok(masked);
            }
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
        let mut p = Self::params(4, world.legacy()?);
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
            p = Self::params(4, transform.legacy()?);
            p.settings = [node.opacity, 1.0, 0.0, 0.0];
        }
        self.dispatch(&out, &source, &source, &source, p, &entries)?;
        if let Some(mask) = self.vector_coverage(node, world, source.roi)? {
            let masked = self.alloc(source.roi)?;
            self.dispatch(
                &masked,
                &out,
                &mask,
                &out,
                Self::params(11, world.legacy()?),
                &[],
            )?;
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
            let opacity = if node.features.mask.is_some()
                || node.features.vector_mask.is_some()
                || node.features.clip_path.is_some()
            {
                1.0
            } else {
                node.opacity
            };
            p = Self::q14_params(
                19,
                dst.roi,
                [Some(dst.roi), Some(src.roi)],
                [
                    crate::composite_opacity_q14(opacity),
                    node.blend as i32,
                    0,
                    0,
                ],
                None,
            )?;
        } else {
            p = Self::params(1, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            p.settings = [1.0, mode(node.blend), f32::from(clip.is_some()), 0.0];
        }
        self.dispatch(&out, &dst, &src, clip.as_ref().unwrap_or(&dst), p, &[])?;
        Ok(out)
    }
    fn clip_blend(&mut self, base: Target, source: Target, node: &RNode) -> Result<Target> {
        let out = self.alloc(base.roi)?;
        let p = if self.resources.exact {
            let opacity = if node.features.mask.is_some()
                || node.features.vector_mask.is_some()
                || node.features.clip_path.is_some()
            {
                1.0
            } else {
                node.opacity
            };
            Self::q14_params(
                27,
                base.roi,
                [Some(base.roi), Some(source.roi)],
                [
                    crate::composite_opacity_q14(opacity),
                    node.blend as i32,
                    0,
                    0,
                ],
                None,
            )?
        } else {
            let mut p = Self::params(9, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            p.settings[1] = mode(node.blend);
            p
        };
        self.dispatch(&out, &base, &source, &base, p, &[])?;
        Ok(out)
    }
    fn stage(
        &mut self,
        input: Target,
        effect: &REffect,
        roi: Roi,
        world: WorldTransform,
    ) -> Result<Target> {
        // A selection on either integer stage is mode 22 below.
        if self.resources.exact
            && !matches!(
                effect.kind,
                REffectKind::ColorAdjust { .. }
                    | REffectKind::GaussianBlur { .. }
                    | REffectKind::Invert
                    | REffectKind::Curves { .. }
                    | REffectKind::Levels { .. }
                    | REffectKind::HueSaturation { .. }
            )
        {
            return Err(RenderError::Internal(
                "v3 effect without an integer stage reached the M5 renderer".into(),
            ));
        }
        let filtered = match effect.kind {
            REffectKind::GaussianBlur { radius, radius_q8 } => {
                if self.resources.exact {
                    let grid = RoiQ::from_roi(roi)?;
                    let (r, rho8) = grid.taps(radius_q8)?;
                    if r == 0 {
                        input.clone()
                    } else {
                        let weights = super::blur_kernel_q16(r, rho8).ok_or_else(|| {
                            RenderError::InvalidArgument("v3 blur kernel out of range".into())
                        })?;
                        let middle_roi = grid.expand(0, r)?.to_roi()?;
                        let middle = self.alloc(middle_roi)?;
                        let horizontal = Self::blur_params(middle_roi, input.roi, r, &weights, 0)?;
                        self.dispatch(&middle, &input, &input, &input, horizontal, &[])?;
                        let out = self.alloc(roi)?;
                        let vertical = Self::blur_params(roi, middle_roi, r, &weights, 1)?;
                        self.dispatch(&out, &middle, &middle, &middle, vertical, &[])?;
                        out
                    }
                } else {
                    let r = (radius / roi.scale).ceil() as u32;
                    if r == 0 {
                        input.clone()
                    } else {
                        let middle = self.alloc(roi.expand(0, r))?;
                        let mut p = Self::params(2, world.legacy()?);
                        p.settings = [(radius / roi.scale / 3.0).max(0.333333), 0.0, 0.0, r as f32];
                        self.dispatch(&middle, &input, &input, &input, p, &[])?;
                        let out = self.alloc(roi)?;
                        p.info[3] = 1;
                        self.dispatch(&out, &middle, &middle, &middle, p, &[])?;
                        out
                    }
                }
            }
            REffectKind::ColorAdjust {
                brightness,
                contrast,
                saturation,
            } => {
                let out = self.alloc(roi)?;
                // Version 1 and 2 keep the float formula in mode 3. Version 3
                // uses the Q14 formula in mode 17, with the Q14 settings in `grid`.
                let p = if self.resources.exact {
                    Self::q14_params(
                        17,
                        roi,
                        [Some(input.roi), None],
                        [
                            super::color_adjust_q14(brightness),
                            super::color_adjust_q14(contrast),
                            super::color_adjust_q14(saturation),
                            0,
                        ],
                        None,
                    )?
                } else {
                    let mut p = Self::params(3, world.legacy()?);
                    p.settings = [brightness, contrast, saturation, 0.0];
                    p
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
                let p = if self.resources.exact {
                    let gamma_q = super::levels_gamma_q14(gamma);
                    let inverse_gamma_q = ((i64::from(1_i32 << 14).pow(2) + i64::from(gamma_q / 2))
                        / i64::from(gamma_q)) as i32;
                    let mut p = Self::q14_params(
                        32,
                        roi,
                        [Some(input.roi), None],
                        [
                            super::color_adjust_q14(in_black),
                            super::color_adjust_q14(in_white),
                            inverse_gamma_q,
                            super::color_adjust_q14(out_black),
                        ],
                        None,
                    )?;
                    let words: &mut [u32] = bytemuck::cast_slice_mut(&mut p.curve);
                    words[24] = super::color_adjust_q14(out_white) as u32;
                    p
                } else {
                    let mut p = Self::params(13, world.legacy()?);
                    p.settings = [in_black, in_white, gamma, out_black];
                    p.extra[0] = out_white;
                    p
                };
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
            REffectKind::HueSaturation {
                hue,
                saturation,
                lightness,
            } => {
                let out = self.alloc(roi)?;
                let p = if self.resources.exact {
                    let hue_q16 = crate::raster_q8::q_from_f32_bits(hue.to_bits(), 16)
                        .and_then(|q| i32::try_from(q).ok())
                        .ok_or_else(|| {
                            RenderError::InvalidArgument("v3 hue out of range".into())
                        })?;
                    Self::q14_params(
                        33,
                        roi,
                        [Some(input.roi), None],
                        [
                            hue_q16,
                            super::color_adjust_q14(saturation),
                            super::color_adjust_q14(lightness),
                            0,
                        ],
                        None,
                    )?
                } else {
                    let mut p = Self::params(15, world.legacy()?);
                    p.settings = [hue, saturation, lightness, 0.0];
                    p
                };
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
            REffectKind::Invert => {
                let out = self.alloc(roi)?;
                let p = if self.resources.exact {
                    Self::q14_params(30, roi, [Some(input.roi), None], [0; 4], None)?
                } else {
                    Self::params(14, world.legacy()?)
                };
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
            REffectKind::Curves { ref points } => {
                let out = self.alloc(roi)?;
                let p = if self.resources.exact {
                    let mut p = Self::q14_params(
                        31,
                        roi,
                        [Some(input.roi), None],
                        [points.len().min(16) as i32, 0, 0, 0],
                        None,
                    )?;
                    let words: &mut [u32] = bytemuck::cast_slice_mut(&mut p.curve);
                    for (i, (x, y)) in points.iter().take(16).enumerate() {
                        words[24 + i * 2] = super::color_adjust_q14(*x) as u32;
                        words[25 + i * 2] = super::color_adjust_q14(*y) as u32;
                    }
                    p
                } else {
                    let mut p = Self::params(16, world.legacy()?);
                    for (i, (x, y)) in points.iter().take(16).enumerate() {
                        p.curve[i * 4] = *x;
                        p.curve[i * 4 + 1] = *y;
                    }
                    p.extra[0] = points.len().min(16) as f32;
                    p
                };
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
                Self::params(5, transform.legacy()?)
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
        world: WorldTransform,
    ) -> Result<Target> {
        let mut outputs = vec![desired; effects.len()];
        let mut roi = desired;
        for (i, e) in effects.iter().enumerate().rev() {
            outputs[i] = roi;
            if let REffectKind::GaussianBlur { radius, radius_q8 } = e.kind {
                if self.resources.exact {
                    let grid = RoiQ::from_roi(roi)?;
                    let (r, _) = grid.taps(radius_q8)?;
                    roi = grid.expand(r, r)?.to_roi()?;
                } else {
                    let r = (radius / roi.scale).ceil() as u32;
                    roi = roi.expand(r, r);
                }
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
        world: WorldTransform,
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
        if let REffectKind::GaussianBlur { radius, .. } = effect.kind {
            let sample = if self.resources.exact {
                let grid = RoiQ::from_roi(desired)?;
                let target = alto_core::blur::blur_coarsen_k(radius)
                    .map(|k| k.max(grid.level))
                    .unwrap_or(grid.level);
                grid.coarsen(target)?.to_roi()?
            } else {
                desired.coarsen(radius / 64.0)
            };
            let source = self.stack(
                input,
                previous,
                if self.resources.exact {
                    Self::input_roi_v3(std::slice::from_ref(effect), sample)?
                } else {
                    Self::input_roi(std::slice::from_ref(effect), sample)
                },
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
                let p = if self.resources.exact {
                    Self::mask_params(
                        22,
                        Some(transform),
                        desired,
                        original.roi,
                        filtered.roi,
                        [0; 4],
                    )?
                } else {
                    Self::params(5, transform.legacy()?)
                };
                self.dispatch(&out, &original, &filtered, &original, p, &entries)?;
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
            if let REffectKind::GaussianBlur { radius, .. } = e.kind {
                let r = (radius / roi.scale).ceil() as u32;
                roi = roi.expand(r, r);
            }
        }
        roi
    }
    fn input_roi_v3(effects: &[REffect], roi: Roi) -> Result<Roi> {
        let mut grid = RoiQ::from_roi(roi)?;
        for effect in effects.iter().rev() {
            if let REffectKind::GaussianBlur { radius_q8, .. } = effect.kind {
                let (r, _) = grid.taps(radius_q8)?;
                grid = grid.expand(r, r)?;
            }
        }
        grid.to_roi()
    }
    fn children(
        &mut self,
        children: &[usize],
        mut accum: Target,
        world: WorldTransform,
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
            let transform = world.concat(node.transform)?;
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
                let filtered = if stack.iter().any(|e| {
                    if self.resources.exact {
                        matches!(e.kind,REffectKind::GaussianBlur {radius_q8, ..} if radius_q8>0)
                    } else {
                        matches!(e.kind,REffectKind::GaussianBlur {radius, ..} if radius>0.0)
                    }
                }) {
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
                    // Mode 23 combines opacity, blend, and an optional mask.
                    // Otherwise only the opacity mixes, in Q14 (mode 20).
                    if node.features.mask.is_some() || node.blend != RenderBlend::Normal {
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
                        let p = Self::q14_params(
                            20,
                            desired,
                            [Some(input.roi), Some(filtered.roi)],
                            [crate::composite_opacity_q14(node.opacity), 0, 0, 0],
                            None,
                        )?;
                        self.dispatch(&out, &input, &filtered, &input, p, &[])?;
                        mixed = out;
                    }
                } else if node.features.mask.is_some()
                    || node.opacity != 1.0
                    || node.features.clip
                    || node.blend != RenderBlend::Normal
                {
                    let out = self.alloc(desired)?;
                    let mut p = Self::params(7, transform.legacy()?);
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
                        p = Self::params(7, t.legacy()?);
                        p.settings = [node.opacity, 1.0, f32::from(node.features.clip), 0.0];
                    }
                    p.settings[2] = 0.0;
                    p.settings[3] = mode(node.blend);
                    self.dispatch(&out, &input, &filtered, &input, p, &entries)?;
                    mixed = out;
                }
                if let Some(mask) = self.vector_coverage(&node, transform, desired)? {
                    let out = self.alloc(desired)?;
                    let p = if self.resources.exact {
                        Self::q14_params(
                            29,
                            desired,
                            [Some(input.roi), Some(mixed.roi)],
                            [0; 4],
                            None,
                        )?
                    } else {
                        Self::params(12, transform.legacy()?)
                    };
                    self.dispatch(&out, &input, &mixed, &mask, p, &[])?;
                    mixed = out;
                }
                if node.features.clip {
                    let out = self.alloc(desired)?;
                    let p = if self.resources.exact {
                        Self::q14_params(
                            28,
                            desired,
                            [Some(input.roi), Some(mixed.roi)],
                            [0; 4],
                            None,
                        )?
                    } else {
                        Self::params(10, transform.legacy()?)
                    };
                    self.dispatch(&out, &input, &mixed, &input, p, &[])?;
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
        parent: WorldTransform,
        depth: usize,
    ) -> Result<Target> {
        if depth > alto_core::MAX_TREE_DEPTH as usize {
            return Err(RenderError::InvalidArgument("effect graph too deep".into()));
        }
        let node = self.scene.nodes[index].clone();
        let world = parent.concat(node.transform)?;
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
        world: WorldTransform,
        depth: usize,
    ) -> Result<Target> {
        let node = self.scene.nodes[index].clone();
        let source = match &node.content {
            RContent::Tiles(tiles) => {
                let layer = if self.mask_preview {
                    None
                } else {
                    Some(node.key)
                };
                // Version 3 mode 18 reads the nearest texel through the Q16 lookup
                // (`mask_sampler`, as the masks) and premultiplies its bytes into
                // Q14. A singular transform draws nothing, as the f64 selection
                // below. Version 1 and 2 keep the f32 inverse and mode 0.
                if self.resources.exact {
                    if v3_inverse(world.fixed()?)?.is_none() {
                        return self.zero(roi);
                    }
                    let (entries, sampler) =
                        self.lookup_entries(tiles, world, roi, layer, "raster")?;
                    let out = self.alloc(roi)?;
                    let p = Self::q14_params(18, roi, [None, None], [0; 4], Some(&sampler))?;
                    let dummy = self.dummy();
                    self.dispatch(&out, &dummy, &dummy, &dummy, p, &entries)?;
                    return Ok(out);
                }
                let out = self.alloc(roi)?;
                let entries = self.entries(tiles, world.legacy()?, roi, layer)?;
                let p = Self::params(0, world.legacy()?);
                let dummy = self.dummy();
                self.dispatch(&out, &dummy, &dummy, &dummy, p, &entries)?;
                out
            }
            RContent::Mesh(paths) => {
                if self.resources.exact {
                    self.vector_q14(paths, world.fixed()?, roi)?
                } else {
                    self.vector(VectorContent::Paths(paths), world.legacy()?, roi, true)?
                }
            }
            RContent::Text(runs) => {
                self.vector(VectorContent::Text(runs), world.legacy()?, roi, false)?
            }
            RContent::TextBake(bake) => {
                if !self.resources.exact {
                    return Err(RenderError::Internal(
                        "a text bake reached a v1 / v2 renderer".into(),
                    ));
                }
                self.text_q14(bake.as_ref(), world.fixed()?, roi)?
            }
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
                self.pipelines_built = true;
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
            // Version 3 checks the integer origin before any stage, and resolves
            // the canvas edge exactly (`resolve_span`), not in f32.
            let origin = if exact {
                Some(integer_origin(key, roi)?)
            } else {
                None
            };
            let source = cx.node(
                scene.root,
                roi,
                WorldTransform::root(scene.render_version),
                0,
            )?;
            let view = cx.resources.pool[source.index]
                .texture
                .create_view(&Default::default());
            let uniform = if let Some(grid) = origin {
                buffer(
                    &cx.r.device,
                    &cx.r.queue,
                    "M5 integer resolve config",
                    &[IntegerResolve {
                        span: resolve_span_q(grid, [scene.canvas.x1, scene.canvas.y1]),
                        info: [slot, 0, 0, 0],
                    }],
                    wgpu::BufferUsages::UNIFORM,
                )?
            } else {
                buffer(
                    &cx.r.device,
                    &cx.r.queue,
                    "M5 resolve config",
                    &[Config {
                        origin_scale: [roi.x, roi.y, roi.scale, scene.canvas.x1 as f32],
                        info: [slot, scene.canvas.y1 as u32, 0, 0],
                    }],
                    wgpu::BufferUsages::UNIFORM,
                )?
            };
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
    fn roi(x: f32, y: f32, scale: f32) -> Roi {
        Roi {
            x,
            y,
            scale,
            w: 256,
            h: 256,
        }
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
            Some([5, 9, 2, 4])
        );
        // Levels below 0 share the finer grid: texel floor((2i + 1) / 4), then 2i + 1.
        assert_eq!(
            map([0.0, 0.0, 0.5], [256, 256], [0.0, 0.0, 1.0]),
            Some([1, 1, 2, 4])
        );
        assert_eq!(
            map([0.0, 0.0, 1.0], [256, 256], [0.0, 0.0, 0.5]),
            Some([2, 2, 4, 2])
        );
        for (out, input) in [
            ([0.5, 0.0, 1.0], [0.0, 0.0, 1.0]),
            ([0.0, 0.0, 1.0], [0.0, -0.25, 1.0]),
            ([0.25, 0.0, 0.5], [0.0, 0.0, 1.0]),
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
                roi(-300.25, 37.0, 0.5),
                [16384, 0, 0, 0]
            )));
            // A mask origin off its level's grid, below or at level 0.
            for at in [roi(0.25, 0.0, 0.5), roi(0.5, 0.0, 1.0)] {
                assert!(unsupported(Context::mask_params(
                    mode,
                    Some(WorldTransform::V3(alto_core::FixedAffine::IDENTITY)),
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
    /// Float tokens a v3 stage must not contain: float types, float built-ins,
    /// float literals (a digit, a point, a digit) and the f32 uniform slots.
    fn float_tokens(body: &str) -> Vec<String> {
        let mut found: Vec<String> = [
            "f32",
            "vec2f",
            "vec3f",
            "vec4f",
            "floor(",
            "fract(",
            "round(",
            "mix(",
            "c.unused",
            "c.extra",
            "out_region",
            "region_a",
            "region_b",
            "inv0",
            "inv1",
            "c.settings",
            "origin_scale",
        ]
        .into_iter()
        .filter(|t| body.contains(t))
        .map(str::to_string)
        .collect();
        let b = body.as_bytes();
        for i in 1..b.len().saturating_sub(1) {
            if b[i] == b'.' && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit() {
                found.push(format!("literal at {i}"));
            }
        }
        found
    }
    /// The v3 paint shaders' only float is the `R8Unorm` coverage texel: one
    /// `texture_2d<f32>` binding, read once and passed straight to `unorm_byte`.
    #[test]
    fn v3_paint_shaders_read_only_the_coverage_byte_as_float() {
        for (name, text, texture) in [
            (
                "vector-q14",
                include_str!("../../shaders/vector-q14.wgsl"),
                "coverage",
            ),
            (
                "glyph-q14",
                include_str!("../../shaders/glyph-q14.wgsl"),
                "scratch",
            ),
        ] {
            let code = strip_comments(text);
            assert_eq!(code.matches("f32").count(), 1, "{name}: one float binding");
            assert!(
                code.contains(&format!("var {texture}: texture_2d<f32>;")),
                "{name}"
            );
            assert_eq!(
                code.matches(&format!("textureLoad({texture},")).count(),
                1,
                "{name}"
            );
            assert!(
                code.contains(&format!("unorm_byte(textureLoad({texture},")),
                "{name}"
            );
            assert!(!code.contains("textureSample"), "{name}");
        }
    }
    /// The float purge: no v3 M5 stage (`effects-q14.wgsl`, every mode from
    /// `main`) and not the v3 resolve reads or computes a float, except the
    /// Rgba8Unorm atlas texel in `atlas_bytes`, which goes straight through
    /// `unorm_byte` back to its bytes. The f32 coordinate helpers of earlier
    /// versions (`load_a`, `load_b`, `sparse_bytes`) are gone, the uniform
    /// declares no float field, and every mode reaches the integer placement.
    #[test]
    fn v3_stages_call_no_f32_coordinate_helper() {
        let shader = strip_comments(&crate::integer_effects_shader());
        let functions = wgsl_functions(&shader);
        for gone in ["load_a", "load_b", "sparse_bytes", "sparse", "load_clip"] {
            assert!(!functions.contains_key(gone), "{gone} is still defined");
        }
        let mut reached = vec!["main".to_string()];
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
            "mask_stage",
            "texel_a",
            "texel_b",
            "input_texel",
            "floor_div",
            "color_adjust_q14",
            "layer_q14",
            "lookup",
            "tile_texel",
            "atlas_bytes",
            "mask_q",
            "pixel_q8",
            "coord_q8",
            "div28",
            "unorm_byte",
            "premul_byte_q14",
            "byte_q14",
        ] {
            assert!(
                reached.iter().any(|r| r == needed),
                "main does not reach {needed}"
            );
        }
        for name in reached
            .iter()
            .filter(|n| !["unorm_byte", "atlas_bytes"].contains(&n.as_str()))
        {
            let tokens = float_tokens(&functions[name]);
            assert!(
                tokens.is_empty(),
                "{name} (reached from main) uses {tokens:?}"
            );
        }
        // The atlas is read only in `atlas_bytes`, and each channel goes through
        // `unorm_byte`.
        assert_eq!(shader.matches("textureLoad(atlas").count(), 1);
        let atlas = &functions["atlas_bytes"];
        assert!(atlas.contains("textureLoad(atlas"));
        for ch in ["s.r", "s.g", "s.b", "s.a"] {
            assert!(atlas.contains(&format!("unorm_byte({ch})")), "{ch}");
        }
        // The uniform declares integers only.
        let config = &shader[shader.find("struct Config").unwrap()..];
        let config = &config[..config.find('}').unwrap()];
        assert!(float_tokens(config).is_empty(), "{config}");
        assert!(config.contains("grid: array<vec4i, 16>"));
        // The v3 resolve is integer throughout, its canvas test included.
        let resolve = strip_comments(include_str!("../../shaders/effects-resolve-q14.wgsl"));
        assert!(
            float_tokens(&resolve).is_empty(),
            "{:?}",
            float_tokens(&resolve)
        );
        assert!(resolve.contains("all(id.xy>=c.span.xy)&&all(id.xy<c.span.zw)"));
    }
    /// Every function `main` reaches in `src`, `main` included.
    fn reached_from_main(functions: &HashMap<String, String>) -> Vec<String> {
        let mut reached = vec!["main".to_string()];
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
        reached
    }
    /// The plain-path float purge: the v3 integer tile compositor
    /// (`integer_composite_shader`) has no f32 fetch, f32 canvas test or float
    /// uniform. Everything `main` reaches is integer except the atlas texel in
    /// `sample_byte`, which goes straight through `unorm_byte`.
    #[test]
    fn v3_plain_compositor_reads_no_float() {
        let shader = strip_comments(&crate::integer_composite_shader());
        let functions = wgsl_functions(&shader);
        for gone in ["fetch_legacy", "sample_texel", "fetch"] {
            assert!(!functions.contains_key(gone), "{gone} is still defined");
        }
        let reached = reached_from_main(&functions);
        for needed in [
            "fetch_bytes",
            "fetch_integer",
            "sample_byte",
            "channel_byte",
            "pixel_q8",
            "coord_q8",
            "unorm_byte",
            "blend_q",
        ] {
            assert!(
                reached.iter().any(|r| r == needed),
                "main does not reach {needed}"
            );
        }
        // `origin_scale` and `inv0` are integer fields here (the struct
        // declarations are checked below).
        let tokens = |body: &str| -> Vec<String> {
            float_tokens(body)
                .into_iter()
                .filter(|t| t != "origin_scale" && t != "inv0")
                .collect()
        };
        for name in reached
            .iter()
            .filter(|n| !["unorm_byte", "sample_byte"].contains(&n.as_str()))
        {
            let found = tokens(&functions[name]);
            assert!(
                found.is_empty(),
                "{name} (reached from main) uses {found:?}"
            );
        }
        let sample = &functions["sample_byte"];
        assert_eq!(shader.matches("textureLoad(atlas").count(), 1);
        assert!(sample.contains("textureLoad(atlas"));
        for ch in ["s.r", "s.g", "s.b", "s.a"] {
            assert!(sample.contains(&format!("unorm_byte({ch})")), "{ch}");
        }
        assert!(tokens(&sample.replace("textureLoad(atlas", "")).is_empty());
        for decl in ["struct Command", "struct Config"] {
            let at = &shader[shader.find(decl).unwrap()..];
            let body = &at[..at.find('}').unwrap()];
            assert!(tokens(body).is_empty(), "{body}");
        }
        assert!(shader.contains("all(id.xy>=config.span.xy) && all(id.xy<config.span.zw)"));
        assert!(shader.contains("let fx = select(qx.v - (x << 8), 0, c.info.w == 2u);"));
        assert!(shader.contains("let fy = select(qy.v - (y << 8), 0, c.info.w == 2u);"));
    }
    /// The v3 resolve span is exactly the pixels whose center is on the canvas,
    /// at origins where an f32 center is not exact (past 2^24 px) and on both
    /// sides of 0.
    #[test]
    fn v3_resolve_span_is_exact() {
        let span = super::super::resolve_span;
        assert_eq!(span([0, 0], 1, [256, 256]), [0, 0, 256, 256]);
        assert_eq!(span([999_936, 0], 1, [1_000_036, 77]), [0, 0, 100, 77]);
        assert_eq!(span([-44, 37], 1, [1_000, 400]), [44, 0, 256, 256]);
        assert_eq!(span([-300, 0], 1, [-10, 400]), [0; 4]);
        for (origin, scale) in [
            (0i64, 1i64),
            (-1, 1),
            (16_777_216, 1),
            (33_554_432 + 256, 2),
            (-8_388_608, 4),
            (999_936, 1),
        ] {
            for end in [
                origin - 1,
                origin,
                origin + 1,
                origin + 77,
                origin + 255,
                origin + 256 * scale,
            ] {
                let [x0, _, x1, _] = span([origin, 0], scale, [end, 1 << 40]);
                for i in 0..256i64 {
                    // The center in exact doubled integers.
                    let c = 2 * origin + (2 * i + 1) * scale;
                    let on = c >= 0 && c < 2 * end;
                    assert_eq!(
                        (x0..x1).contains(&(i as u32)),
                        on,
                        "origin {origin} scale {scale} end {end} pixel {i}"
                    );
                }
            }
        }
        // f32 would round the canvas edge 16777217 to 16777216 and drop pixel 0.
        assert_eq!(span([16_777_216, 0], 1, [16_777_217, 1]), [0, 0, 1, 1]);
        assert_eq!(16_777_217f32, 16_777_216f32);
    }
    fn grid(level: i32, tx: i64, ty: i64) -> RoiQ {
        RoiQ {
            level,
            ox: tx * 256,
            oy: ty * 256,
            w: 256,
            h: 256,
        }
    }
    fn same(a: Roi, b: Roi) -> bool {
        a.x.to_bits() == b.x.to_bits()
            && a.y.to_bits() == b.y.to_bits()
            && a.scale.to_bits() == b.scale.to_bits()
            && (a.w, a.h) == (b.w, b.h)
    }
    #[test]
    fn blur_roi_q_matches_f32_roi() {
        let tiles = [-(1i64 << 15), -3, -1, 0, 1, 7, 1 << 15];
        let radii = [1u32, 2, 255, 256, 257, 4096, 8191, 8192, 8193, 16383, 16384];
        for level in -6..=12 {
            for &tx in &tiles {
                for &ty in &tiles {
                    let q = grid(level, tx, ty);
                    let f = q.to_roi().unwrap();
                    assert_eq!(RoiQ::from_roi(f).unwrap(), q);
                    for &r8 in &radii {
                        // R / 256 is exact in f32, so v2 and v3 read one radius.
                        let rf = r8 as f32 / 256.0;
                        let k = alto_core::blur::blur_coarsen_k(rf).unwrap();
                        let sample = q.coarsen(k.max(level)).unwrap();
                        let sample_f = f.coarsen(rf / 64.0);
                        assert!(
                            same(sample.to_roi().unwrap(), sample_f),
                            "coarsen level {level} tile ({tx}, {ty}) R {r8}"
                        );
                        let (taps, _) = sample.taps(r8).unwrap();
                        assert_eq!(taps, (rf / sample_f.scale).ceil() as u32);
                        let input = sample.expand(taps, taps).unwrap();
                        assert!(
                            same(input.to_roi().unwrap(), sample_f.expand(taps, taps)),
                            "expand level {level} tile ({tx}, {ty}) R {r8}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn blur_rejects_out_of_range_levels() {
        for level in [-7, -40, 13, i32::MIN, i32::MAX] {
            let q = RoiQ {
                level,
                ox: 0,
                oy: 0,
                w: 256,
                h: 256,
            };
            assert!(matches!(
                q.taps(16384),
                Err(RenderError::InvalidArgument(_))
            ));
            assert!(matches!(q.coarsen(0), Err(RenderError::InvalidArgument(_))));
            assert!(matches!(
                q.coarsen(i32::MAX),
                Err(RenderError::InvalidArgument(_))
            ));
            let f = Roi {
                x: 0.0,
                y: 0.0,
                scale: 2f32.powi(level.clamp(-126, 127)),
                w: 256,
                h: 256,
            };
            assert!(RoiQ::from_roi(f).is_err());
        }
        // In range, a ratio over 128 and a finer target are refused too.
        assert!(grid(-6, 0, 0).coarsen(2).is_err());
        assert!(grid(0, 0, 0).coarsen(-1).is_err());
        assert!(grid(0, 0, 0).taps(16385).is_err());
    }
}
