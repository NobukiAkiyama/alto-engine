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
mod tile_renderer;
use cache::overview_level;
mod memory;
mod presentation;
mod readback;
mod uniform;
pub use memory::GpuBytes;
use memory::{Class, Governor, cache_layers};
pub use presentation::DisplayReport;
pub use readback::{FinishControl, ReadbackStats, StrokeReadback};
mod gpu;
pub mod vector;
pub(crate) use gpu::*;
mod geometry;
pub use geometry::*;
mod compose;
mod device;
mod frame;
mod jobs;
mod pyramid;
use pyramid::pyramid_children;
mod scene;
mod stroke;
mod tile_read;
mod upload;

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
const MAX_PYRAMID_STEPS_IN_FLIGHT: u32 = 8;
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
    /// Bounds pyramid submissions queued without a Worker-side queue completion wait.
    pyramid_submissions_since_wait: u32,
    gpu_wait_requested: bool,
    prefetch_stats: PrefetchStatus,
    prefetch_job: Option<TileJob>,
    visible_job: Option<TileJob>,
    visible_tile_jobs: bool,
    composite: wgpu::ComputePipeline,
    // Built with the device: new documents are v3. v1/v2 never use them.
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
    tile_renderer: Option<tile_renderer::Resources>,
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
    /// Build v3 subtrees away from every edge from one leaf (`uniform`).
    uniform_pyramid: bool,
    uniform: Option<uniform::Memo>,
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
#[cfg(test)]
mod v3_cache_tests {
    /// A new renderer already holds the uint display cache, so the first v3 scene
    /// charges no second cache while the first waits for the GPU to retire it.
    #[test]
    fn first_v3_scene_allocates_no_second_display_cache() {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::METAL | wgpu::Backends::VULKAN | wgpu::Backends::DX12;
        let instance = wgpu::Instance::new(descriptor);
        let Ok(mut renderer) = pollster::block_on(super::Renderer::headless(&instance, 64)) else {
            return;
        };
        let before = renderer.gpu_bytes().budget_used;
        let scene = alto_scene::RenderScene {
            canvas: alto_core::IRect {
                x0: 0,
                y0: 0,
                x1: 8192,
                y1: 8192,
            },
            root: 0,
            nodes: vec![],
            render_version: alto_core::RENDER_VERSION_EXACT,
        };
        renderer.prepare_scene(&scene).unwrap();
        assert_eq!(renderer.gpu_bytes().budget_used, before);
    }
}
#[cfg(test)]
mod readback_unmap_tests {
    fn headless_instance() -> wgpu::Instance {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::METAL | wgpu::Backends::VULKAN | wgpu::Backends::DX12;
        wgpu::Instance::new(descriptor)
    }
    fn assert_reads_leave_renderer_usable(renderer: &mut super::Renderer) {
        let scene = alto_scene::RenderScene {
            canvas: alto_core::IRect {
                x0: 0,
                y0: 0,
                x1: 256,
                y1: 256,
            },
            root: 0,
            nodes: vec![alto_scene::RNode {
                key: alto_core::NodeId::new(),
                version: 0,
                visible: true,
                transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                opacity: 1.0,
                blend: crate::RenderBlend::Normal,
                effects: 0,
                features: Default::default(),
                content: crate::RContent::Group {
                    children: vec![],
                    isolated: false,
                },
            }],
            render_version: alto_core::RENDER_VERSION_EXACT,
        };
        let store = alto_raster::ObjectStore::default();
        for _ in 0..2 {
            pollster::block_on(renderer.read_tile(&scene, &store, (0, 0, 0))).unwrap();
            let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());
            assert!(!renderer.is_lost(), "{}", renderer.lost_detail());
            assert_eq!(renderer.lost_detail(), "");
            assert_eq!(renderer.readback_stats().mapped, 0);
        }
    }
    /// One readback leaves the device usable. The pool buffer used to be unmapped
    /// by `read_tiles` and again by the lease; native wgpu reports the second as
    /// an uncaptured validation error, which marks the renderer lost.
    #[test]
    fn one_read_tile_does_not_mark_the_renderer_lost() {
        let _gpu = crate::cpu_composite::GPU_RENDERER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let instance = headless_instance();
        let Ok(mut renderer) = pollster::block_on(super::Renderer::headless(&instance, 64)) else {
            return;
        };
        assert_reads_leave_renderer_usable(&mut renderer);
    }
    #[test]
    fn dropping_another_instances_renderer_leaves_this_one_usable() {
        let _gpu = crate::cpu_composite::GPU_RENDERER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let instance = headless_instance();
        let Ok(mut renderer) = pollster::block_on(super::Renderer::headless(&instance, 64)) else {
            return;
        };
        let before_drop = renderer.gpu_bytes();
        // Native wgpu resource IDs are only unique within an Instance. Dropping
        // another renderer must not destroy resources owned by this one.
        let other_instance = headless_instance();
        let Ok(other) = pollster::block_on(super::Renderer::headless(&other_instance, 64)) else {
            return;
        };
        if renderer.readback.as_ref().unwrap().buffer != other.readback.as_ref().unwrap().buffer {
            // wgpu may make native IDs global in a future release; then this test
            // no longer exercises the collision this regression covers.
            return;
        }
        drop(other);
        let _ = renderer.device.poll(wgpu::PollType::wait_indefinitely());
        assert_eq!(renderer.gpu_bytes().budget_used, before_drop.budget_used);
        assert!(!renderer.is_lost(), "{}", renderer.lost_detail());
        let before_probe = renderer.gpu_bytes().budget_used;
        let probe = super::memory::create_buffer(
            &renderer.governor,
            &renderer.device,
            &wgpu::BufferDescriptor {
                label: Some("multi-instance registry probe"),
                size: 4,
                usage: wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            },
            super::memory::Class::Vector,
            false,
        )
        .unwrap();
        assert_eq!(renderer.gpu_bytes().budget_used, before_probe + 4);
        super::memory::destroy_buffer(&renderer.governor, &probe, true);
        assert_eq!(renderer.gpu_bytes().budget_used, before_probe);
        assert_reads_leave_renderer_usable(&mut renderer);
    }
}
