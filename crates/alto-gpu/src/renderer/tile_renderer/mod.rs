//! General tile renderer for effects, masks and clipping. Back-propagated ROIs provide the halo for every blur stage.
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
mod graph;
mod paint;
mod params;
#[cfg(test)]
mod pool_tests;
mod stages;
mod targets;
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
    glyph_q14: Option<wgpu::ComputePipeline>,
    pool: Vec<Pooled>,
    /// Version 3 vector paths prepared for a world and level, shared by every
    /// tile (`paint.rs`).
    vectors: Vec<PreparedVector>,
    pub(super) allocations: u64,
    pub(super) reuses: u64,
}
/// The most vector nodes whose prepared paths are kept.
const PREPARED_VECTORS: usize = 32;
/// The most prepared stroke-outline bytes kept over all nodes. Past it a tile
/// builds the outlines it needs without keeping them.
const PREPARED_BYTES: usize = 32 << 20;
/// A vector node's paths in Q8 level pixels for one world and level: what does
/// not depend on the tile. The entry holds its own `Arc`, so the address of a
/// live `paths` can only match the same, unchanged path list.
struct PreparedVector {
    paths: Arc<Vec<StyledPath>>,
    world: alto_core::FixedAffine,
    level: i32,
    used: u64,
    /// `None` when a point of some path is outside the Q8 range: the whole node
    /// is transparent (VECTOR_TRANSFORM_RANGE).
    mapped:
        Option<Vec<std::result::Result<crate::raster_q8::Prepared, crate::raster_q8::RasterError>>>,
    /// Per path, its stroke outline once a tile has needed it.
    outlines: Vec<Option<StrokeOutline>>,
    /// The packed bytes of `outlines`.
    bytes: usize,
}
type StrokeOutline =
    std::result::Result<Option<crate::raster_q8::Prepared>, crate::raster_q8::RasterError>;
impl Resources {
    fn new(device: &wgpu::Device, governor: &Arc<Governor>, exact: bool) -> Result<Self> {
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
                    include_str!("../../../shaders/effects-resolve-q14.wgsl"),
                ),
            )
        } else {
            (
                compute(
                    device,
                    "M5 effects",
                    &crate::shader_with_blend(include_str!("../../../shaders/effects.wgsl")),
                ),
                compute(
                    device,
                    "M5 resolve",
                    include_str!("../../../shaders/effects-resolve.wgsl"),
                ),
            )
        };
        Ok(Self {
            exact,
            allocations: 0,
            reuses: 0,
            pipeline,
            resolve,
            glyph_q14: exact.then(|| {
                compute(
                    device,
                    "M5 integer glyph paint",
                    &format!(
                        "{}\n{}",
                        include_str!("../../../shaders/q14.wgsl"),
                        include_str!("../../../shaders/glyph-q14.wgsl")
                    ),
                )
            }),
            vectors: Vec::new(),
            pool: vec![Pooled {
                texture: memory::create_texture(
                    governor,
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
        self.vectors
            .retain(|v| frame.saturating_sub(v.used) < POOL_IDLE_FRAMES);
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

enum VectorContent<'a> {
    Paths(&'a [StyledPath]),
    Text(&'a [alto_vector::TextRun]),
}
impl Renderer {
    pub(super) fn compose_tiles(
        &mut self,
        scene: &RenderScene,
        key: CacheKey,
        slot: u32,
        store: &dyn TileStore,
    ) -> Result<()> {
        let exact = scene.render_version >= alto_core::RENDER_VERSION_EXACT;
        let mut resources = match self.tile_renderer.take() {
            Some(r) if r.exact() == exact => r,
            other => {
                if let Some(old) = other {
                    old.retire(&mut self.retired);
                }
                self.pipelines_built = true;
                Resources::new(&self.device, &self.governor, exact)?
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
                    &cx.r.governor,
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
                    &cx.r.governor,
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
        self.tile_renderer = Some(resources);
        result
    }
}
