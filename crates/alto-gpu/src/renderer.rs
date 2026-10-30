//! Worker and native renderer. The document remains CPU-owned; all textures are caches.
use crate::{RContent, RenderBlend, RenderScene};
use alto_core::{ContentHash, MAX_ISOLATED_GROUPS, NodeId, Rect, TILE, TileKey};
use alto_raster::{Dab, TileData, TileStore};
use bytemuck::{Pod, Zeroable};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

mod cache;
mod m5;
use cache::overview_level;
mod memory;
mod presentation;
mod readback;
pub use memory::GpuBytes;
use memory::{Class, Governor, cache_layers};
pub use presentation::DisplayReport;
pub use readback::{FinishControl, ReadbackStats, StrokeReadback};
pub mod vector;

pub use crate::RenderError;
type Result<T> = std::result::Result<T, RenderError>;
type CacheKey = (i32, i32, i32);
/// CPU time one resumable job slice may use before yielding to the Worker.
const JOB_SLICE_MS: f64 = 1.8;
/// Passes over a job's inputs before it is reported as blocked by atlas pressure.
const MAX_JOB_RETRIES: u32 = 8;
/// One reusable 512x512 RGBA texture for pyramid downsampling.
const SCRATCH_BYTES: u64 = 512 * 512 * 4;
const TILE_RGBA_BYTES: u64 = 256 * 256 * 4;
/// Jobs that need at most this many inputs decode them inline even with a pool.
const INLINE_DECODE_MAX: usize = 8;

/// Nonnegative Q16 half-kernel for version 3 Gaussian blur. Its symmetric
/// sum is exactly 65536, so a constant premultiplied input stays constant.
pub(crate) fn blur_kernel_q16(radius: u32, rho8: u32) -> Option<Vec<u32>> {
    if radius == 0 || radius > 64 || rho8 > 16384 {
        return None;
    }
    let rho = u128::from(rho8.max(256));
    let one = 1u128 << 32;
    let round = |n: u128, d: u128| (n + d / 2) / d;
    let mut g = Vec::with_capacity(radius as usize + 1);
    for i in 0..=radius {
        let y = round(4608 * u128::from(i) * u128::from(i) * one, rho * rho);
        let mut h = one;
        for k in (1..=6u128).rev() {
            h = one.checked_sub(round(y * h, k * one))?;
        }
        for _ in 0..6 {
            h = round(h * h, one);
        }
        g.push(h);
    }
    let mut tails = vec![0u128; g.len() + 1];
    for k in (1..g.len()).rev() {
        tails[k] = tails[k + 1] + g[k];
    }
    let total = g[0] + 2 * tails[1];
    let mut p = vec![0u128; g.len() + 1];
    for k in 1..g.len() {
        p[k] = round(65536 * tails[k], total);
    }
    let mut weights = vec![u32::try_from(65536 - 2 * p[1]).ok()?];
    for k in 1..g.len() {
        weights.push(u32::try_from(p[k].checked_sub(p[k + 1])?).ok()?);
    }
    Some(weights)
}

#[cfg(test)]
mod blur_kernel_tests {
    use super::blur_kernel_q16;

    #[test]
    fn blur_kernel_q16_matches_locked_vectors() {
        assert_eq!(blur_kernel_q16(1, 256), Some(vec![64112, 712]));
        assert_eq!(blur_kernel_q16(2, 256), Some(vec![64112, 712, 0]));
        assert_eq!(
            blur_kernel_q16(8, 2048),
            Some(vec![9818, 9151, 7411, 5214, 3187, 1693, 781, 313, 109])
        );
        assert_eq!(
            blur_kernel_q16(12, 3072),
            Some(vec![
                6548, 6346, 5778, 4943, 3971, 2998, 2125, 1416, 886, 521, 288, 149, 73
            ])
        );
        let w = blur_kernel_q16(64, 16384).unwrap();
        assert_eq!(&w[..4], &[1228, 1228, 1223, 1216]);
        assert_eq!(&w[61..], &[21, 18, 15, 14]);
        assert_eq!(w[0] + 2 * w[1..].iter().sum::<u32>(), 65536);
    }

    /// Every `(r, rho8)` some real radius `rho` in `(0, 64]` reaches, with
    /// `ceil(rho) = r` and `floor(256 rho + 1/2) = rho8`.
    #[test]
    fn blur_kernel_q16_is_exact() {
        let mut pairs = 0;
        for r in 1..=64u32 {
            for rho8 in 0..=16384u32 {
                // [(2 rho8 - 1) / 512, (2 rho8 + 1) / 512) meets (r - 1, r].
                if 2 * rho8 > 512 * r + 1 || 2 * rho8 < 512 * (r - 1) {
                    continue;
                }
                pairs += 1;
                let w = blur_kernel_q16(r, rho8).unwrap();
                assert_eq!(w.len(), r as usize + 1);
                assert_eq!(
                    w[0] + 2 * w[1..].iter().sum::<u32>(),
                    65536,
                    "({r}, {rho8})"
                );
                assert!(w.iter().all(|&v| v < 65536), "({r}, {rho8})");
                assert!(w.windows(2).all(|p| p[0] >= p[1]), "({r}, {rho8}) {w:?}");
            }
        }
        assert_eq!(pairs, 16448);
        assert_eq!(blur_kernel_q16(0, 256), None);
        assert_eq!(blur_kernel_q16(65, 16384), None);
        assert_eq!(blur_kernel_q16(64, 16385), None);
    }
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Command {
    inv0: [f32; 4],
    inv1: [f32; 4],
    info: [u32; 4],
    /// Little-endian i64 Q16 translation (`tx` lo/hi, `ty` lo/hi). Zero on the f32 path.
    trans: [u32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Entry {
    xy: [i32; 2],
    slot: u32,
    pad: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Config {
    origin_scale: [f32; 4],
    info: [u32; 4],
}
/// The version 3 tile compositor uniform (`integer_composite_shader`): the
/// integer origin and level scale, the command count and cache slot, and the
/// on-canvas pixel rectangle (`resolve_span`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct IntegerConfig {
    pub origin_scale: [i32; 4],
    pub info: [u32; 4],
    pub span: [u32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ViewUniform {
    center_zoom: [f32; 4],
    screen: [f32; 4],
    canvas: [f32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuDab {
    center_radius: [f32; 4],
    extra: [f32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BrushUniform {
    color: [f32; 4],
    params: [f32; 4],
    origin: [f32; 4],
    info: [u32; 4],
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub center: [f64; 2],
    pub zoom: f64,
    pub rotation: f64,
}
impl Default for Viewport {
    fn default() -> Self {
        Self {
            center: [512.0, 384.0],
            zoom: 1.0,
            rotation: 0.0,
        }
    }
}
#[derive(Default, Clone, Copy, Debug)]
pub struct FrameReport {
    pub composed: u32,
    pub pending: u32,
    pub gpu_bytes: u64,
    pub gpu: GpuBytes,
    pub display: DisplayReport,
    pub profile: Option<RenderProfile>,
    /// The frame was not presented because the previous one is still on the GPU;
    /// render again once it finishes.
    pub deferred: bool,
}
#[derive(Default, Clone, Copy, Debug)]
pub struct RenderProfile {
    pub compose_ms: f64,
    pub commands_ms: f64,
    pub get_ms: f64,
    pub write_ms: f64,
    pub uploads: u32,
    pub atlas_hits: u32,
    pub upload_bytes: u64,
    pub pyramid_hits: u32,
    pub max_step_ms: f64,
    pub steps: u32,
    pub unique_inputs: u32,
    pub atlas_evictions: u32,
}
#[derive(Default, Clone, Copy, Debug)]
pub struct PrefetchStatus {
    pub candidates: u32,
    pub ready: u32,
    pub composed: u32,
    pub total_ms: f64,
    pub max_step_ms: f64,
    pub upload_bytes: u64,
    pub max_plan_ms: f64,
    pub max_input_ms: f64,
    pub max_get_ms: f64,
    pub max_convert_ms: f64,
    pub max_write_ms: f64,
    pub max_submit_ms: f64,
    pub cancelled: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Result of advancing one resumable tile job (visible or prefetch) by one slice.
pub enum JobStep {
    Idle,
    Progress,
    Ready,
    Blocked,
}
struct PlannedSource {
    xy: [i32; 2],
    hash: ContentHash,
}
enum PlanTask {
    Node {
        idx: usize,
        world: WorldTransform,
        depth: u32,
    },
    Close {
        opacity: f32,
        blend: RenderBlend,
    },
    Tiles {
        idx: usize,
        next: usize,
        end: usize,
        inv: [f64; 6],
        min: [f64; 2],
        max: [f64; 2],
        start: u32,
        /// Render version 3: the integer draw; its keys select the tiles.
        v3: Option<PlainDraw>,
    },
}
struct TileJob {
    key: CacheKey,
    region: [f64; 4],
    stack: Vec<PlanTask>,
    commands: Vec<Command>,
    sources: Vec<PlannedSource>,
    unique: Vec<ContentHash>,
    missing: HashSet<ContentHash>,
    cursor: usize,
    planned: bool,
    retries: u32,
}
/// Inputs decoded on other threads. The renderer asks; the host decodes them and
/// hands the pixels back, and they go straight into the atlas.
#[derive(Default)]
struct Decoder {
    queued: std::collections::VecDeque<ContentHash>,
    in_flight: HashMap<ContentHash, Arc<alto_raster::StoredTile>>,
    failed: Option<ContentHash>,
    decoded: u64,
}
impl Decoder {
    fn request(&mut self, hash: ContentHash, object: Arc<alto_raster::StoredTile>) {
        if let std::collections::hash_map::Entry::Vacant(e) = self.in_flight.entry(hash) {
            e.insert(object);
            self.queued.push_back(hash);
        }
    }
}
#[derive(Default, Clone, Copy)]
struct UploadCost {
    get_ms: f64,
    convert_ms: f64,
    write_ms: f64,
    bytes: u64,
}
struct AtlasSlot {
    hash: ContentHash,
    used: u64,
}
struct Cached {
    slot: u32,
    used: u64,
    prefetched: bool,
}
pub struct StrokeTile {
    pub output: wgpu::Texture,
    base: wgpu::Texture,
    coverage: wgpu::Buffer,
    pub before: Option<ContentHash>,
    selection: Option<wgpu::Texture>,
}

pub struct GpuStroke {
    pub layer: NodeId,
    pub tiles: BTreeMap<TileKey, StrokeTile>,
    pub color: [f32; 4],
    pub hardness: f32,
    pub opacity: f32,
    pub erase: bool,
    pub epoch: u64,
    pub mask: bool,
    /// Layer to document transform, so damage is tracked in display coordinates.
    pub world: [f64; 6],
    /// The same transform composed from the document's local matrices for v3
    /// cache reach. It cannot be reconstructed from the rounded f64 world.
    pub fixed_world: Option<alto_core::FixedAffine>,
    /// Document pixels this stroke has shown so far.
    pub damage: Option<Rect>,
}

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// `None` only for the test renderer (`headless`), which never presents.
    surface: Option<wgpu::Surface<'static>>,
    config: wgpu::SurfaceConfiguration,
    atlas: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    atlas_slots: Vec<Option<AtlasSlot>>,
    atlas_map: HashMap<ContentHash, u32>,
    cache: wgpu::Texture,
    cache_view: wgpu::TextureView,
    scratch: wgpu::Texture,
    cache_map: BTreeMap<CacheKey, Cached>,
    cache_slots: Vec<Option<CacheKey>>,
    visible_keys: HashSet<CacheKey>,
    overview_keys: HashSet<CacheKey>,
    overview_pins: HashSet<CacheKey>,
    /// Children of a v3 parent being built synchronously. Never evicted meanwhile.
    pyramid_pins: Vec<CacheKey>,
    cache_pressure: bool,
    eviction_view: Viewport,
    ideal_level: i32,
    atlas_distances: HashMap<ContentHash, f64>,
    last_view: Option<Viewport>,
    last_pending: u32,
    /// Set from the last present until its GPU work completes. At most one frame is
    /// presented ahead of the GPU: Chrome loses the device when a worker canvas is
    /// resized while presented frames pile up (crbug.com/557281760).
    present_in_flight: Arc<AtomicBool>,
    /// A frame was held back by `present_in_flight` and is still owed.
    present_deferred: bool,
    /// Pipelines were created since the host last asked. A software adapter compiles
    /// them on the GPU process before later queue work, so the next present can stay
    /// in flight for seconds without the device being lost.
    pipelines_built: bool,
    prefetch_stats: PrefetchStatus,
    prefetch_job: Option<TileJob>,
    visible_job: Option<TileJob>,
    visible_tile_jobs: bool,
    composite: wgpu::ComputePipeline,
    // Keep v3 shader compilation off v1/v2 queues, including after GPU recovery.
    composite_q14: OnceLock<wgpu::ComputePipeline>,
    downsample: wgpu::ComputePipeline,
    downsample_q14: OnceLock<wgpu::ComputePipeline>,
    integer_cache: bool,
    present_q14: OnceLock<wgpu::RenderPipeline>,
    present: wgpu::RenderPipeline,
    brush: wgpu::ComputePipeline,
    sampler: wgpu::Sampler,
    /// Kept for the device's lifetime. On WebGPU these hold the `GPU` and `GPUAdapter`
    /// objects; CI lost the device with "A valid external Instance reference no longer
    /// exists" while only the device was referenced.
    _instance: wgpu::Instance,
    _adapter: wgpu::Adapter,
    pub lost: Arc<AtomicBool>,
    /// The reason and message the device was lost with, for error reports.
    lost_detail: Arc<std::sync::Mutex<String>>,
    frame: u64,
    pub stroke: Option<GpuStroke>,
    pub adapter_name: String,
    budget_bytes: u64,
    m5: Option<m5::Resources>,
    vector: Option<vector::VectorResources>,
    /// Small text through the glyph atlas (true) or always through the vector path.
    glyph_atlas: bool,
    profiling: bool,
    /// Some while the host decodes job inputs on other threads.
    decoder: Option<Decoder>,
    pub(crate) retired: Retired,
    governor: Arc<Governor>,
    readback: Option<readback::SharedPool>,
    readback_fault: Option<u64>,
    presentation: presentation::Presentation,
    /// Monotonic milliseconds. Fixed for the renderer's lifetime; see `set_clock`.
    clock: fn() -> f64,
    profile: RenderProfile,
}
fn rgba_pixels(tile: &TileData) -> std::borrow::Cow<'_, [u8]> {
    if tile.format == alto_core::PixelFormat::R8 {
        std::borrow::Cow::Owned(tile.pixels.iter().flat_map(|v| [*v, *v, *v, 255]).collect())
    } else {
        std::borrow::Cow::Borrowed(&tile.pixels)
    }
}
fn texture(
    device: &wgpu::Device,
    label: &str,
    layers: u32,
    usage: wgpu::TextureUsages,
) -> Result<wgpu::Texture> {
    memory::create_texture(
        device,
        &wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: TILE,
                height: TILE,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage,
            view_formats: &[],
        },
        if label == "content atlas" || label == "display tiles" {
            Class::Fixed
        } else {
            Class::Stroke
        },
    )
}
fn array_view(t: &wgpu::Texture) -> wgpu::TextureView {
    t.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    })
}
/// GPU objects to destroy once the commands using them are submitted. On WebGPU,
/// dropping a wgpu object does nothing: the JS object, and the memory behind it, lives
/// until JS collects it. Short-lived buffers and replaced textures are destroyed here
/// instead; WebGPU defers the actual release until submitted work no longer uses them.
#[derive(Clone, Copy)]
enum MemoryClass {
    Fixed,
    Effects,
    Stroke,
    Vector,
}
impl MemoryClass {
    fn charge(self, b: &mut GpuBytes, n: u64) {
        match self {
            Self::Fixed => b.fixed += n,
            Self::Effects => b.effects += n,
            Self::Stroke => b.stroke += n,
            Self::Vector => b.vector += n,
        }
    }
}
#[derive(Default)]
pub(crate) struct Retired {
    buffers: Vec<(wgpu::Buffer, MemoryClass)>,
    textures: Vec<(wgpu::Texture, MemoryClass)>,
}
impl Retired {
    pub(crate) fn buffer(&mut self, b: &wgpu::Buffer) {
        self.buffers.push((b.clone(), MemoryClass::Vector));
    }
    pub(crate) fn texture(&mut self, t: wgpu::Texture) {
        self.textures.push((t, MemoryClass::Vector));
    }
    pub(crate) fn effect_texture(&mut self, t: wgpu::Texture) {
        self.textures.push((t, MemoryClass::Effects));
    }
    pub(crate) fn bytes(&self) -> GpuBytes {
        let mut b = GpuBytes::default();
        for (buffer, class) in &self.buffers {
            class.charge(&mut b, buffer.size());
        }
        for (t, class) in &self.textures {
            let n = t.width() as u64
                * t.height() as u64
                * t.depth_or_array_layers() as u64
                * t.sample_count() as u64
                * t.format().block_copy_size(None).unwrap_or(4) as u64;
            class.charge(&mut b, n);
        }
        b.retired_fixed = b.fixed;
        b.retired = b.total();
        b
    }
    fn classified_buffer(&mut self, b: &wgpu::Buffer, class: MemoryClass) {
        self.buffers.push((b.clone(), class));
    }
    pub(crate) fn take(&mut self, other: &mut Retired) {
        self.buffers.append(&mut other.buffers);
        self.textures.append(&mut other.textures);
    }
    /// Only after the submit that used them.
    pub(crate) fn release(&mut self) {
        for (b, _) in self.buffers.drain(..) {
            memory::destroy_buffer(&b, false);
        }
        for (t, _) in self.textures.drain(..) {
            memory::destroy_texture(&t, false);
        }
    }
}
/// A buffer filled through the queue. Not mapped at creation: on WebGPU a mapped
/// buffer needs shared memory that lives until JS collects the GPUBuffer (dropping
/// it in Rust does nothing there), and creating one per composed tile ran the
/// renderer out of mappings on slow machines ("size (144) is too large").
fn buffer<T: Pod>(
    d: &wgpu::Device,
    q: &wgpu::Queue,
    label: &str,
    data: &[T],
    usage: wgpu::BufferUsages,
) -> Result<wgpu::Buffer> {
    // WGSL runtime arrays still require space for one element when their logical length is zero.
    let zero = T::zeroed();
    let bytes: &[u8] = if data.is_empty() {
        bytemuck::bytes_of(&zero)
    } else {
        bytemuck::cast_slice(data)
    };
    let size = (bytes.len() as u64).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
    let class = if label.starts_with("vector") || label.starts_with("glyph") {
        Class::Vector
    } else if label.starts_with("M5") {
        Class::Effects
    } else {
        Class::Fixed
    };
    let b = memory::create_buffer(
        d,
        &wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
        class,
        class == Class::Fixed,
    )?;
    if bytes.len() as u64 == size {
        q.write_buffer(&b, 0, bytes);
    } else {
        let mut padded = bytes.to_vec();
        padded.resize(size as usize, 0);
        q.write_buffer(&b, 0, &padded);
    }
    Ok(b)
}
fn compute(d: &wgpu::Device, label: &str, src: &str) -> wgpu::ComputePipeline {
    let shader = d.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(src.into()),
    });
    d.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    })
}
fn integer_present(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("integer present"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/present-q14.wgsl").into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("integer present"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}
fn mode(m: RenderBlend) -> f32 {
    m as u32 as f32
}

/// Q14 quantize of a color-adjust setting. 1.0 is 16384. The value is clamped
/// to [-1, 4], and `value * 16384` is rounded half away from zero exactly
/// (`q14_round`, integer). A value that is not finite is 0.
/// Render version 3 stores these Q14 integers in the mode 17 uniform.
pub(crate) fn color_adjust_q14(value: f32) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    crate::q14_round(value.clamp(-1.0, 4.0))
}

/// Q14 quantize of a levels gamma, clamped to the document range [0.1, 10]
/// rather than the [-1, 4] of [`color_adjust_q14`]. A value that is not finite is 1.
pub(crate) fn levels_gamma_q14(value: f32) -> i32 {
    if !value.is_finite() {
        return 1 << 14;
    }
    crate::q14_round(value.clamp(0.1, 10.0))
}

/// Where a v3 stage reads the atlas: the mask of modes 21 to 23, or the raster
/// tiles of mode 18. The output ROI uses integer document pixels for scale >= 1,
/// or grid units with a negative reciprocal scale for levels -6..-1. The Q16
/// inverse and its i64 Q16 translation bits (`effects-q14.wgsl` `Config.grid`
/// rows 3 to 5). The positions are always `SAMPLE_INTEGER` (`sample.rs`), and the
/// texel is the nearest one (`effects-q14.wgsl` `lookup`).
pub(crate) struct MaskSampler {
    pub origin: [i32; 3],
    pub inverse: [i32; 4],
    pub translation: [u32; 4],
}

/// An f32 that is exactly an `i32`.
fn exact_i32(v: f32) -> Option<i32> {
    (v.fract() == 0.0 && (-2_147_483_648.0..2_147_483_648.0).contains(&v)).then_some(v as i32)
}

/// The sampler for a v3 mask with document-to-pixel `transform`, read at an
/// output ROI with origin `origin` and level scale `scale`. Every v3 mask takes
/// the Q16 lookup (`SAMPLE_INTEGER`), axis-aligned unit inverses included; there
/// is no f32 lookup. The Q16 inverse is `integer_inverse`, and the Q8 pixel
/// center needs an origin and a scale that are exact integers, with the scale at
/// least 1. `None` when any of that fails or `transform` is singular: the host
/// refuses the render (DQ3-4).
#[cfg(test)]
pub(crate) fn mask_sampler(
    transform: [f64; 6],
    origin: [f32; 2],
    scale: f32,
) -> Option<MaskSampler> {
    let x = exact_i32(origin[0])?;
    let y = exact_i32(origin[1])?;
    let scale = exact_i32(scale).filter(|s| *s >= 1)?;
    let q = crate::sample::integer_inverse(inverse(transform)?)?;
    Some(MaskSampler {
        origin: [x, y, scale],
        inverse: q.m,
        translation: crate::sample::pack_wide(q.tx, q.ty),
    })
}

pub(crate) fn fixed_mask_sampler(
    transform: &alto_core::FixedAffine,
    origin: [i32; 3],
) -> Option<MaskSampler> {
    if origin[2] == 0
        || (origin[2] < 0 && !matches!(origin[2].checked_neg()?, 2 | 4 | 8 | 16 | 32 | 64))
    {
        return None;
    }
    let q = crate::sample::fixed_inverse(transform)??;
    Some(MaskSampler {
        origin,
        inverse: q.m,
        translation: crate::sample::pack_wide(q.tx, q.ty),
    })
}

/// The Q16 inverse of a [`MaskSampler`], unpacked.
pub(crate) fn sampler_inverse(s: &MaskSampler) -> crate::sample::IntegerInverse {
    let wide = |lo: u32, hi: u32| ((u64::from(hi) << 32) | u64::from(lo)) as i64;
    let t = s.translation;
    crate::sample::IntegerInverse {
        m: s.inverse,
        tx: wide(t[0], t[1]),
        ty: wide(t[2], t[3]),
    }
}

/// What the integer lookup (`pixel_q8`, `coord_q8`) of the pixels
/// `[x0, x1) x [y0, y1)` of a target at `origin` and level `scale` reads,
/// evaluated exactly in i128 at the four corners. The Q24 sum is affine in the
/// pixel index, and the half-away rounding and the floors are monotone, so the
/// corners bound every pixel.
///
/// i128 bound (Linus 05:25). The inputs are a Q16 coefficient `|m| < 2^31`
/// (`quantize_coeff`), a Q16 translation `|t| < 2^53` (`quantize_translation`),
/// and from the callers an origin `|o| < 2^70` (a tile key below 2^31 times a
/// tile of at most `256 << 30` px, `invalidate_reach`), a pixel index
/// `0 <= i < 2^39` and a scale `1 <= s < 2^31`. Then a pixel center
/// `o * 256 + (i * 256 + 128) * s` is below `2^78 + 2^79 < 2^80`, each product
/// `m * center` below `2^111`, `t * 256` below `2^61`, and the Q24 sum below
/// `2^113`: far inside i128. Every step is still checked: an input outside
/// those bounds gives `None`, which `v3_reach` refuses (GpuUnsupported) and
/// `invalidate_reach` treats as reaching the tile.
pub(crate) struct ExactReach {
    /// Inclusive source tile keys `[min x, min y, max x, max y]`, with `reach`
    /// more texels on each axis (1 for the 4-tap, 0 for nearest).
    pub keys: [i128; 4],
    /// Whether every corner's Q8 pixel center fits i32 (the shader's `pixel_q8`).
    pub centers_fit: bool,
    /// Per axis, whether some corner's source Q8 leaves i32: the shader drops
    /// (makes transparent) exactly those pixels, since its 64-bit sums cannot
    /// overflow first (each product is below 2^62, the translation below 2^61).
    pub dropped: [bool; 2],
}

pub(crate) fn exact_reach(
    inv: &crate::sample::IntegerInverse,
    origin: [i64; 2],
    scale: i64,
    span: [i64; 4],
    reach: i32,
) -> Option<ExactReach> {
    exact_reach_q8(
        inv,
        origin.map(|o| i128::from(o) * 256),
        i128::from(scale) * 256,
        span,
        reach,
    )
}

fn exact_reach_q8(
    inv: &crate::sample::IntegerInverse,
    origin: [i128; 2],
    step: i128,
    span: [i64; 4],
    reach: i32,
) -> Option<ExactReach> {
    let center = |o: i128, i: i64| {
        o.checked_add(i128::from(i).checked_mul(step)?)?
            .checked_add(step / 2)
    };
    let source = |m0: i32, px: i128, m1: i32, py: i128, t: i64| {
        let sum = i128::from(m0)
            .checked_mul(px)?
            .checked_add(i128::from(m1).checked_mul(py)?)?
            .checked_add(i128::from(t).checked_mul(256)?)?;
        let half = 1i128 << 15;
        Some(
            (if sum >= 0 {
                sum.checked_add(half)?
            } else {
                sum.checked_sub(half)?
            }) >> 16,
        )
    };
    let i32_range = i128::from(i32::MIN)..=i128::from(i32::MAX);
    let mut out = ExactReach {
        keys: [i128::MAX, i128::MAX, i128::MIN, i128::MIN],
        centers_fit: true,
        dropped: [false; 2],
    };
    let [x0, y0, x1, y1] = span;
    for (x, y) in [(x0, y0), (x1 - 1, y0), (x0, y1 - 1), (x1 - 1, y1 - 1)] {
        let (px, py) = (center(origin[0], x)?, center(origin[1], y)?);
        out.centers_fit &= i32_range.contains(&px) && i32_range.contains(&py);
        let q = [
            source(inv.m[0], px, inv.m[2], py, inv.tx)?,
            source(inv.m[1], px, inv.m[3], py, inv.ty)?,
        ];
        for (axis, q) in q.into_iter().enumerate() {
            out.dropped[axis] |= !i32_range.contains(&q);
            let texel = q >> 8;
            out.keys[axis] = out.keys[axis].min(texel >> 8);
            out.keys[axis + 2] = out.keys[axis + 2].max((texel + i128::from(reach)) >> 8);
        }
    }
    Some(out)
}

/// The tiles a v3 atlas read (a plain draw, mode 18's raster, a mask) needs on
/// the pixels `span` of a target at `origin` and `scale` (Linus 04:40, DQ3-4):
/// `Ok(None)` when no tile of `keys` (the source's tiles) is in the
/// [`exact_reach`] range, so the read contributes nothing and is skipped;
/// otherwise the inclusive key range to select. `Err` only when pixels that do
/// reach a tile cannot be computed exactly by the shader: a Q8 pixel center
/// outside i32 (a tile origin at 2^23 px or past), or a pixel the shader drops
/// for a source Q8 outside i32 that could read a listed tile (the tiles in
/// range must lie in texels `-2^23 + reach ..= 2^23 - 1` on every axis that
/// drops a pixel).
pub(crate) fn v3_reach(
    inv: &crate::sample::IntegerInverse,
    origin: [i64; 2],
    scale: i64,
    span: [i64; 4],
    reach: i32,
    keys: impl Iterator<Item = TileKey>,
) -> std::result::Result<Option<[i32; 4]>, ()> {
    v3_reach_q8(
        inv,
        origin.map(|o| i128::from(o) * 256),
        i128::from(scale) * 256,
        span,
        reach,
        keys,
    )
}

pub(crate) fn v3_reach_q8(
    inv: &crate::sample::IntegerInverse,
    origin: [i128; 2],
    step: i128,
    span: [i64; 4],
    reach: i32,
    keys: impl Iterator<Item = TileKey>,
) -> std::result::Result<Option<[i32; 4]>, ()> {
    if span[0] >= span[2] || span[1] >= span[3] {
        return Ok(None);
    }
    let r = exact_reach_q8(inv, origin, step, span, reach).ok_or(())?;
    let mut present = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    for k in keys {
        let (x, y) = (i128::from(k.tx), i128::from(k.ty));
        if (r.keys[0]..=r.keys[2]).contains(&x) && (r.keys[1]..=r.keys[3]).contains(&y) {
            present = [
                present[0].min(k.tx),
                present[1].min(k.ty),
                present[2].max(k.tx),
                present[3].max(k.ty),
            ];
        }
    }
    if present[0] > present[2] {
        return Ok(None);
    }
    if !r.centers_fit {
        return Err(());
    }
    let low = -(1i64 << 23) + i64::from(reach);
    let high = (1i64 << 23) - 1;
    for axis in 0..2 {
        let (first, last) = (
            i64::from(present[axis]) * 256,
            i64::from(present[axis + 2]) * 256 + 255,
        );
        if r.dropped[axis] && (first < low || last > high) {
            return Err(());
        }
    }
    let clamp = |v: i128| v.clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as i32;
    Ok(Some(r.keys.map(clamp)))
}

/// The integer texel map of one input of a v3 mask stage (`effects-q14.wgsl`
/// `input_texel`). `out` and `input` are ROIs as `[x, y, scale]`, and `size` is
/// the output size in pixels. Equal ROIs map pixel `i` to texel `i`: `[0; 4]`.
/// Otherwise the texel of output pixel `i` is `floor((p - in_x) / in_scale)` at
/// the center `p = out_x + (i + 1/2) * out_scale`. Both ROIs are converted to
/// the finer common power-of-two grid, so this also covers levels -6..-1.
/// The doubled numerator, step and divisor must fit the shader's i32 words.
pub(crate) fn input_map(out: [f32; 3], size: [u32; 2], input: [f32; 3]) -> Option<[i32; 4]> {
    if out == input {
        return Some([0; 4]);
    }
    let [ox, oy, out_level] = crate::raster_q8::roi_grid(out[0], out[1], out[2])?;
    let [ix, iy, in_level] = crate::raster_q8::roi_grid(input[0], input[1], input[2])?;
    let common = out_level.min(in_level);
    let out_step = 1_i128.checked_shl(u32::try_from(out_level - common).ok()?)?;
    let in_step = 1_i128.checked_shl(u32::try_from(in_level - common).ok()?)?;
    let step = 2_i128.checked_mul(out_step)?;
    let denominator = 2_i128.checked_mul(in_step)?;
    let numerator = |o: i64, i: i64, len: u32| -> Option<i32> {
        let base = (i128::from(o).checked_mul(out_step)?)
            .checked_sub(i128::from(i).checked_mul(in_step)?)?
            .checked_mul(2)?
            .checked_add(out_step)?;
        let last = i128::from(len.max(1) - 1).checked_mul(step)?;
        i32::try_from(last).ok()?;
        i32::try_from(base.checked_add(last)?).ok()?;
        i32::try_from(base).ok()
    };
    Some([
        numerator(ox, ix, size[0])?,
        numerator(oy, iy, size[1])?,
        i32::try_from(step).ok()?,
        i32::try_from(denominator).ok()?,
    ])
}

/// The pixel-index rectangle `[x0, y0, x1, y1)` of a 256² tile whose pixel
/// centers lie on the canvas, for `effects-resolve-q14.wgsl`. The tile has
/// integer origin `origin` and level scale `scale`; pixel `i` is on the canvas
/// when `0 <= origin + (i + 1/2) * scale < end`, the test of the v1/v2 resolve,
/// here doubled and exact in i64. An empty rectangle is all zero.
pub(crate) fn resolve_span(origin: [i64; 2], scale: i64, end: [i64; 2]) -> [u32; 4] {
    let axis = |o: i64, e: i64| {
        let inside = |i: i64| (0..2 * e).contains(&(2 * o + (2 * i + 1) * scale));
        let lo = (0..256).find(|&i| inside(i)).unwrap_or(256);
        let hi = (lo..256)
            .take_while(|&i| inside(i))
            .last()
            .map_or(lo, |i| i + 1);
        (lo as u32, hi as u32)
    };
    let (x0, x1) = axis(origin[0], end[0]);
    let (y0, y1) = axis(origin[1], end[1]);
    if x0 == x1 || y0 == y1 {
        return [0; 4];
    }
    [x0, y0, x1, y1]
}

/// How a version 3 plain draw (`integer_composite_shader`) reads its tiles in
/// the level-0 display tile `key`: the Q16 inverse, the filter
/// (`SAMPLE_NEAREST` for an axis-aligned unit inverse, `axis_unit_q16`, else
/// the integer 4-tap `SAMPLE_INTEGER`), and the inclusive tile keys
/// `[min tx, min ty, max tx, max ty]` its on-canvas pixels read (`None`: no
/// pixel of the tile is on the canvas). There is no f32 fetch.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PlainDraw {
    pub inverse: crate::sample::IntegerInverse,
    pub sample: u32,
    pub keys: Option<[i32; 4]>,
}

impl PlainDraw {
    /// Whether the draw reads source tile `k`.
    fn reads(&self, k: TileKey) -> bool {
        self.keys
            .is_some_and(|[x0, y0, x1, y1]| (x0..=x1).contains(&k.tx) && (y0..=y1).contains(&k.ty))
    }
    /// The draw command. `submit_composite` turns the opacity and the mode into
    /// their integers.
    fn command(&self, opacity: f32, blend: RenderBlend, start: u32, count: u32) -> Command {
        Command {
            inv0: self.inverse.m.map(|v| f32::from_bits(v.cast_unsigned())),
            inv1: [0.0, 0.0, opacity, mode(blend)],
            info: [0, start, count, self.sample],
            trans: crate::sample::pack_wide(self.inverse.tx, self.inverse.ty),
        }
    }
}

/// The [`PlainDraw`] of a draw with an integer Q16 inverse and source tiles `tiles`
/// in the level-0 display tile `key` of a canvas ending at `end`. The pixels are
/// those of `resolve_span`, and the tiles are chosen by [`v3_reach`] (the 4-tap
/// also reads the next texel on each axis): a draw that reaches none of its
/// tiles has `keys: None` and is skipped. Refused (`GpuUnsupported`, DQ3-4) when
/// pixels that reach a tile have no exact
/// shader value; never sampled in f32.
pub(crate) fn plain_draw_v3(
    inverse: crate::sample::IntegerInverse,
    key: CacheKey,
    end: [i32; 2],
    tiles: impl Iterator<Item = TileKey>,
) -> Result<PlainDraw> {
    let unsupported = |what: &str| RenderError::GpuUnsupported(format!("v3 draw {what}"));
    if key.0 != 0 {
        return Err(RenderError::Internal(
            "v3 plain tile above level 0 (the pyramid builds those)".into(),
        ));
    }
    let origin = [key.1, key.2].map(|k| i64::from(k) * 256);
    let nearest = crate::sample::axis_unit_q16(inverse.m);
    let span = resolve_span(origin, 1, [i64::from(end[0]), i64::from(end[1])]).map(i64::from);
    let keys = v3_reach(&inverse, origin, 1, span, i32::from(!nearest), tiles)
        .map_err(|()| unsupported("ROI has no integer tile range"))?;
    Ok(PlainDraw {
        inverse,
        sample: if nearest {
            crate::sample::SAMPLE_NEAREST
        } else {
            crate::sample::SAMPLE_INTEGER
        },
        keys,
    })
}

/// The integer uniform of a v3 M5 stage, `effects-q14.wgsl` `Config.grid`:
/// row 0 the Q14 settings, rows 1 and 2 the `input_map` of inputs `a` and `b`,
/// rows 3 to 5 the atlas lookup: the mask of modes 21 to 23 or the raster of
/// mode 18 (zero without one; `mask_sampler` builds both). The rows go into the
/// `curve` slots of the M5 uniform, which no v3 stage reads otherwise. That
/// uniform is shared with versions 1 and 2 and its Rust type is `[f32; 64]`,
/// so the integers travel as f32 bit patterns (`f32::from_bits`, a copy). The
/// shader declares the slots `array<vec4i, 16>`; no float is loaded or computed.
pub(crate) fn mask_grid(
    settings: [i32; 4],
    a: [i32; 4],
    b: [i32; 4],
    sampler: Option<&MaskSampler>,
) -> [f32; 64] {
    let mut rows = [[0u32; 4]; 16];
    rows[0] = settings.map(i32::cast_unsigned);
    rows[1] = a.map(i32::cast_unsigned);
    rows[2] = b.map(i32::cast_unsigned);
    if let Some(s) = sampler {
        rows[3] = [s.origin[0], s.origin[1], s.origin[2], 0].map(i32::cast_unsigned);
        rows[4] = s.inverse.map(i32::cast_unsigned);
        rows[5] = s.translation;
    }
    let mut out = [0.0; 64];
    for (slot, bits) in out.iter_mut().zip(rows.as_flattened()) {
        *slot = f32::from_bits(*bits);
    }
    out
}

/// The feature that render version 3 has no integer definition for, or `None`.
/// Version 3 draws plain scenes with the integer tile compositor and M5 stages
/// for all document effects, adjustments, masks, filled and stroked vectors,
/// clipping and baked text. Direct layout runs are an invalid v3 scene input.
/// Every node is checked, visible or not, before any tile is written.
/// `tests/e2e/cpu-composite.js` `sceneGap` mirrors this.
pub fn v3_gap(scene: &RenderScene) -> Option<&'static str> {
    let integer_effect = |e: &crate::REffect| {
        matches!(
            e.kind,
            crate::REffectKind::ColorAdjust { .. }
                | crate::REffectKind::GaussianBlur { .. }
                | crate::REffectKind::Invert
                | crate::REffectKind::Curves { .. }
                | crate::REffectKind::Levels { .. }
                | crate::REffectKind::HueSaturation { .. }
        )
    };
    for n in &scene.nodes {
        let f = &n.features;
        if !f.effects.iter().all(integer_effect) {
            return Some("effects");
        }
        match &n.content {
            // Version 3 text draws from its bake (`TextBake`); layout runs are
            // never a v3 input.
            RContent::Text(_) => return Some("text"),
            RContent::Adjustment(e) if !integer_effect(e) => {
                return Some("adjustment");
            }
            _ => {}
        }
    }
    None
}
// The renderer keeps raw matrices in its hot loops; the math is `Affine2`'s.
pub(crate) fn multiply(a: [f64; 6], b: [f64; 6]) -> [f64; 6] {
    alto_core::Affine2(a).concat(alto_core::Affine2(b)).0
}

/// A document-to-canvas matrix while rendering. Version 3 never performs
/// arithmetic on the stored f64 values after `FixedAffine::from_affine`.
#[derive(Clone, Copy)]
pub(crate) enum WorldTransform {
    Legacy([f64; 6]),
    V3(alto_core::FixedAffine),
}

impl WorldTransform {
    pub(crate) fn root(version: u32) -> Self {
        if version == 3 {
            Self::V3(alto_core::FixedAffine::IDENTITY)
        } else {
            Self::Legacy([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
        }
    }

    pub(crate) fn concat(self, local: [f64; 6]) -> Result<Self> {
        match self {
            Self::Legacy(parent) => Ok(Self::Legacy(multiply(parent, local))),
            Self::V3(parent) => {
                let local = alto_core::FixedAffine::from_affine(alto_core::Affine2(local))
                    .ok_or_else(|| {
                        RenderError::GpuUnsupported("v3 transform out of range".into())
                    })?;
                let world = parent.concat(local).ok_or_else(|| {
                    RenderError::GpuUnsupported("v3 composed transform out of range".into())
                })?;
                Ok(Self::V3(world))
            }
        }
    }

    pub(crate) fn legacy(self) -> Result<[f64; 6]> {
        match self {
            Self::Legacy(world) => Ok(world),
            Self::V3(_) => Err(RenderError::Internal(
                "v3 reached a legacy transform path".into(),
            )),
        }
    }

    pub(crate) fn fixed(self) -> Result<alto_core::FixedAffine> {
        match self {
            Self::V3(world) => Ok(world),
            Self::Legacy(_) => Err(RenderError::Internal(
                "legacy render reached a v3 transform path".into(),
            )),
        }
    }
}

pub(crate) fn v3_inverse(
    world: alto_core::FixedAffine,
) -> Result<Option<crate::sample::IntegerInverse>> {
    crate::sample::fixed_inverse(&world)
        .ok_or_else(|| RenderError::GpuUnsupported("v3 draw transform has no Q16 inverse".into()))
}
pub(crate) fn inverse(m: [f64; 6]) -> Option<[f64; 6]> {
    alto_core::Affine2(m).inverse().map(|m| m.0)
}
pub(crate) fn point(m: [f64; 6], p: [f64; 2]) -> [f64; 2] {
    alto_core::Affine2(m).apply(p)
}
/// The world transform and the key box of every source tile a stroke has
/// touched, for the version 3 integer invalidation (`invalidate_reach`).
fn stroke_reach(stroke: &GpuStroke) -> Option<(Option<alto_core::FixedAffine>, [i32; 4])> {
    let keys = stroke.tiles.keys().fold(None, |b: Option<[i32; 4]>, k| {
        Some(match b {
            None => [k.tx, k.ty, k.tx, k.ty],
            Some([x0, y0, x1, y1]) => [x0.min(k.tx), y0.min(k.ty), x1.max(k.tx), y1.max(k.ty)],
        })
    })?;
    Some((stroke.fixed_world, keys))
}
/// The canvas end `[x1, y1]` of the version 3 resolve tests (they start at 0).
fn canvas_end(scene: &RenderScene) -> [i32; 2] {
    [scene.canvas.x1, scene.canvas.y1]
}
/// Bounding box of `[x0, y0, x1, y1]` mapped through `m`, as (min, max).
fn bounds_through(m: [f64; 6], r: [f64; 4]) -> ([f64; 2], [f64; 2]) {
    let corners = [[r[0], r[1]], [r[2], r[1]], [r[0], r[3]], [r[2], r[3]]].map(|p| point(m, p));
    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    for p in corners {
        for axis in 0..2 {
            min[axis] = min[axis].min(p[axis]);
            max[axis] = max[axis].max(p[axis]);
        }
    }
    (min, max)
}
impl TileJob {
    fn new(scene: &RenderScene, key: CacheKey) -> Self {
        let s = 2f64.powi(key.0);
        Self {
            key,
            region: [
                (key.1 as f64 * 256.0 * s).max(scene.canvas.x0 as f64),
                (key.2 as f64 * 256.0 * s).max(scene.canvas.y0 as f64),
                ((key.1 + 1) as f64 * 256.0 * s).min(scene.canvas.x1 as f64),
                ((key.2 + 1) as f64 * 256.0 * s).min(scene.canvas.y1 as f64),
            ],
            stack: vec![PlanTask::Node {
                idx: scene.root,
                world: WorldTransform::root(scene.render_version),
                depth: 0,
            }],
            commands: Vec::new(),
            sources: Vec::new(),
            unique: Vec::new(),
            missing: HashSet::new(),
            cursor: 0,
            planned: false,
            retries: 0,
        }
    }
    // One scene node or one source tile per call. No store reads or GPU writes occur here.
    fn plan_one(&mut self, scene: &RenderScene, store: &dyn TileStore) -> Result<()> {
        let Some(task) = self.stack.pop() else {
            self.planned = true;
            let mut seen = HashSet::new();
            self.unique = self
                .sources
                .iter()
                .filter_map(|s| {
                    if seen.insert(s.hash) {
                        Some(s.hash)
                    } else {
                        None
                    }
                })
                .collect();
            return Ok(());
        };
        match task {
            PlanTask::Close { opacity, blend } => self.commands.push(Command {
                inv0: [0.0; 4],
                inv1: [0.0, 0.0, opacity, mode(blend)],
                info: [2, 0, 0, 0],
                trans: [0; 4],
            }),
            PlanTask::Node { idx, world, depth } => {
                let n = scene
                    .nodes
                    .get(idx)
                    .ok_or(RenderError::Internal("scene node".into()))?;
                if !n.visible || n.opacity <= 0.0 {
                    return Ok(());
                }
                let world = world.concat(n.transform)?;
                match &n.content {
                    RContent::Group { children, isolated } => {
                        let isolate =
                            *isolated || n.opacity < 1.0 || n.blend != RenderBlend::Normal;
                        if isolate && depth >= MAX_ISOLATED_GROUPS {
                            return Err(RenderError::InvalidArgument(format!(
                                "group nesting exceeds {MAX_ISOLATED_GROUPS}"
                            )));
                        }
                        if isolate {
                            self.commands.push(Command {
                                inv0: [0.0; 4],
                                inv1: [0.0; 4],
                                info: [1, 0, 0, 0],
                                trans: [0; 4],
                            });
                            self.stack.push(PlanTask::Close {
                                opacity: n.opacity,
                                blend: n.blend,
                            });
                        }
                        let mut start = 0;
                        for (i, &child) in children.iter().enumerate().rev() {
                            let cn = &scene.nodes[child];
                            if !cn.visible || cn.opacity != 1.0 || cn.blend != RenderBlend::Normal {
                                continue;
                            }
                            if let RContent::Tiles(ts) = &cn.content {
                                let child_world = world.concat(cn.transform)?;
                                // Version 3 hides lower layers only behind the tiles the
                                // integer sampler actually reads.
                                let (x0, y0, x1, y1) =
                                    if let WorldTransform::V3(fixed) = child_world {
                                        let Some(inv) = v3_inverse(fixed)? else {
                                            continue;
                                        };
                                        match plain_draw_v3(
                                            inv,
                                            self.key,
                                            canvas_end(scene),
                                            ts.iter().map(|t| t.key),
                                        )?
                                        .keys
                                        {
                                            Some([x0, y0, x1, y1]) => (x0, y0, x1 + 1, y1 + 1),
                                            None => continue,
                                        }
                                    } else {
                                        let Some(inv) = inverse(child_world.legacy()?) else {
                                            continue;
                                        };
                                        let (min, max) = bounds_through(inv, self.region);
                                        (
                                            (min[0] / 256.0).floor() as i32,
                                            (min[1] / 256.0).floor() as i32,
                                            (max[0] / 256.0).ceil() as i32,
                                            (max[1] / 256.0).ceil() as i32,
                                        )
                                    };
                                let mut opaque = x1 > x0
                                    && y1 > y0
                                    && (x1 - x0) as i64 * (y1 - y0) as i64 <= 1024;
                                for x in x0..x1 {
                                    for y in y0..y1 {
                                        let k = TileKey { tx: x, ty: y };
                                        opaque &= ts
                                            .binary_search_by_key(&k, |t| t.key)
                                            .ok()
                                            .is_some_and(|j| store.is_opaque(&ts[j].hash));
                                    }
                                }
                                if opaque {
                                    start = i;
                                    break;
                                }
                            }
                        }
                        for &child in children[start..].iter().rev() {
                            self.stack.push(PlanTask::Node {
                                idx: child,
                                world,
                                depth: depth + u32::from(isolate),
                            });
                        }
                    }
                    RContent::Tiles(tiles) => {
                        let (inv, min, max, v3) = match world {
                            WorldTransform::V3(fixed) => {
                                let Some(q) = v3_inverse(fixed)? else {
                                    return Ok(());
                                };
                                let draw = plain_draw_v3(
                                    q,
                                    self.key,
                                    canvas_end(scene),
                                    tiles.iter().map(|t| t.key),
                                )?;
                                ([0.0; 6], [0.0; 2], [0.0; 2], Some(draw))
                            }
                            WorldTransform::Legacy(world) => {
                                let Some(inv) = inverse(world) else {
                                    return Ok(());
                                };
                                let (min, max) = bounds_through(inv, self.region);
                                (inv, min, max, None)
                            }
                        };
                        let (x0, x1) = match v3 {
                            Some(d) => d.keys.map_or((0, 0), |k| (k[0], k[2] + 1)),
                            None => (
                                (min[0] / 256.0).floor() as i32,
                                (max[0] / 256.0).ceil() as i32,
                            ),
                        };
                        let next = tiles.partition_point(|t| t.key.tx < x0);
                        let end = tiles.partition_point(|t| t.key.tx < x1);
                        self.stack.push(PlanTask::Tiles {
                            idx,
                            next,
                            end,
                            inv,
                            min,
                            max,
                            start: self.sources.len() as u32,
                            v3,
                        });
                    }
                    _ => {}
                }
            }
            PlanTask::Tiles {
                idx,
                next,
                end,
                inv,
                min,
                max,
                start,
                v3,
            } => {
                let n = &scene.nodes[idx];
                let RContent::Tiles(tiles) = &n.content else {
                    return Err(RenderError::Internal("tile plan changed".into()));
                };
                if next == end {
                    if let Some(d) = v3 {
                        // A draw that reaches none of its tiles is skipped (DQ3-4).
                        if d.keys.is_some() {
                            let count = self.sources.len() as u32 - start;
                            self.commands
                                .push(d.command(n.opacity, n.blend, start, count));
                        }
                        return Ok(());
                    }
                    let packed = crate::sample::pack_inverse(inv, scene.render_version);
                    self.commands.push(Command {
                        inv0: packed.inv0,
                        inv1: [
                            packed.translation[0],
                            packed.translation[1],
                            n.opacity,
                            mode(n.blend),
                        ],
                        info: [
                            0,
                            start,
                            self.sources.len() as u32 - start,
                            packed.sample_mode,
                        ],
                        trans: packed.trans,
                    });
                } else {
                    let t = &tiles[next];
                    let k = t.key;
                    let keep = match v3 {
                        Some(d) => d.reads(k),
                        None => {
                            (k.tx as f64 * 256.0) < max[0]
                                && ((k.tx + 1) as f64 * 256.0) > min[0]
                                && (k.ty as f64 * 256.0) < max[1]
                                && ((k.ty + 1) as f64 * 256.0) > min[1]
                        }
                    };
                    if keep {
                        self.sources.push(PlannedSource {
                            xy: [k.tx, k.ty],
                            hash: t.hash,
                        });
                    }
                    self.stack.push(PlanTask::Tiles {
                        idx,
                        next: next + 1,
                        end,
                        inv,
                        min,
                        max,
                        start,
                        v3,
                    });
                }
            }
        }
        Ok(())
    }
}
impl Renderer {
    pub async fn new(
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        budget_mb: u32,
    ) -> Result<Self> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            })
            .await
            .map_err(|e| RenderError::GpuUnsupported(e.to_string()))?;
        Self::with_adapter(instance, adapter, Some(surface), width, height, budget_mb).await
    }
    /// A renderer without a surface, for tests that compose display tiles and
    /// read them back (`read_tiles`). It cannot present.
    #[cfg(test)]
    pub(crate) async fn headless(instance: &wgpu::Instance, budget_mb: u32) -> Result<Self> {
        let adapter = instance
            .request_adapter(&Default::default())
            .await
            .map_err(|e| RenderError::GpuUnsupported(e.to_string()))?;
        Self::with_adapter(instance, adapter, None, 256, 256, budget_mb).await
    }
    async fn with_adapter(
        instance: &wgpu::Instance,
        adapter: wgpu::Adapter,
        surface: Option<wgpu::Surface<'static>>,
        width: u32,
        height: u32,
        budget_mb: u32,
    ) -> Result<Self> {
        let (slots, display_layers) =
            cache_layers(budget_mb, adapter.limits().max_texture_array_layers)?;
        let limits = wgpu::Limits {
            max_texture_array_layers: slots.max(display_layers),
            ..Default::default()
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: limits,
                ..Default::default()
            })
            .await
            .map_err(|e| RenderError::Internal(e.to_string()))?;
        let governor = Governor::new(
            budget_mb as u64 * 1048576,
            readback::capacity(budget_mb as u64 * 1048576),
        );
        memory::install(&device, &governor);
        let readback = readback::Pool::new(&device, &governor).await?;
        let config = match &surface {
            Some(surface) => {
                let mut config = surface
                    .get_default_config(&adapter, width.max(1), height.max(1))
                    .ok_or(RenderError::GpuUnsupported("no surface format".into()))?;
                if let Some(f) = surface
                    .get_capabilities(&adapter)
                    .formats
                    .iter()
                    .find(|f| !f.is_srgb())
                {
                    config.format = *f;
                }
                surface.configure(&device, &config);
                config
            }
            None => wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: wgpu::TextureFormat::Rgba8Unorm,
                width: width.max(1),
                height: height.max(1),
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
                view_formats: vec![],
                color_space: Default::default(),
            },
        };
        let lost = Arc::new(AtomicBool::new(false));
        let lost_detail = Arc::new(std::sync::Mutex::new(String::new()));
        let (flag, detail) = (lost.clone(), lost_detail.clone());
        device.set_device_lost_callback(move |reason, message| {
            if let Ok(mut d) = detail.lock() {
                *d = format!("{reason:?}: {message}");
            }
            flag.store(true, Ordering::Relaxed);
        });
        let (flag, detail) = (lost.clone(), lost_detail.clone());
        device.on_uncaptured_error(Arc::new(move |e| {
            if let Ok(mut d) = detail.lock() {
                *d = format!("uncaptured GPU error: {e}");
            }
            flag.store(true, Ordering::Release);
        }));
        let atlas = texture(
            &device,
            "content atlas",
            slots,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        )?;
        let cache = texture(
            &device,
            "display tiles",
            display_layers,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        )?;
        let scratch = memory::create_texture(
            &device,
            &wgpu::TextureDescriptor {
                label: Some("pyramid input"),
                size: wgpu::Extent3d {
                    width: 512,
                    height: 512,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            },
            Class::Fixed,
        )?;
        let atlas_view = array_view(&atlas);
        let cache_view = array_view(&cache);
        let downsample = compute(
            &device,
            "display pyramid",
            include_str!("../shaders/downsample.wgsl"),
        );
        let composite_src = crate::shader_with_blend(include_str!("../shaders/composite.wgsl"));
        let composite = compute(&device, "tile compositor", &composite_src);
        let brush = compute(
            &device,
            "brush coverage",
            include_str!("../shaders/brush.wgsl"),
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("present"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/present.wgsl").into()),
        });
        let present = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("present"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Self {
            device,
            queue,
            surface,
            config,
            atlas,
            atlas_view,
            atlas_slots: (0..slots).map(|_| None).collect(),
            atlas_map: HashMap::new(),
            cache,
            cache_view,
            scratch,
            cache_map: BTreeMap::new(),
            cache_slots: vec![None; display_layers as usize],
            visible_keys: HashSet::new(),
            overview_keys: HashSet::new(),
            overview_pins: HashSet::new(),
            pyramid_pins: Vec::new(),
            cache_pressure: false,
            eviction_view: Viewport::default(),
            ideal_level: 0,
            atlas_distances: HashMap::new(),
            last_view: None,
            last_pending: 0,
            present_in_flight: Arc::new(AtomicBool::new(false)),
            present_deferred: false,
            pipelines_built: true,
            prefetch_stats: PrefetchStatus::default(),
            prefetch_job: None,
            visible_job: None,
            visible_tile_jobs: true,
            composite,
            composite_q14: OnceLock::new(),
            downsample,
            downsample_q14: OnceLock::new(),
            integer_cache: false,
            present_q14: OnceLock::new(),
            present,
            brush,
            sampler,
            _instance: instance.clone(),
            _adapter: adapter.clone(),
            lost,
            lost_detail,
            frame: 0,
            stroke: None,
            adapter_name: adapter.get_info().name,
            budget_bytes: budget_mb as u64 * 1048576,
            m5: None,
            vector: None,
            glyph_atlas: true,
            profiling: false,
            decoder: None,
            retired: Retired::default(),
            governor,
            readback: Some(readback.shared()),
            readback_fault: None,
            presentation: Default::default(),
            clock: || 0.0,
            profile: RenderProfile::default(),
        })
    }
    pub fn resize(&mut self, w: u32, h: u32) {
        if self.config.width != w.max(1) || self.config.height != h.max(1) {
            self.cancel_prefetch();
            self.presentation.resized();
            self.config.width = w.max(1);
            self.config.height = h.max(1);
            if let Some(surface) = &self.surface {
                surface.configure(&self.device, &self.config);
            }
        }
    }
    pub fn vector_stats(&self) -> (u64, u64, usize, u64) {
        self.vector.as_ref().map_or((0, 0, 0, 0), |v| {
            let (h, m, n) = v.counts();
            (h, m, n, v.bytes())
        })
    }
    pub fn glyph_stats(&self) -> crate::glyph_atlas::AtlasStats {
        self.vector
            .as_ref()
            .map_or_else(Default::default, |v| v.glyph_stats())
    }
    pub fn glyph_atlas(&self) -> bool {
        self.glyph_atlas
    }
    pub fn set_profiling(&mut self, enabled: bool) {
        self.profiling = enabled;
    }
    /// Resumable jobs ask the host to decode their inputs instead of decoding inline.
    /// Turning it off forgets outstanding requests; inline decoding takes them over.
    pub fn set_parallel_decode(&mut self, enabled: bool) {
        if enabled != self.decoder.is_some() {
            self.decoder = enabled.then(Decoder::default);
        }
    }
    /// Up to `max` inputs to decode, with their compressed bytes and whether the
    /// content address must be checked. A damaged object fails its job.
    pub fn take_decode_requests(
        &mut self,
        max: usize,
    ) -> Vec<(ContentHash, Arc<alto_raster::StoredTile>)> {
        let Some(d) = self.decoder.as_mut() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while out.len() < max
            && let Some(hash) = d.queued.pop_front()
        {
            if let Some(object) = d.in_flight.get(&hash) {
                out.push((hash, object.clone()));
            }
        }
        out
    }
    /// Pixels the host decoded for a request. `checked`: it verified the content
    /// address. Answers to requests no job is waiting for any more are dropped.
    pub fn provide_decoded(
        &mut self,
        hash: ContentHash,
        pixels: &[u8],
        checked: bool,
    ) -> Result<()> {
        let Some(d) = self.decoder.as_mut() else {
            return Ok(());
        };
        let Some(object) = d.in_flight.remove(&hash) else {
            return Ok(());
        };
        if checked {
            object.mark_verified();
        }
        d.decoded += 1;
        if self.atlas_map.contains_key(&hash) {
            return Ok(());
        }
        let expected = 256 * 256 * object.format.bytes_per_pixel();
        if pixels.len() != expected {
            d.failed = Some(hash);
            return Ok(());
        }
        // Inputs the waiting jobs already have must stay until those jobs compose.
        let pinned: HashSet<u32> = [&self.visible_job, &self.prefetch_job]
            .into_iter()
            .flatten()
            .flat_map(|j| j.unique.iter())
            .filter_map(|h| self.atlas_map.get(h).copied())
            .collect();
        let slot = self.free_atlas_slot(&pinned)?;
        let rgba: std::borrow::Cow<'_, [u8]> = if object.format == alto_core::PixelFormat::R8 {
            std::borrow::Cow::Owned(pixels.iter().flat_map(|v| [*v, *v, *v, 255]).collect())
        } else {
            std::borrow::Cow::Borrowed(pixels)
        };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.atlas,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: slot as u32,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &rgba,
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
        if self.profiling {
            self.profile.uploads += 1;
            self.profile.upload_bytes += TILE_RGBA_BYTES;
        }
        self.atlas_slots[slot] = Some(AtlasSlot {
            hash,
            used: self.frame,
        });
        self.atlas_map.insert(hash, slot as u32);
        Ok(())
    }
    /// The host could not decode a request, or the result failed its check.
    pub fn decode_failed(&mut self, hash: ContentHash) {
        if let Some(d) = self.decoder.as_mut() {
            d.in_flight.remove(&hash);
            d.failed = Some(hash);
        }
    }
    /// Inputs decoded on other threads so far.
    pub fn decoded_elsewhere(&self) -> u64 {
        self.decoder.as_ref().map_or(0, |d| d.decoded)
    }
    /// The time source for frame budgets and profiles. Set once after construction.
    pub fn set_clock(&mut self, clock: fn() -> f64) {
        self.clock = clock;
    }
    /// Switches small text between the glyph atlas and the vector path; display tiles
    /// are rebuilt.
    pub fn set_glyph_atlas(&mut self, enabled: bool) {
        if self.glyph_atlas != enabled {
            self.glyph_atlas = enabled;
            self.invalidate(None);
        }
    }
    pub fn destroy_device(&self) {
        self.device.destroy();
    }
    /// Why the device was lost, as the browser reported it; empty while it is not.
    pub fn lost_detail(&self) -> String {
        self.lost_detail
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default()
    }
    /// True once after pipelines were created: at device creation, on the switch
    /// to the v3 integer cache, when M5 or vector resources are first built, and
    /// when the dense vector pipelines are built.
    pub fn take_pipelines_built(&mut self) -> bool {
        std::mem::take(&mut self.pipelines_built)
    }
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }
    /// Allows a controlled comparison with the synchronous reference path.
    pub fn set_visible_tile_jobs(&mut self, enabled: bool) {
        if self.visible_tile_jobs != enabled {
            self.visible_tile_jobs = enabled;
            self.invalidate(None);
        }
    }
    // Validate before cache access or command construction. Every v3 scene that
    // passes writes integer bytes, plain or M5, so v3 always uses the uint cache.
    fn prepare_scene(&mut self, scene: &RenderScene) -> Result<()> {
        if scene.render_version == 3 && scene.nodes.iter().any(|n| n.blend as u32 >= 17) {
            return Err(RenderError::InvalidArgument(
                "v3 unspecified blend mode".into(),
            ));
        }
        if scene.render_version == 3
            && let Some(gap) = v3_gap(scene)
        {
            return Err(RenderError::GpuUnsupported(format!(
                "v3 {gap} has no integer definition yet"
            )));
        }
        let integer = scene.render_version == 3;
        if integer == self.integer_cache {
            return Ok(());
        }
        let format = if integer {
            wgpu::TextureFormat::Rgba8Uint
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let cache = memory::create_texture(
            &self.device,
            &wgpu::TextureDescriptor {
                label: Some("display tiles"),
                size: self.cache.size(),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: self.cache.usage(),
                view_formats: &[],
            },
            Class::Fixed,
        )?;
        let scratch = memory::create_texture(
            &self.device,
            &wgpu::TextureDescriptor {
                label: Some("pyramid input"),
                size: self.scratch.size(),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: self.scratch.usage(),
                view_formats: &[],
            },
            Class::Fixed,
        )?;
        self.reset_render_cache();
        self.cache_view = array_view(&cache);
        let old_cache = std::mem::replace(&mut self.cache, cache);
        let old_scratch = std::mem::replace(&mut self.scratch, scratch);
        self.retired.textures.push((old_cache, MemoryClass::Fixed));
        self.retired
            .textures
            .push((old_scratch, MemoryClass::Fixed));
        self.integer_cache = integer;
        if integer {
            // Build the v3 pipelines together, so their compilation is one wait.
            self.composite_q14.get_or_init(|| {
                compute(
                    &self.device,
                    "integer tile compositor",
                    &crate::integer_composite_shader(),
                )
            });
            self.downsample_q14.get_or_init(|| {
                compute(
                    &self.device,
                    "integer display pyramid",
                    include_str!("../shaders/downsample-q14.wgsl"),
                )
            });
            self.present_q14
                .get_or_init(|| integer_present(&self.device, self.config.format));
            self.pipelines_built = true;
        }
        Ok(())
    }
    /// Diagnostic reset: keep the document, discard all GPU-derived tiles.
    pub fn reset_render_cache(&mut self) {
        self.reset_presentation();
        self.invalidate(None);
        self.atlas_map.clear();
        self.atlas_slots.iter_mut().for_each(|slot| *slot = None);
    }
    fn reset_jobs(&mut self) {
        // Requests not yet handed out belong to the jobs being dropped. Those in
        // flight are kept: their pixels are still worth placing when they arrive.
        if let Some(d) = self.decoder.as_mut() {
            for hash in d.queued.drain(..) {
                d.in_flight.remove(&hash);
            }
        }
        self.cancel_prefetch();
        self.visible_job = None;
        self.last_view = None;
        self.last_pending = 0;
        self.visible_keys.clear();
        self.overview_pins.clear();
        self.cache_pressure = false;
    }
    pub fn invalidate(&mut self, bounds: Option<Rect>) {
        self.presentation.changed();
        self.reset_jobs();
        if bounds.is_none() {
            self.prefetch_stats = PrefetchStatus::default();
        }
        let keys: Vec<_> = self
            .cache_map
            .keys()
            .filter(|&&(l, x, y)| match bounds {
                None => true,
                Some(b) => {
                    let s = 256.0 * 2f64.powi(l);
                    x as f64 * s < b.x1
                        && (x + 1) as f64 * s > b.x0
                        && y as f64 * s < b.y1
                        && (y + 1) as f64 * s > b.y0
                }
            })
            .copied()
            .collect();
        self.discard(keys);
    }
    fn discard(&mut self, keys: Vec<CacheKey>) {
        for k in keys {
            if let Some(c) = self.cache_map.remove(&k)
                && !self.presentation.protects(c.slot)
            {
                self.cache_slots[c.slot as usize] = None;
            }
        }
    }
    /// Render version 3: discards every cached display tile whose level-0
    /// pixels the integer lookup of a draw with `world` maps onto the source
    /// tile keys `keys` (`exact_reach` over the whole tile, with the 4-tap's
    /// extra texel). Every refusal and every byte of a v3 tile depends only on
    /// the draws that reach it (`v3_reach`), so a tile kept here cannot change.
    /// A draw without a Q16 inverse is refused on every tile: all are discarded.
    pub fn invalidate_reach(&mut self, world: [f64; 6], keys: [i32; 4]) {
        let Some(fixed) = alto_core::FixedAffine::from_affine(alto_core::Affine2(world)) else {
            self.invalidate(None);
            return;
        };
        self.invalidate_reach_fixed(fixed, keys);
    }
    fn invalidate_reach_fixed(&mut self, world: alto_core::FixedAffine, keys: [i32; 4]) {
        let q = match crate::sample::fixed_inverse(&world) {
            Some(Some(q)) => q,
            Some(None) => return,
            None => {
                self.invalidate(None);
                return;
            }
        };
        self.presentation.changed();
        self.reset_jobs();
        let k = keys.map(i128::from);
        let targets: Vec<_> = self
            .cache_map
            .keys()
            .filter(|&&(l, x, y)| {
                if !(0..=30).contains(&l) {
                    return true;
                }
                let size = 256i64 << l;
                let (Some(ox), Some(oy)) = (
                    i64::from(x).checked_mul(size),
                    i64::from(y).checked_mul(size),
                ) else {
                    return true;
                };
                // Outside the bounds of `exact_reach`: discard, conservatively.
                let Some(r) = exact_reach(&q, [ox, oy], 1, [0, 0, size, size], 1) else {
                    return true;
                };
                r.keys[0] <= k[2] && r.keys[2] >= k[0] && r.keys[1] <= k[3] && r.keys[3] >= k[1]
            })
            .copied()
            .collect();
        self.discard(targets);
    }
    fn cancel_prefetch(&mut self) {
        if self.prefetch_job.take().is_some() {
            self.prefetch_stats.cancelled += 1;
        }
    }
    fn upload(
        &mut self,
        hash: ContentHash,
        store: &dyn TileStore,
        pinned: &mut HashSet<u32>,
        encoder: &mut wgpu::CommandEncoder,
        previews: &HashMap<ContentHash, wgpu::Texture>,
    ) -> Result<u32> {
        Ok(self.upload_timed(hash, store, pinned, encoder, previews)?.0)
    }
    /// The empty or least recently used atlas slot outside `pinned`, emptied.
    fn free_atlas_slot(&mut self, pinned: &HashSet<u32>) -> Result<usize> {
        let slot = self
            .atlas_slots
            .iter()
            .position(Option::is_none)
            .or_else(|| {
                self.atlas_slots
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !pinned.contains(&(*i as u32)))
                    .min_by(|(_, a), (_, b)| {
                        let rank = |s: &Option<AtlasSlot>| {
                            s.as_ref().map(|s| {
                                (
                                    self.atlas_distances
                                        .get(&s.hash)
                                        .copied()
                                        .unwrap_or(f64::INFINITY),
                                    s.used,
                                )
                            })
                        };
                        let (ad, at) = rank(a).unwrap_or((f64::INFINITY, 0));
                        let (bd, bt) = rank(b).unwrap_or((f64::INFINITY, 0));
                        bd.total_cmp(&ad).then(at.cmp(&bt))
                    })
                    .map(|(i, _)| i)
            })
            .ok_or(RenderError::Budget(
                "GPU budget too small for one display tile".into(),
            ))?;
        if let Some(old) = self.atlas_slots[slot].take() {
            if self.profiling {
                self.profile.atlas_evictions += 1;
            }
            self.atlas_map.remove(&old.hash);
        }
        Ok(slot)
    }
    /// Places one source tile in the atlas. `u32::MAX` means the store has no such tile.
    fn upload_timed(
        &mut self,
        hash: ContentHash,
        store: &dyn TileStore,
        pinned: &mut HashSet<u32>,
        encoder: &mut wgpu::CommandEncoder,
        previews: &HashMap<ContentHash, wgpu::Texture>,
    ) -> Result<(u32, UploadCost)> {
        let mut cost = UploadCost::default();
        if let Some(&slot) = self.atlas_map.get(&hash) {
            if self.profiling {
                self.profile.atlas_hits += 1;
            }
            self.atlas_slots[slot as usize]
                .as_mut()
                .ok_or(RenderError::Internal("atlas slot".into()))?
                .used = self.frame;
            pinned.insert(slot);
            return Ok((slot, cost));
        }
        let now = self.clock;
        let t = now();
        let pixels = if previews.contains_key(&hash) {
            None
        } else {
            alto_raster::load(store, &hash)?
        };
        cost.get_ms = now() - t;
        if pixels.is_none() && !previews.contains_key(&hash) {
            return Ok((u32::MAX, cost));
        }
        let slot = self.free_atlas_slot(pinned)?;
        let origin = wgpu::Origin3d {
            x: 0,
            y: 0,
            z: slot as u32,
        };
        let extent = wgpu::Extent3d {
            width: 256,
            height: 256,
            depth_or_array_layers: 1,
        };
        if let Some(preview) = previews.get(&hash) {
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: preview,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas,
                    mip_level: 0,
                    origin,
                    aspect: wgpu::TextureAspect::All,
                },
                extent,
            );
        } else if let Some(tile) = pixels {
            let t = now();
            let rgba = rgba_pixels(&tile);
            cost.convert_ms = now() - t;
            let t = now();
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas,
                    mip_level: 0,
                    origin,
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(1024),
                    rows_per_image: Some(256),
                },
                extent,
            );
            cost.write_ms = now() - t;
            cost.bytes = TILE_RGBA_BYTES;
        }
        if self.profiling {
            self.profile.uploads += 1;
            self.profile.get_ms += cost.get_ms;
            self.profile.write_ms += cost.write_ms;
            self.profile.upload_bytes += cost.bytes;
        }
        self.atlas_slots[slot] = Some(AtlasSlot {
            hash,
            used: self.frame,
        });
        self.atlas_map.insert(hash, slot as u32);
        pinned.insert(slot as u32);
        Ok((slot as u32, cost))
    }
    fn commands(
        &mut self,
        scene: &RenderScene,
        key: CacheKey,
        store: &dyn TileStore,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(Vec<Command>, Vec<Entry>)> {
        let mut previews = HashMap::new();
        let mut overrides = BTreeMap::new();
        if let Some(stroke) = &self.stroke
            && !stroke.mask
        {
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
        let mut commands = Vec::new();
        let mut entries = Vec::new();
        let mut pinned = HashSet::new();
        let s = 2f64.powi(key.0);
        // The composite shader writes only inside the canvas, so content outside it is
        // never sampled. Clipping here also lets opaque layers hide lower ones in edge tiles.
        let region = [
            (key.1 as f64 * 256.0 * s).max(scene.canvas.x0 as f64),
            (key.2 as f64 * 256.0 * s).max(scene.canvas.y0 as f64),
            ((key.1 + 1) as f64 * 256.0 * s).min(scene.canvas.x1 as f64),
            ((key.2 + 1) as f64 * 256.0 * s).min(scene.canvas.y1 as f64),
        ];
        struct Build<'a> {
            scene: &'a RenderScene,
            key: CacheKey,
            region: [f64; 4],
            overrides: &'a BTreeMap<(NodeId, TileKey), ContentHash>,
        }
        #[allow(clippy::too_many_arguments)] // Recursive scene compiler keeps GPU upload and plan outputs in one traversal.
        fn walk(
            r: &mut Renderer,
            b: &Build<'_>,
            idx: usize,
            world: WorldTransform,
            depth: u32,
            commands: &mut Vec<Command>,
            entries: &mut Vec<Entry>,
            store: &dyn TileStore,
            encoder: &mut wgpu::CommandEncoder,
            pinned: &mut HashSet<u32>,
            previews: &HashMap<ContentHash, wgpu::Texture>,
        ) -> Result<()> {
            let n = b
                .scene
                .nodes
                .get(idx)
                .ok_or(RenderError::Internal("scene node".into()))?;
            if !n.visible || n.opacity <= 0.0 {
                return Ok(());
            }
            let world = world.concat(n.transform)?;
            match &n.content {
                RContent::Group { children, isolated } => {
                    let isolate = *isolated || n.opacity < 1.0 || n.blend != RenderBlend::Normal;
                    if isolate {
                        if depth >= MAX_ISOLATED_GROUPS {
                            return Err(RenderError::InvalidArgument(format!(
                                "group nesting exceeds {MAX_ISOLATED_GROUPS}"
                            )));
                        }
                        commands.push(Command {
                            inv0: [0.0; 4],
                            inv1: [0.0; 4],
                            info: [1, 0, 0, 0],
                            trans: [0; 4],
                        });
                    }
                    let mut start = 0;
                    for (i, &child) in children.iter().enumerate().rev() {
                        let cn = &b.scene.nodes[child];
                        if !cn.visible || cn.opacity != 1.0 || cn.blend != RenderBlend::Normal {
                            continue;
                        }
                        if let RContent::Tiles(ts) = &cn.content {
                            let child_world = world.concat(cn.transform)?;
                            // Version 3 hides lower layers only behind the tiles the
                            // integer sampler actually reads.
                            let (x0, y0, x1, y1) = if let WorldTransform::V3(fixed) = child_world {
                                let Some(inv) = v3_inverse(fixed)? else {
                                    continue;
                                };
                                match plain_draw_v3(
                                    inv,
                                    b.key,
                                    canvas_end(b.scene),
                                    ts.iter().map(|t| t.key).chain(
                                        b.overrides
                                            .keys()
                                            .filter(|(l, _)| *l == cn.key)
                                            .map(|(_, k)| *k),
                                    ),
                                )?
                                .keys
                                {
                                    Some([x0, y0, x1, y1]) => (x0, y0, x1 + 1, y1 + 1),
                                    None => continue,
                                }
                            } else {
                                let Some(inv) = inverse(child_world.legacy()?) else {
                                    continue;
                                };
                                let (min, max) = bounds_through(inv, b.region);
                                (
                                    (min[0] / 256.0).floor() as i32,
                                    (min[1] / 256.0).floor() as i32,
                                    (max[0] / 256.0).ceil() as i32,
                                    (max[1] / 256.0).ceil() as i32,
                                )
                            };
                            let mut opaque =
                                x1 > x0 && y1 > y0 && (x1 - x0) as i64 * (y1 - y0) as i64 <= 1024;
                            for x in x0..x1 {
                                for y in y0..y1 {
                                    let k = TileKey { tx: x, ty: y };
                                    opaque &= !b.overrides.contains_key(&(cn.key, k))
                                        && ts
                                            .binary_search_by_key(&k, |t| t.key)
                                            .ok()
                                            .is_some_and(|j| store.is_opaque(&ts[j].hash));
                                }
                            }
                            if opaque {
                                start = i;
                                break;
                            }
                        }
                    }
                    for &child in &children[start..] {
                        walk(
                            r,
                            b,
                            child,
                            world,
                            depth + u32::from(isolate),
                            commands,
                            entries,
                            store,
                            encoder,
                            pinned,
                            previews,
                        )?;
                    }
                    if isolate {
                        commands.push(Command {
                            inv0: [0.0; 4],
                            inv1: [0.0, 0.0, n.opacity, mode(n.blend)],
                            info: [2, 0, 0, 0],
                            trans: [0; 4],
                        });
                    }
                }
                RContent::Tiles(tiles) => {
                    let (inv, minx, maxx, miny, maxy) = match world {
                        WorldTransform::Legacy(world) => {
                            let Some(inv) = inverse(world) else {
                                return Ok(());
                            };
                            let (min, max) = bounds_through(inv, b.region);
                            (inv, min[0], max[0], min[1], max[1])
                        }
                        WorldTransform::V3(_) => ([0.0; 6], 0.0, 0.0, 0.0, 0.0),
                    };
                    let mut map: BTreeMap<TileKey, ContentHash> =
                        tiles.iter().map(|t| (t.key, t.hash)).collect();
                    for ((layer, k), hash) in b.overrides {
                        if *layer == n.key {
                            map.insert(*k, *hash);
                        }
                    }
                    let v3 = if let WorldTransform::V3(fixed) = world {
                        let Some(q) = v3_inverse(fixed)? else {
                            return Ok(());
                        };
                        Some(plain_draw_v3(
                            q,
                            b.key,
                            canvas_end(b.scene),
                            map.keys().copied(),
                        )?)
                    } else {
                        None
                    };
                    let start = entries.len() as u32;
                    for (k, hash) in map {
                        let skip = match v3 {
                            Some(d) => !d.reads(k),
                            None => {
                                k.tx as f64 * 256.0 >= maxx
                                    || (k.tx + 1) as f64 * 256.0 <= minx
                                    || k.ty as f64 * 256.0 >= maxy
                                    || (k.ty + 1) as f64 * 256.0 <= miny
                            }
                        };
                        if skip {
                            continue;
                        }
                        let slot = r.upload(hash, store, pinned, encoder, previews)?;
                        entries.push(Entry {
                            xy: [k.tx, k.ty],
                            slot,
                            pad: 0,
                        });
                    }
                    if let Some(d) = v3 {
                        // A draw that reaches none of its tiles is skipped (DQ3-4).
                        if d.keys.is_some() {
                            let count = entries.len() as u32 - start;
                            commands.push(d.command(n.opacity, n.blend, start, count));
                        }
                        return Ok(());
                    }
                    let packed = crate::sample::pack_inverse(inv, b.scene.render_version);
                    commands.push(Command {
                        inv0: packed.inv0,
                        inv1: [
                            packed.translation[0],
                            packed.translation[1],
                            n.opacity,
                            mode(n.blend),
                        ],
                        info: [0, start, entries.len() as u32 - start, packed.sample_mode],
                        trans: packed.trans,
                    });
                }
                _ => {}
            }
            Ok(())
        }
        walk(
            self,
            &Build {
                scene,
                key,
                region,
                overrides: &overrides,
            },
            scene.root,
            WorldTransform::root(scene.render_version),
            0,
            &mut commands,
            &mut entries,
            store,
            encoder,
            &mut pinned,
            &previews,
        )?;
        Ok((commands, entries))
    }
    fn cache_slot(&mut self, key: CacheKey, prefetch: bool) -> Result<Option<usize>> {
        let slot = if let Some(i) = self.cache_slots.iter().position(Option::is_none) {
            i
        } else {
            self.cache_pressure = true;
            let Some((&old, _)) = self
                .cache_map
                .iter()
                .filter(|(cached, entry)| {
                    !self.presentation.protects(entry.slot)
                        && !self.visible_keys.contains(cached)
                        && !self.overview_keys.contains(cached)
                        && !self.overview_pins.contains(cached)
                        && !self.pyramid_pins.contains(cached)
                        && !(prefetch
                            && key.0 > 0
                            && cached.0 == key.0 - 1
                            && cached.1.div_euclid(2) == key.1
                            && cached.2.div_euclid(2) == key.2)
                })
                .min_by(|(ak, a), (bk, b)| {
                    let (ab, ai, ad) = self.eviction_rank(**ak);
                    let (bb, bi, bd) = self.eviction_rank(**bk);
                    // Higher bucket / nonideal / distance is evicted first; older breaks ties.
                    bb.cmp(&ab)
                        .then(bi.cmp(&ai))
                        .then(bd.total_cmp(&ad))
                        .then(a.used.cmp(&b.used))
                })
            else {
                return if prefetch {
                    Ok(None)
                } else {
                    Err(RenderError::Budget(
                        "GPU budget: no evictable display tile".into(),
                    ))
                };
            };
            let c = self
                .cache_map
                .remove(&old)
                .ok_or(RenderError::Internal("cache missing".into()))?;
            // A resumable caller may defer submission or cancel. Keep the
            // reverse map consistent even before a replacement is published.
            self.cache_slots[c.slot as usize] = None;
            c.slot as usize
        };
        Ok(Some(slot))
    }
    fn compose(
        &mut self,
        scene: &RenderScene,
        key: CacheKey,
        store: &dyn TileStore,
        prefetch: bool,
    ) -> Result<bool> {
        if self.integer_cache && key.0 > 0 {
            return self.compose_from_children(scene, key, store, prefetch);
        }
        let Some(slot) = self.cache_slot(key, prefetch)? else {
            return Ok(false);
        };
        if !crate::damage::plain_scene(scene) {
            self.compose_m5(scene, key, slot as u32, store)?;
            self.cache_slots[slot] = Some(key);
            self.cache_map.insert(
                key,
                Cached {
                    slot: slot as u32,
                    used: self.frame,
                    prefetched: prefetch,
                },
            );
            return Ok(true);
        }
        // A plain scene needs neither; their GPU objects are destroyed, not dropped.
        if let Some(m5) = self.m5.take() {
            m5.retire(&mut self.retired);
        }
        if let Some(vector) = self.vector.take() {
            vector.retire(&mut self.retired);
        }
        self.release_retired();
        if self.downsample_cached(scene, key, slot as u32)? {
            if self.profiling {
                self.profile.pyramid_hits += 1;
            }
            self.cache_slots[slot] = Some(key);
            self.cache_map.insert(
                key,
                Cached {
                    slot: slot as u32,
                    used: self.frame,
                    prefetched: prefetch,
                },
            );
            return Ok(true);
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("compose tile"),
            });
        let commands_start = (self.clock)();
        let (commands, entries) = self.commands(scene, key, store, &mut encoder)?;
        if self.profiling {
            self.profile.commands_ms += (self.clock)() - commands_start;
        }
        self.submit_composite(scene, key, slot, commands, entries, encoder, prefetch)
    }
    /// Render version 3 defines a level >= 1 tile only as the integer downsample of
    /// its children, so a cold request builds the missing children first and its
    /// bytes equal the warm (cached-children) result. Determinism over speed.
    /// Only explicit readback (`read_tiles`) reaches this synchronously; display
    /// and prefetch build v3 parents step by step through `hierarchy_step`.
    fn compose_from_children(
        &mut self,
        scene: &RenderScene,
        key: CacheKey,
        store: &dyn TileStore,
        prefetch: bool,
    ) -> Result<bool> {
        let pins = self.pyramid_pins.len();
        let result = self.compose_children_then_parent(scene, key, store, prefetch);
        self.pyramid_pins.truncate(pins);
        result
    }
    fn compose_children_then_parent(
        &mut self,
        scene: &RenderScene,
        key: CacheKey,
        store: &dyn TileStore,
        prefetch: bool,
    ) -> Result<bool> {
        let mut warm = true;
        for child in pyramid_children(scene, key) {
            if !self.cache_map.contains_key(&child) {
                warm = false;
                if !self.compose(scene, child, store, prefetch)? {
                    return Ok(false);
                }
            }
            self.pyramid_pins.push(child);
        }
        if warm && self.profiling {
            self.profile.pyramid_hits += 1;
        }
        let Some(slot) = self.cache_slot(key, prefetch)? else {
            return Ok(false);
        };
        if !self.downsample_cached(scene, key, slot as u32)? {
            return Err(RenderError::Internal("v3 pyramid children missing".into()));
        }
        self.cache_slots[slot] = Some(key);
        self.cache_map.insert(
            key,
            Cached {
                slot: slot as u32,
                used: self.frame,
                prefetched: prefetch,
            },
        );
        Ok(true)
    }
    #[allow(clippy::too_many_arguments)] // Submission shares the visible and prefetch paths without copying GPU setup.
    fn submit_composite(
        &mut self,
        scene: &RenderScene,
        key: CacheKey,
        slot: usize,
        mut commands: Vec<Command>,
        entries: Vec<Entry>,
        mut encoder: wgpu::CommandEncoder,
        prefetch: bool,
    ) -> Result<bool> {
        let integer = scene.render_version == 3;
        if integer {
            for c in &mut commands {
                // Every v3 draw is integer (`plain_draw_v3`); there is no f32 fetch.
                if c.info[0] == 0
                    && ![crate::sample::SAMPLE_INTEGER, crate::sample::SAMPLE_NEAREST]
                        .contains(&c.info[3])
                {
                    return Err(RenderError::Internal(
                        "v3 draw without an integer sampler".into(),
                    ));
                }
                c.inv1[2] = f32::from_bits(crate::composite_opacity_q14(c.inv1[2]) as u32);
                c.inv1[3] = f32::from_bits(c.inv1[3] as u32);
            }
        }
        let pipeline = if integer {
            self.composite_q14.get_or_init(|| {
                compute(
                    &self.device,
                    "integer tile compositor",
                    &crate::integer_composite_shader(),
                )
            })
        } else {
            &self.composite
        };
        let integer_output = if integer {
            Some(memory::create_buffer(
                &self.device,
                &wgpu::BufferDescriptor {
                    label: Some("integer composite bytes"),
                    size: 256 * 256 * 4,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                },
                Class::Fixed,
                true,
            )?)
        } else {
            None
        };
        let cb = buffer(
            &self.device,
            &self.queue,
            "scene commands",
            &commands,
            wgpu::BufferUsages::STORAGE,
        )?;
        self.retired.classified_buffer(&cb, MemoryClass::Fixed);
        let tb = buffer(
            &self.device,
            &self.queue,
            "sparse tile index",
            &entries,
            wgpu::BufferUsages::STORAGE,
        )?;
        self.retired.classified_buffer(&tb, MemoryClass::Fixed);
        let scale = 2f32.powi(key.0);
        let ub = if integer {
            // Level 0 only (`plain_draw_v3`): the origin is the key times 256.
            let origin = [key.1, key.2].map(|k| i64::from(k) * 256);
            let [Ok(ox), Ok(oy)] = origin.map(i32::try_from) else {
                return Err(RenderError::GpuUnsupported(
                    "v3 tile has no integer origin".into(),
                ));
            };
            buffer(
                &self.device,
                &self.queue,
                "integer tile config",
                &[IntegerConfig {
                    origin_scale: [ox, oy, 1, 0],
                    info: [commands.len() as u32, slot as u32, 0, 0],
                    span: resolve_span(
                        origin,
                        1,
                        [i64::from(scene.canvas.x1), i64::from(scene.canvas.y1)],
                    ),
                }],
                wgpu::BufferUsages::UNIFORM,
            )?
        } else {
            buffer(
                &self.device,
                &self.queue,
                "tile config",
                &[Config {
                    origin_scale: [
                        key.1 as f32 * 256.0 * scale,
                        key.2 as f32 * 256.0 * scale,
                        scale,
                        scene.canvas.x1 as f32,
                    ],
                    info: [
                        commands.len() as u32,
                        slot as u32,
                        scene.canvas.y1 as u32,
                        0,
                    ],
                }],
                wgpu::BufferUsages::UNIFORM,
            )?
        };
        self.retired.classified_buffer(&ub, MemoryClass::Fixed);
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compose"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: match &integer_output {
                        Some(out) => out.as_entire_binding(),
                        None => wgpu::BindingResource::TextureView(&self.cache_view),
                    },
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: cb.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: tb.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: ub.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("composite"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(16, 16, 1);
        }
        if let Some(out) = integer_output {
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &out,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(1024),
                        rows_per_image: Some(256),
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self.cache,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: slot as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: 256,
                    height: 256,
                    depth_or_array_layers: 1,
                },
            );
            self.retired.classified_buffer(&out, MemoryClass::Fixed);
        }
        self.queue.submit(Some(encoder.finish()));
        self.release_retired();
        self.cache_slots[slot] = Some(key);
        self.cache_map.insert(
            key,
            Cached {
                slot: slot as u32,
                used: self.frame,
                prefetched: prefetch,
            },
        );
        Ok(true)
    }
    fn resumable_scene(&self, scene: &RenderScene) -> bool {
        self.stroke.is_none() && crate::damage::plain_scene(scene)
    }
    fn prefetch_keys(&self, scene: &RenderScene, view: Viewport) -> Vec<CacheKey> {
        if !self.resumable_scene(scene) {
            return Vec::new();
        }
        let zoom = view.zoom.max(0.01);
        let target_zoom = (zoom / 2.0).max(0.01);
        let current_level = (1.0 / zoom).log2().floor().clamp(0.0, 12.0) as i32;
        let level = (1.0 / target_zoom).log2().floor().clamp(0.0, 12.0) as i32;
        if level != current_level + 1 {
            return Vec::new();
        }
        let c = view.rotation.cos();
        let s = view.rotation.sin();
        let corners = [[-0.5, -0.5], [0.5, -0.5], [-0.5, 0.5], [0.5, 0.5]].map(|p| {
            let x = p[0] * self.config.width as f64 / target_zoom;
            let y = p[1] * self.config.height as f64 / target_zoom;
            [
                view.center[0] + c * x + s * y,
                view.center[1] - s * x + c * y,
            ]
        });
        let x0 = corners
            .iter()
            .map(|p| p[0])
            .fold(f64::INFINITY, f64::min)
            .max(0.0);
        let y0 = corners
            .iter()
            .map(|p| p[1])
            .fold(f64::INFINITY, f64::min)
            .max(0.0);
        let x1 = corners
            .iter()
            .map(|p| p[0])
            .fold(f64::NEG_INFINITY, f64::max)
            .min(scene.canvas.x1 as f64);
        let y1 = corners
            .iter()
            .map(|p| p[1])
            .fold(f64::NEG_INFINITY, f64::max)
            .min(scene.canvas.y1 as f64);
        if x1 <= x0 || y1 <= y0 {
            return Vec::new();
        }
        let span = 256.0 * 2f64.powi(level);
        let mut keys = Vec::new();
        for y in (y0 / span).floor() as i32..(y1 / span).ceil() as i32 {
            for x in (x0 / span).floor() as i32..(x1 / span).ceil() as i32 {
                keys.push((level, x, y));
                if keys.len() > self.cache_slots.len() {
                    return Vec::new();
                }
            }
        }
        keys.sort_by(|a, b| {
            let distance = |key: &CacheKey| {
                let dx = (key.1 as f64 + 0.5) * span - view.center[0];
                let dy = (key.2 as f64 + 0.5) * span - view.center[1];
                dx * dx + dy * dy
            };
            distance(a).total_cmp(&distance(b)).then_with(|| a.cmp(b))
        });
        keys
    }
    pub fn prefetch_status(&self, scene: &RenderScene, view: Viewport) -> PrefetchStatus {
        let mut keys = self.prefetch_keys(scene, view);
        if self.cache_pressure || self.presentation.needs_overview() {
            keys.extend(
                self.overview_keys
                    .iter()
                    .filter(|k| !keys.contains(k))
                    .copied()
                    .collect::<Vec<_>>(),
            );
        }
        PrefetchStatus {
            candidates: keys.len() as u32,
            ready: keys
                .iter()
                .filter(|key| self.cache_map.contains_key(key))
                .count() as u32,
            ..self.prefetch_stats
        }
    }
    /// Advances one parent job for a bounded CPU slice. The Worker yields between calls.
    pub fn prefetch_step(
        &mut self,
        scene: &RenderScene,
        store: &dyn TileStore,
        view: Viewport,
    ) -> Result<JobStep> {
        self.prepare_scene(scene)?;
        let now = self.clock;
        if self.is_lost() || self.last_pending != 0 || self.last_view != Some(view) {
            return Ok(JobStep::Idle);
        }
        if (self.cache_pressure || self.presentation.needs_overview())
            && self
                .overview_keys
                .iter()
                .any(|k| !self.cache_map.contains_key(k))
        {
            let step = self.overview_step(scene, store)?;
            self.capture_overview();
            return Ok(step);
        }
        self.overview_pins.clear();
        let keys = self.prefetch_keys(scene, view);
        if self.integer_cache {
            // Version 3 parents come only from their children: descend like the overview.
            let Some(key) = keys
                .into_iter()
                .find(|key| !self.cache_map.contains_key(key))
            else {
                return Ok(JobStep::Idle);
            };
            return self.hierarchy_step(scene, store, key);
        }
        if self
            .prefetch_job
            .as_ref()
            .is_some_and(|j| !keys.contains(&j.key) || self.cache_map.contains_key(&j.key))
        {
            self.cancel_prefetch();
        }
        if self.prefetch_job.is_none() {
            let Some(key) = keys
                .into_iter()
                .find(|key| !self.cache_map.contains_key(key))
            else {
                return Ok(JobStep::Idle);
            };
            self.prefetch_job = Some(TileJob::new(scene, key));
        }
        let mut job = self
            .prefetch_job
            .take()
            .ok_or(RenderError::Internal("prefetch job".into()))?;
        let start = now();
        let result = self.advance_tile_job(&mut job, scene, store, true);
        let elapsed = now() - start;
        if result.is_ok() {
            if result.as_ref().is_ok_and(|s| *s == JobStep::Ready) {
                self.prefetch_stats.composed += 1;
            } else {
                self.prefetch_job = Some(job);
            }
            self.prefetch_stats.total_ms += elapsed;
            self.prefetch_stats.max_step_ms = self.prefetch_stats.max_step_ms.max(elapsed);
        }
        result
    }
    fn advance_tile_job(
        &mut self,
        job: &mut TileJob,
        scene: &RenderScene,
        store: &dyn TileStore,
        prefetch: bool,
    ) -> Result<JobStep> {
        let now = self.clock;
        if let Some(hash) = self.decoder.as_mut().and_then(|d| d.failed.take()) {
            return Err(RenderError::CorruptData(format!("corrupt object {hash}")));
        }
        if !job.planned {
            let start = now();
            loop {
                job.plan_one(scene, store)?;
                if job.planned || now() - start >= JOB_SLICE_MS {
                    break;
                }
            }
            if self.profiling && !prefetch {
                self.profile.commands_ms += now() - start;
                if job.planned {
                    self.profile.unique_inputs += job.unique.len() as u32;
                }
            }
            if prefetch {
                self.prefetch_stats.max_plan_ms =
                    self.prefetch_stats.max_plan_ms.max(now() - start);
            }
            if job.unique.len() > self.atlas_slots.len() {
                return Ok(JobStep::Blocked);
            }
            return Ok(JobStep::Progress);
        }
        if job.unique.len() > self.atlas_slots.len() {
            return Ok(JobStep::Blocked);
        }
        // A few inputs decode faster here than a round trip to the pool takes;
        // that is the common case after a small edit.
        let to_decode = job.unique[job.cursor.min(job.unique.len())..]
            .iter()
            .filter(|h| !job.missing.contains(h) && !self.atlas_map.contains_key(h))
            .count();
        if to_decode > INLINE_DECODE_MAX
            && let Some(decoder) = self.decoder.as_mut()
            && job.cursor < job.unique.len()
        {
            // Ask for every input the atlas lacks; they arrive between slices.
            while job.cursor < job.unique.len() {
                let hash = job.unique[job.cursor];
                if !job.missing.contains(&hash) && !self.atlas_map.contains_key(&hash) {
                    match store.object(&hash) {
                        Some(object) => decoder.request(hash, object),
                        None => {
                            job.missing.insert(hash);
                        }
                    }
                }
                job.cursor += 1;
            }
            return Ok(JobStep::Progress);
        }
        if job.cursor < job.unique.len() {
            let start = now();
            let mut uploaded = false;
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("tile job upload"),
                });
            // Pin every resident input once per slice, including inputs later in
            // the plan. Rebuilding a prefix set for each upload is quadratic and
            // can evict inputs that the same job will shortly need.
            let mut pinned: HashSet<u32> = job
                .unique
                .iter()
                .filter_map(|h| self.atlas_map.get(h).copied())
                .collect();
            while job.cursor < job.unique.len() {
                let hash = job.unique[job.cursor];
                if !job.missing.contains(&hash) && !self.atlas_map.contains_key(&hash) {
                    let input_start = now();
                    let (slot, cost) =
                        self.upload_timed(hash, store, &mut pinned, &mut encoder, &HashMap::new())?;
                    if prefetch {
                        let s = &mut self.prefetch_stats;
                        s.max_input_ms = s.max_input_ms.max(now() - input_start);
                        s.max_get_ms = s.max_get_ms.max(cost.get_ms);
                        s.max_convert_ms = s.max_convert_ms.max(cost.convert_ms);
                        s.max_write_ms = s.max_write_ms.max(cost.write_ms);
                        s.upload_bytes += cost.bytes;
                    }
                    if slot == u32::MAX {
                        job.missing.insert(hash);
                    } else {
                        uploaded = true;
                    }
                }
                job.cursor += 1;
                if now() - start >= JOB_SLICE_MS {
                    break;
                }
            }
            if uploaded {
                self.queue.submit(Some(encoder.finish()));
                self.release_retired();
            }
            return Ok(JobStep::Progress);
        }
        if let Some(i) = job
            .unique
            .iter()
            .position(|h| !job.missing.contains(h) && !self.atlas_map.contains_key(h))
        {
            let waiting = self.decoder.as_ref().is_some_and(|d| {
                job.unique[i..]
                    .iter()
                    .any(|h| !self.atlas_map.contains_key(h) && d.in_flight.contains_key(h))
            });
            if waiting {
                return Ok(JobStep::Progress);
            }
            // Evicted after it arrived, or never requested: go round again.
            job.cursor = i;
            job.retries += 1;
            return Ok(if job.retries > MAX_JOB_RETRIES {
                JobStep::Blocked
            } else {
                JobStep::Progress
            });
        }
        let Some(slot) = self.cache_slot(job.key, prefetch)? else {
            return Ok(JobStep::Blocked);
        };
        let entries = job
            .sources
            .iter()
            .map(|s| Entry {
                xy: s.xy,
                slot: self.atlas_map.get(&s.hash).copied().unwrap_or(u32::MAX),
                pad: 0,
            })
            .collect();
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tile job compose"),
            });
        let submit_start = now();
        self.submit_composite(
            scene,
            job.key,
            slot,
            std::mem::take(&mut job.commands),
            entries,
            encoder,
            prefetch,
        )?;
        if prefetch {
            self.prefetch_stats.max_submit_ms =
                self.prefetch_stats.max_submit_ms.max(now() - submit_start);
        }
        Ok(JobStep::Ready)
    }
    pub fn render(
        &mut self,
        scene: &RenderScene,
        store: &dyn TileStore,
        view: Viewport,
        budget_ms: f64,
    ) -> Result<FrameReport> {
        self.prepare_scene(scene)?;
        if self.is_lost() {
            return Err(RenderError::GpuLost(format!(
                "device lost ({})",
                self.lost_detail()
            )));
        }
        let now = self.clock;
        let _ = self.device.poll(wgpu::PollType::Poll);
        if !self.present_in_flight.load(Ordering::Acquire) {
            self.presentation.acknowledge();
        }
        self.frame += 1;
        if let Some(m5) = &mut self.m5 {
            m5.expire(self.frame, &mut self.retired);
        }
        // Expiration has no unsubmitted consumers: render entry owns no encoder.
        self.release_retired();
        self.profile = RenderProfile::default();
        let start = now();
        let zoom = view.zoom.max(0.01);
        let vector_scene = scene.nodes.iter().any(|n| {
            matches!(
                n.content,
                RContent::Mesh(_) | RContent::Text(_) | RContent::TextBake(_)
            ) || n.features.vector_mask.is_some()
                || n.features.clip_path.is_some()
        });
        let min_level = if vector_scene { -6.0 } else { 0.0 };
        let requested_level = (1.0 / zoom).log2().floor().clamp(min_level, 12.0) as i32;
        let mut level = requested_level;
        self.eviction_view = view;
        self.ideal_level = level;
        if self.last_view != Some(view) {
            self.update_atlas_distances(scene, view);
        }
        let ol = overview_level(scene.canvas.x1.max(0) as u32, scene.canvas.y1.max(0) as u32);
        let span = 256i64 << ol;
        let ow = (scene.canvas.x1.max(0) as i64 + span - 1) / span;
        let oh = (scene.canvas.y1.max(0) as i64 + span - 1) / span;
        if ow.saturating_mul(oh) > self.cache_slots.len() as i64 {
            return Err(RenderError::Budget(
                "GPU budget: overview exceeds display capacity".into(),
            ));
        }
        self.overview_keys = (0..oh)
            .flat_map(|y| (0..ow).map(move |x| (ol, x as i32, y as i32)))
            .collect();
        let c = view.rotation.cos();
        let s = view.rotation.sin();
        let corners = [[-0.5, -0.5], [0.5, -0.5], [-0.5, 0.5], [0.5, 0.5]].map(|p| {
            let x = p[0] * self.config.width as f64 / zoom;
            let y = p[1] * self.config.height as f64 / zoom;
            [
                view.center[0] + c * x + s * y,
                view.center[1] - s * x + c * y,
            ]
        });
        let x0 = corners
            .iter()
            .map(|p| p[0])
            .fold(f64::INFINITY, f64::min)
            .max(0.0);
        let y0 = corners
            .iter()
            .map(|p| p[1])
            .fold(f64::INFINITY, f64::min)
            .max(0.0);
        let x1 = corners
            .iter()
            .map(|p| p[0])
            .fold(f64::NEG_INFINITY, f64::max)
            .min(scene.canvas.x1 as f64);
        let y1 = corners
            .iter()
            .map(|p| p[1])
            .fold(f64::NEG_INFINITY, f64::max)
            .min(scene.canvas.y1 as f64);
        let workspace = 1 + 3 * ol as usize + 2 * self.overview_keys.len();
        let available = self
            .cache_slots
            .len()
            .checked_sub(workspace)
            .filter(|n| *n > 0)
            .ok_or(RenderError::Budget(
                "GPU budget: overview workspace exceeds display capacity".into(),
            ))?;
        let mut required;
        loop {
            let span = 256.0 * 2f64.powi(level);
            let nx = ((x1 / span).ceil() as i64 - (x0 / span).floor() as i64).max(0);
            let ny = ((y1 / span).ceil() as i64 - (y0 / span).floor() as i64).max(0);
            if nx.saturating_mul(ny) <= available as i64 {
                required = Vec::with_capacity((nx * ny) as usize);
                for y in (y0 / span).floor() as i32..(y1 / span).ceil() as i32 {
                    for x in (x0 / span).floor() as i32..(x1 / span).ceil() as i32 {
                        required.push((level, x, y));
                    }
                }
                break;
            }
            if level == 12 {
                return Err(RenderError::Budget(
                    "GPU budget: viewport coverage cannot fit".into(),
                ));
            }
            level += 1;
        }
        let scale = 2f64.powi(level);
        self.ideal_level = level;
        self.presentation.request(view, requested_level, level);
        self.visible_keys = required.iter().copied().collect();
        if self.cache_slots.iter().filter(|s| s.is_none()).count() <= self.overview_keys.len() + 4 {
            self.cache_pressure = true;
        }
        for key in &required {
            if let Some(entry) = self.cache_map.get_mut(key) {
                entry.used = self.frame;
                entry.prefetched = false;
            }
        }
        // Keep the current job across viewport changes only while its tile is
        // still required. Document invalidation always drops it.
        if self.visible_job.as_ref().is_some_and(|job| {
            !self.visible_keys.contains(&job.key) || self.cache_map.contains_key(&job.key)
        }) {
            self.visible_job = None;
        }
        required.sort_by(|a, b| {
            let distance = |k: &CacheKey| {
                let x = (k.1 as f64 + 0.5) * 256.0 * scale - view.center[0];
                let y = (k.2 as f64 + 0.5) * 256.0 * scale - view.center[1];
                x * x + y * y
            };
            distance(a).total_cmp(&distance(b)).then(a.cmp(b))
        });
        let mut composed = 0;
        for key in &required {
            if self.cache_map.contains_key(key) {
                continue;
            }
            if now() - start >= budget_ms && self.profile.steps > 0 {
                break;
            }
            let step_start = now();
            if self.integer_cache && key.0 > 0 {
                // Version 3 defines a parent only as the integer downsample of its
                // children (cold == warm). Build it through the hierarchy: one leaf
                // slice or one downsample per step, while the frame budget lasts,
                // then hand the Worker back. A huge cold canvas takes many frames.
                let mut done = false;
                loop {
                    let step_start = now();
                    let step = self.hierarchy_step(scene, store, *key)?;
                    self.profile.steps += 1;
                    self.profile.max_step_ms = self.profile.max_step_ms.max(now() - step_start);
                    if step == JobStep::Blocked {
                        return Err(RenderError::Budget(
                            "GPU budget too small for the v3 display pyramid".into(),
                        ));
                    }
                    // Progress, not an idle wait: the deferred-present watchdog
                    // must not count these frames (PRESENT_DEFER_LIMIT).
                    composed += 1;
                    if self.cache_map.contains_key(key) {
                        done = true;
                        break;
                    }
                    if now() - start >= budget_ms {
                        break;
                    }
                }
                if self.profiling {
                    self.profile.compose_ms += now() - step_start;
                }
                if done {
                    continue;
                }
                break;
            }
            if self.presentation.hierarchy == Some(*key) {
                self.hierarchy_step(scene, store, *key)?;
                self.profile.steps += 1;
                if self.cache_map.contains_key(key) {
                    self.presentation.hierarchy = None;
                    composed += 1;
                }
                break;
            }
            if self.visible_tile_jobs && self.resumable_scene(scene) {
                // Reuse already-composited children before touching source tiles.
                if self.visible_job.is_none() {
                    let slot = self
                        .cache_slot(*key, false)?
                        .ok_or(RenderError::Budget("GPU budget: visible tile".into()))?;
                    if self.downsample_cached(scene, *key, slot as u32)? {
                        self.cache_slots[slot] = Some(*key);
                        self.cache_map.insert(
                            *key,
                            Cached {
                                slot: slot as u32,
                                used: self.frame,
                                prefetched: false,
                            },
                        );
                        self.profile.pyramid_hits += 1;
                        composed += 1;
                        self.profile.steps += 1;
                        self.profile.max_step_ms = self.profile.max_step_ms.max(now() - step_start);
                        continue;
                    }
                    self.visible_job = Some(TileJob::new(scene, *key));
                }
                let mut job = self
                    .visible_job
                    .take()
                    .ok_or(RenderError::Internal("visible job".into()))?;
                match self.advance_tile_job(&mut job, scene, store, false)? {
                    JobStep::Ready => {
                        composed += 1;
                    }
                    JobStep::Blocked if key.0 > 0 => {
                        self.presentation.hierarchy = Some(*key);
                    }
                    JobStep::Blocked => {
                        return Err(RenderError::Budget(
                            "GPU budget too small for one display tile".into(),
                        ));
                    }
                    _ => {
                        self.visible_job = Some(job);
                    }
                }
            } else {
                self.compose(scene, *key, store, false)?;
                composed += 1;
            }
            let elapsed = now() - step_start;
            self.profile.steps += 1;
            self.profile.max_step_ms = self.profile.max_step_ms.max(elapsed);
            if self.profiling {
                self.profile.compose_ms += elapsed;
            }
            // Each incomplete job yields to the Worker before its next slice.
            if self.visible_job.is_some() {
                break;
            }
        }
        let pending = required
            .iter()
            .filter(|k| !self.cache_map.contains_key(k))
            .count() as u32;
        self.last_view = Some(view);
        self.last_pending = pending;
        self.presentation.set_pending(pending);
        // Required coverage work also runs while optional idle prefetch is disabled.
        if self.presentation.needs_overview()
            && composed == 0
            && !self.present_in_flight.load(Ordering::Acquire)
            && now() - start < budget_ms
        {
            self.overview_step(scene, store)?;
        }
        self.capture_overview();
        if !self.presentation.covers() {
            return self.frame_report(composed, pending);
        }
        let _ = self.device.poll(wgpu::PollType::Poll);
        if self.presentation.unchanged() && composed == 0 && !self.present_deferred {
            let mut frame = self.frame_report(composed, pending)?;
            frame.deferred = self.present_in_flight.load(Ordering::Acquire);
            return Ok(frame);
        }
        if self.present_in_flight.load(Ordering::Acquire) {
            self.present_deferred = true;
            let mut frame = self.frame_report(composed, pending)?;
            frame.deferred = true;
            return Ok(frame);
        }
        self.present_deferred = false;
        let mut visible = if pending > 0 {
            self.presentation.roots()
        } else {
            Vec::new()
        };
        if pending > 0 {
            for (&key, entry) in &self.cache_map {
                if key.0 != level {
                    visible.push((key, entry.slot));
                }
            }
            visible.sort_by_key(|(k, _)| std::cmp::Reverse(k.0));
        }
        for key in &required {
            if let Some(entry) = self.cache_map.get_mut(key) {
                visible.push((*key, entry.slot));
            }
        }
        let tiles: Vec<_> = visible
            .iter()
            .map(|(k, slot)| Config {
                origin_scale: [
                    k.1 as f32 * 256.0 * 2f32.powi(k.0),
                    k.2 as f32 * 256.0 * 2f32.powi(k.0),
                    2f32.powi(k.0),
                    0.0,
                ],
                info: [*slot, 0, 0, 0],
            })
            .collect();
        let ub = buffer(
            &self.device,
            &self.queue,
            "viewport",
            &[ViewUniform {
                center_zoom: [
                    view.center[0] as f32,
                    view.center[1] as f32,
                    zoom as f32,
                    view.rotation as f32,
                ],
                screen: [
                    self.config.width as f32,
                    self.config.height as f32,
                    0.0,
                    0.0,
                ],
                canvas: [scene.canvas.x1 as f32, scene.canvas.y1 as f32, 0.0, 0.0],
            }],
            wgpu::BufferUsages::UNIFORM,
        )?;
        self.retired.classified_buffer(&ub, MemoryClass::Fixed);
        let tb = buffer(
            &self.device,
            &self.queue,
            "visible tiles",
            &tiles,
            wgpu::BufferUsages::STORAGE,
        )?;
        self.retired.classified_buffer(&tb, MemoryClass::Fixed);
        let present = if self.integer_cache {
            self.present_q14
                .get_or_init(|| integer_present(&self.device, self.config.format))
        } else {
            &self.present
        };
        let mut present_entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&self.cache_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: ub.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: tb.as_entire_binding(),
            },
        ];
        if !self.integer_cache {
            present_entries.push(wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&self.sampler),
            });
        }
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("present"),
            layout: &present.get_bind_group_layout(0),
            entries: &present_entries,
        });
        let Some(current) = self
            .surface
            .as_ref()
            .map(wgpu::Surface::get_current_texture)
        else {
            return Err(RenderError::Internal("renderer has no surface".into()));
        };
        let frame = match current {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated => {
                if let Some(surface) = &self.surface {
                    surface.configure(&self.device, &self.config);
                }
                self.present_deferred = true;
                let mut r = self.frame_report(composed, pending)?;
                r.deferred = true;
                return Ok(r);
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                self.present_deferred = true;
                let mut r = self.frame_report(composed, pending)?;
                r.deferred = true;
                return Ok(r);
            }
            _ => return Err(RenderError::GpuLost("surface".into())),
        };
        let target = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.075,
                            g: 0.085,
                            b: 0.105,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(present);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..6, 0..tiles.len() as u32);
        }
        self.queue.submit(Some(encoder.finish()));
        self.release_retired();
        self.queue.present(frame);
        self.presentation.published();
        self.present_in_flight.store(true, Ordering::Release);
        let in_flight = self.present_in_flight.clone();
        self.queue
            .on_submitted_work_done(move || in_flight.store(false, Ordering::Release));
        self.frame_report(composed, pending)
    }
    fn gpu_bytes(&self) -> GpuBytes {
        let mut gpu = self.governor.snapshot();
        gpu.live_fixed =
            (self.atlas_map.len() + self.cache_slots.iter().filter(|s| s.is_some()).count()) as u64
                * TILE_RGBA_BYTES
                + SCRATCH_BYTES;
        gpu.atlas_layers = self.atlas_slots.len() as u64;
        gpu.display_layers = self.cache_slots.len() as u64;
        gpu.overview_tiles = self.overview_keys.len() as u64;
        gpu.overview_ready = self
            .overview_keys
            .iter()
            .filter(|k| self.cache_map.contains_key(k))
            .count() as u64;
        gpu.effects_allocations = self.m5.as_ref().map_or(0, |m| m.allocations);
        gpu.effects_reuses = self.m5.as_ref().map_or(0, |m| m.reuses);
        gpu
    }
    /// Called after submit or after discarding an encoder; never while a consumer is unsubmitted.
    fn release_retired(&mut self) {
        // Mesh pressure can retire vector resources even when this tile never
        // renders a vector node. Account for them before after_submit clears them.
        if let Some(vector) = &mut self.vector {
            self.retired.take(&mut vector.retired);
        }
        self.retired.release();
        self.governor.submitted(&self.queue);
    }
    fn frame_report(&self, composed: u32, pending: u32) -> Result<FrameReport> {
        let gpu = self.gpu_bytes();
        Ok(FrameReport {
            composed,
            pending,
            profile: self.profiling.then_some(self.profile),
            deferred: self.present_in_flight.load(Ordering::Acquire),
            gpu_bytes: gpu.total(),
            gpu,
            display: self.display_report(),
        })
    }
    #[allow(clippy::too_many_arguments)] // One value per brush setting; the Worker passes them positionally.
    pub fn begin_stroke(
        &mut self,
        layer: NodeId,
        color: [f32; 4],
        hardness: f32,
        opacity: f32,
        erase: bool,
        mask: bool,
        world: [f64; 6],
        fixed_world: Option<alto_core::FixedAffine>,
    ) -> Result<()> {
        if self.stroke.is_some() {
            return Err(RenderError::Conflict("stroke already active".into()));
        }
        self.governor.reset_peak();
        self.stroke = Some(GpuStroke {
            layer,
            tiles: BTreeMap::new(),
            color,
            hardness: hardness.clamp(0.0, 0.999),
            opacity: opacity.clamp(0.0, 1.0),
            erase,
            epoch: 0,
            mask,
            world,
            fixed_world,
            damage: None,
        });
        Ok(())
    }
    pub fn stamp(
        &mut self,
        dabs: &[Dab],
        tiles: &BTreeMap<TileKey, ContentHash>,
        store: &dyn TileStore,
        selection: Option<(&alto_raster::TileMap, [f64; 6], [u32; 2])>,
    ) -> Result<Option<Rect>> {
        if dabs.is_empty() {
            return Ok(None);
        }
        let mut stroke = self
            .stroke
            .take()
            .ok_or(RenderError::Conflict("no stroke".into()))?;
        let mut affected: BTreeMap<TileKey, Vec<GpuDab>> = BTreeMap::new();
        let mut bounds = Rect {
            x0: f64::INFINITY,
            y0: f64::INFINITY,
            x1: f64::NEG_INFINITY,
            y1: f64::NEG_INFINITY,
        };
        for d in dabs {
            let r = d.radius;
            bounds.x0 = bounds.x0.min((d.center[0] - r) as f64);
            bounds.y0 = bounds.y0.min((d.center[1] - r) as f64);
            bounds.x1 = bounds.x1.max((d.center[0] + r) as f64);
            bounds.y1 = bounds.y1.max((d.center[1] + r) as f64);
            for y in ((d.center[1] - r) / 256.0).floor() as i32
                ..=((d.center[1] + r) / 256.0).floor() as i32
            {
                for x in ((d.center[0] - r) / 256.0).floor() as i32
                    ..=((d.center[0] + r) / 256.0).floor() as i32
                {
                    affected
                        .entry(TileKey { tx: x, ty: y })
                        .or_default()
                        .push(GpuDab {
                            center_radius: [d.center[0], d.center[1], r, d.opacity],
                            extra: [0.0; 4],
                        });
                }
            }
        }
        let new_tiles = affected
            .keys()
            .filter(|k| !stroke.tiles.contains_key(k))
            .count();
        let tile_bytes = 786432 + if selection.is_some() { 262144 } else { 0 };
        let command_bytes = affected
            .values()
            .map(|d| {
                (d.len() * std::mem::size_of::<GpuDab>() + std::mem::size_of::<BrushUniform>())
                    as u64
            })
            .sum::<u64>();
        let new_bytes = new_tiles as u64 * tile_bytes + command_bytes;
        let cpu_finish = (stroke.tiles.len() + new_tiles) as u64
            * (TILE_RGBA_BYTES + TILE_RGBA_BYTES / 255 + 256)
            * 4
            + self.readback_capacity()
            + 1048576;
        if self.gpu_bytes().budget_used.saturating_add(new_bytes) > self.budget_bytes
            || cpu_finish > 512 * 1048576
        {
            self.stroke = Some(stroke);
            return Err(RenderError::Budget(
                "stroke would consume its GPU or CPU finish reservation".into(),
            ));
        }
        let checkpoint = self.governor.checkpoint();
        let old_keys: HashSet<_> = stroke.tiles.keys().copied().collect();
        let result = (|| -> Result<()> {
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("brush"),
                });
            let mut selection_tiles = HashMap::new();
            if let Some((m, _, _)) = selection {
                for (key, hash) in &m.tiles {
                    if let Some(tile) = alto_raster::load(store, hash)? {
                        selection_tiles.insert(*key, tile);
                    }
                }
            }
            for (key, dabs) in affected {
                if let std::collections::btree_map::Entry::Vacant(entry) = stroke.tiles.entry(key) {
                    let base = texture(
                        &self.device,
                        "stroke base",
                        1,
                        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    )?;
                    let output = texture(
                        &self.device,
                        "stroke output",
                        1,
                        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                    )?;
                    let coverage = memory::create_buffer(
                        &self.device,
                        &wgpu::BufferDescriptor {
                            label: Some("stroke coverage"),
                            size: 262144,
                            usage: wgpu::BufferUsages::STORAGE,
                            mapped_at_creation: false,
                        },
                        Class::Stroke,
                        false,
                    )?;
                    let before = tiles.get(&key).copied();
                    let previous = before
                        .map(|h| alto_raster::load(store, &h))
                        .transpose()?
                        .flatten();
                    let data = previous.unwrap_or_else(|| {
                        Arc::new(TileData {
                            format: if stroke.mask {
                                alto_core::PixelFormat::R8
                            } else {
                                alto_core::PixelFormat::Rgba8
                            },
                            pixels: vec![0; if stroke.mask { 65536 } else { 262144 }]
                                .into_boxed_slice(),
                        })
                    });
                    self.queue.write_texture(
                        base.as_image_copy(),
                        &rgba_pixels(&data),
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
                    let selected = selection
                        .map(|(_, world, size)| -> Result<wgpu::Texture> {
                            let target = texture(
                                &self.device,
                                "stroke selection",
                                1,
                                wgpu::TextureUsages::TEXTURE_BINDING
                                    | wgpu::TextureUsages::COPY_DST,
                            )?;
                            let mut pixels = vec![0u8; 262144];
                            for y in 0..256 {
                                for x in 0..256 {
                                    let p = point(
                                        world,
                                        [
                                            (key.tx * 256 + x) as f64 + 0.5,
                                            (key.ty * 256 + y) as f64 + 0.5,
                                        ],
                                    );
                                    let px = p[0].floor() as i32;
                                    let py = p[1].floor() as i32;
                                    let value = if px < 0
                                        || py < 0
                                        || px >= size[0] as i32
                                        || py >= size[1] as i32
                                    {
                                        0
                                    } else {
                                        selection_tiles
                                            .get(&TileKey {
                                                tx: px.div_euclid(256),
                                                ty: py.div_euclid(256),
                                            })
                                            .map_or(0, |t| {
                                                t.pixels[(py.rem_euclid(256) * 256
                                                    + px.rem_euclid(256))
                                                    as usize]
                                            })
                                    };
                                    pixels[(y * 256 + x) as usize * 4] = value;
                                }
                            }
                            self.queue.write_texture(
                                target.as_image_copy(),
                                &pixels,
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
                            Ok(target)
                        })
                        .transpose()?;
                    entry.insert(StrokeTile {
                        base,
                        output,
                        coverage,
                        before,
                        selection: selected,
                    });
                }
                let tile = stroke
                    .tiles
                    .get(&key)
                    .ok_or(RenderError::Internal("stroke tile".into()))?;
                let db = buffer(
                    &self.device,
                    &self.queue,
                    "dabs",
                    &dabs,
                    wgpu::BufferUsages::STORAGE,
                )?;
                self.retired.classified_buffer(&db, MemoryClass::Fixed);
                let ub = buffer(
                    &self.device,
                    &self.queue,
                    "brush config",
                    &[BrushUniform {
                        color: stroke.color,
                        params: [stroke.hardness, stroke.opacity, 0.0, 0.0],
                        origin: [key.tx as f32 * 256.0, key.ty as f32 * 256.0, 0.0, 0.0],
                        info: [
                            dabs.len() as u32,
                            u32::from(stroke.erase),
                            u32::from(tile.selection.is_some()),
                            0,
                        ],
                    }],
                    wgpu::BufferUsages::UNIFORM,
                )?;
                self.retired.classified_buffer(&ub, MemoryClass::Fixed);
                let base = tile.base.create_view(&Default::default());
                let output = tile.output.create_view(&Default::default());
                let selected = tile
                    .selection
                    .as_ref()
                    .unwrap_or(&tile.base)
                    .create_view(&Default::default());
                let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("brush"),
                    layout: &self.brush.get_bind_group_layout(0),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&base),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&output),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: tile.coverage.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: db.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: ub.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: wgpu::BindingResource::TextureView(&selected),
                        },
                    ],
                });
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&self.brush);
                pass.set_bind_group(0, &bg, &[]);
                pass.dispatch_workgroups(16, 16, 1);
            }
            self.queue.submit(Some(encoder.finish()));
            self.release_retired();
            Ok(())
        })();
        if let Err(e) = result {
            stroke.tiles.retain(|key, _| old_keys.contains(key));
            checkpoint.rollback(&self.queue);
            self.release_retired();
            self.stroke = Some(stroke);
            return Err(e);
        }
        stroke.epoch += 1;
        let damage = crate::damage::layer_rect_damage(stroke.world, bounds);
        stroke.damage = Some(
            stroke
                .damage
                .map_or(damage, |d| crate::damage::union(d, damage)),
        );
        let reach = stroke_reach(&stroke);
        self.stroke = Some(stroke);
        self.invalidate(Some(damage));
        if self.integer_cache
            && let Some((fixed_world, keys)) = reach
        {
            if let Some(fixed) = fixed_world {
                self.invalidate_reach_fixed(fixed, keys);
            } else {
                self.invalidate(None);
            }
        }
        Ok(Some(damage))
    }
    pub fn cancel_stroke(&mut self) {
        let Some(stroke) = self.stroke.take() else {
            return;
        };
        let reach = stroke_reach(&stroke);
        for tile in stroke.tiles.into_values() {
            self.retired
                .textures
                .push((tile.output, MemoryClass::Stroke));
            self.retired.textures.push((tile.base, MemoryClass::Stroke));
            self.retired
                .classified_buffer(&tile.coverage, MemoryClass::Stroke);
            if let Some(t) = tile.selection {
                self.retired.textures.push((t, MemoryClass::Stroke));
            }
        }
        self.release_retired();
        if let Some(damage) = stroke.damage {
            self.invalidate(Some(damage));
        }
        if self.integer_cache
            && let Some((fixed_world, keys)) = reach
        {
            if let Some(fixed) = fixed_world {
                self.invalidate_reach_fixed(fixed, keys);
            } else {
                self.invalidate(None);
            }
        }
    }
    /// Discards the display tiles an edit can change. Jobs always restart: they index
    /// the previous scene.
    pub fn invalidate_damage(&mut self, damage: &crate::damage::Damage) {
        use crate::damage::Damage;
        match damage {
            Damage::None => self.reset_jobs(),
            Damage::Rects(rects) => {
                for r in rects {
                    self.invalidate(Some(*r));
                }
            }
            Damage::Sources { rects, sources } => {
                for r in rects {
                    self.invalidate(Some(*r));
                }
                for s in sources {
                    if let Some(fixed) = s.fixed_world {
                        self.invalidate_reach_fixed(fixed, s.keys);
                    } else {
                        self.invalidate_reach(s.world, s.keys);
                    }
                }
            }
            Damage::All => self.invalidate(None),
        }
    }
}
impl Renderer {
    pub async fn read_tile(
        &mut self,
        scene: &RenderScene,
        store: &dyn TileStore,
        key: CacheKey,
    ) -> Result<Vec<u8>> {
        self.read_tiles(scene, store, &[key]).await
    }
    /// Display tiles, 256×256 RGBA premultiplied each, back to back in `keys` order,
    /// with one map for all. Each copy is submitted right after its tile is composed:
    /// composing a later tile may reuse an earlier tile's cache slot, and the queue
    /// runs the copy before that.
    pub async fn read_tiles(
        &mut self,
        scene: &RenderScene,
        store: &dyn TileStore,
        keys: &[CacheKey],
    ) -> Result<Vec<u8>> {
        self.prepare_scene(scene)?;
        const TILE_BYTES: u64 = TILE_RGBA_BYTES;
        let total = TILE_BYTES
            .checked_mul(keys.len() as u64)
            .ok_or(RenderError::Budget("readback size overflow".into()))?;
        if total > 512 * 1048576 {
            return Err(RenderError::Budget("CPU readback exceeds 512 MiB".into()));
        }
        let pool = self
            .readback
            .as_ref()
            .ok_or(RenderError::Internal("readback pool missing".into()))?
            .clone();
        let _lease = pool.acquire()?;
        let mut out = Vec::new();
        out.try_reserve_exact(total as usize)
            .map_err(|_| RenderError::Budget("CPU readback allocation failed".into()))?;
        for batch in keys.chunks((pool.bytes / TILE_BYTES) as usize) {
            for (i, key) in batch.iter().enumerate() {
                self.copy_display_tile(scene, store, *key, &pool.buffer, i as u64 * TILE_BYTES)?;
            }
            pool.map(batch.len() as u64 * TILE_BYTES).await?;
            let view = pool
                .buffer
                .slice(..batch.len() as u64 * TILE_BYTES)
                .get_mapped_range()
                .map_err(|e| RenderError::Internal(e.to_string()))?;
            out.extend_from_slice(&view);
            drop(view);
            pool.buffer.unmap();
        }
        Ok(out)
    }
    fn copy_display_tile(
        &mut self,
        scene: &RenderScene,
        store: &dyn TileStore,
        key: CacheKey,
        buffer: &wgpu::Buffer,
        offset: u64,
    ) -> Result<()> {
        if !self.cache_map.contains_key(&key) {
            self.compose(scene, key, store, false)?;
        }
        let slot = self
            .cache_map
            .get(&key)
            .ok_or(RenderError::Internal("cache tile".into()))?
            .slot;
        let mut e = self.device.create_command_encoder(&Default::default());
        e.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.cache,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: slot,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset,
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
        self.queue.submit(Some(e.finish()));
        self.release_retired();
        Ok(())
    }
}

/// The children of `key` that lie inside the canvas, in TL, TR, BL, BR order.
fn pyramid_children(scene: &RenderScene, key: CacheKey) -> Vec<CacheKey> {
    let mut out = Vec::new();
    if key.0 <= 0 {
        return out;
    }
    for y in 0..2 {
        for x in 0..2 {
            let child = (key.0 - 1, key.1 * 2 + x, key.2 * 2 + y);
            let span = 256i64 << child.0;
            if (child.1 as i64) * span < scene.canvas.x1 as i64
                && (child.2 as i64) * span < scene.canvas.y1 as i64
            {
                out.push(child);
            }
        }
    }
    out
}

impl Renderer {
    fn downsample_cached(&self, scene: &RenderScene, key: CacheKey, slot: u32) -> Result<bool> {
        if key.0 <= 0 {
            return Ok(false);
        }
        let child_level = key.0 - 1;
        let scale = (1u32 << child_level) as i32;
        let mut children = Vec::new();
        for y in 0..2 {
            for x in 0..2 {
                let k = (child_level, key.1 * 2 + x, key.2 * 2 + y);
                if k.1 * 256 * scale >= scene.canvas.x1 || k.2 * 256 * scale >= scene.canvas.y1 {
                    continue;
                }
                let Some(c) = self.cache_map.get(&k) else {
                    return Ok(false);
                };
                children.push((x, y, c.slot));
            }
        }
        let mut e = self.device.create_command_encoder(&Default::default());
        // WebGPU has no command-encoder clear. An empty pass clears reused texels so a
        // missing edge child stays transparent. clear_texture aborts on this backend.
        {
            let clear = self.scratch.create_view(&Default::default());
            let _pass = e.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pyramid clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &clear,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        for (x, y, from) in children {
            e.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.cache,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: from,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self.scratch,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: x as u32 * 256,
                        y: y as u32 * 256,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: 256,
                    height: 256,
                    depth_or_array_layers: 1,
                },
            );
        }
        let pipeline = if self.integer_cache {
            self.downsample_q14.get_or_init(|| {
                compute(
                    &self.device,
                    "integer display pyramid",
                    include_str!("../shaders/downsample-q14.wgsl"),
                )
            })
        } else {
            &self.downsample
        };
        let input = self.scratch.create_view(&Default::default());
        let ub = buffer(
            &self.device,
            &self.queue,
            "pyramid slot",
            &[[slot, 0, 0, 0]],
            wgpu::BufferUsages::UNIFORM,
        )?;
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("display pyramid"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.cache_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ub.as_entire_binding(),
                },
            ],
        });
        {
            let mut p = e.begin_compute_pass(&Default::default());
            p.set_pipeline(pipeline);
            p.set_bind_group(0, &bg, &[]);
            p.dispatch_workgroups(16, 16, 1);
        }
        self.queue.submit(Some(e.finish()));
        // This takes &self; the uniform is its own and the submit above was its use.
        memory::destroy_buffer(&ub, false);
        self.governor.submitted(&self.queue);
        Ok(true)
    }
}

#[cfg(test)]
mod shader_tests {
    #[test]
    fn shaders_validate() {
        let integer = crate::integer_composite_shader();
        for (name, text) in [
            ("integer composite", integer.as_str()),
            ("composite", include_str!("../shaders/composite.wgsl")),
            ("present", include_str!("../shaders/present.wgsl")),
            ("brush", include_str!("../shaders/brush.wgsl")),
            ("downsample", include_str!("../shaders/downsample.wgsl")),
            (
                "integer downsample",
                include_str!("../shaders/downsample-q14.wgsl"),
            ),
            (
                "integer present",
                include_str!("../shaders/present-q14.wgsl"),
            ),
            ("integer effects", &crate::integer_effects_shader()),
            (
                "integer effects resolve",
                include_str!("../shaders/effects-resolve-q14.wgsl"),
            ),
            ("effects", include_str!("../shaders/effects.wgsl")),
            ("vector", include_str!("../shaders/vector.wgsl")),
            ("vector-paint", include_str!("../shaders/vector-paint.wgsl")),
            (
                "vector-dense-coverage",
                concat!(
                    include_str!("../shaders/vector.wgsl"),
                    "\n",
                    include_str!("../shaders/vector-dense-coverage.wgsl")
                ),
            ),
            (
                "vector-dense-paint",
                include_str!("../shaders/vector-dense-paint.wgsl"),
            ),
            ("glyph-atlas", include_str!("../shaders/glyph-atlas.wgsl")),
            (
                "vector-q14",
                concat!(
                    include_str!("../shaders/q14.wgsl"),
                    "\n",
                    include_str!("../shaders/vector-q14.wgsl")
                ),
            ),
            (
                "glyph-q14",
                concat!(
                    include_str!("../shaders/q14.wgsl"),
                    "\n",
                    include_str!("../shaders/glyph-q14.wgsl")
                ),
            ),
            (
                "effects resolve",
                include_str!("../shaders/effects-resolve.wgsl"),
            ),
        ] {
            let owned = if name == "composite" || name == "effects" {
                crate::shader_with_blend(text)
            } else {
                text.to_string()
            };
            let module = naga::front::wgsl::parse_str(&owned)
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&owned)));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    }
}
