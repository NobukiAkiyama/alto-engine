//! Worker and native renderer. The document remains CPU-owned; all textures are caches.
use crate::{RContent, RenderBlend, RenderScene};
use alto_core::{ContentHash, MAX_ISOLATED_GROUPS, NodeId, Rect, TILE, TileKey};
use alto_raster::{Dab, TileData, TileStore};
use bytemuck::{Pod, Zeroable};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use wgpu::util::DeviceExt;

mod m5;
pub mod vector;

type Result<T> = std::result::Result<T, String>;
type CacheKey = (i32, i32, i32);
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Command {
    inv0: [f32; 4],
    inv1: [f32; 4],
    info: [u32; 4],
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
#[derive(Clone, Copy, Debug)]
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
    pub profile: Option<RenderProfile>,
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
}
struct AtlasSlot {
    hash: ContentHash,
    used: u64,
}
struct Cached {
    slot: u32,
    used: u64,
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
}

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    atlas: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    atlas_slots: Vec<Option<AtlasSlot>>,
    atlas_map: HashMap<ContentHash, u32>,
    cache: wgpu::Texture,
    cache_view: wgpu::TextureView,
    cache_map: BTreeMap<CacheKey, Cached>,
    cache_slots: Vec<Option<CacheKey>>,
    composite: wgpu::ComputePipeline,
    downsample: wgpu::ComputePipeline,
    present: wgpu::RenderPipeline,
    brush: wgpu::ComputePipeline,
    sampler: wgpu::Sampler,
    pub lost: Arc<AtomicBool>,
    frame: u64,
    pub stroke: Option<GpuStroke>,
    pub adapter_name: String,
    budget_bytes: u64,
    m5: Option<m5::Resources>,
    vector: Option<vector::VectorResources>,
    /// Small text through the glyph atlas (true) or always through the vector path.
    glyph_atlas: bool,
    profiling: bool,
    profile_now: fn() -> f64,
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
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
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
    })
}
fn array_view(t: &wgpu::Texture) -> wgpu::TextureView {
    t.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    })
}
fn buffer<T: Pod>(
    d: &wgpu::Device,
    label: &str,
    data: &[T],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    // WGSL runtime arrays still require space for one element when their logical length is zero.
    let zero = T::zeroed();
    d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: if data.is_empty() {
            bytemuck::bytes_of(&zero)
        } else {
            bytemuck::cast_slice(data)
        },
        usage,
    })
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
fn mode(m: RenderBlend) -> f32 {
    m as u32 as f32
}
pub fn multiply(a: [f64; 6], b: [f64; 6]) -> [f64; 6] {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}
pub fn inverse(m: [f64; 6]) -> Option<[f64; 6]> {
    let det = m[0] * m[3] - m[1] * m[2];
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    Some([
        m[3] / det,
        -m[1] / det,
        -m[2] / det,
        m[0] / det,
        (m[2] * m[5] - m[3] * m[4]) / det,
        (m[1] * m[4] - m[0] * m[5]) / det,
    ])
}
pub fn point(m: [f64; 6], p: [f64; 2]) -> [f64; 2] {
    [
        m[0] * p[0] + m[2] * p[1] + m[4],
        m[1] * p[0] + m[3] * p[1] + m[5],
    ]
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
            .map_err(|e| format!("GPU_UNSUPPORTED: {e}"))?;
        let slots = adapter
            .limits()
            .max_texture_array_layers
            .min(512)
            .min(((budget_mb.max(128) - 64) * 4).max(64));
        let limits = wgpu::Limits {
            max_texture_array_layers: slots.max(256),
            ..Default::default()
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits: limits,
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or("GPU_UNSUPPORTED: no surface format")?;
        if let Some(f) = surface
            .get_capabilities(&adapter)
            .formats
            .iter()
            .find(|f| !f.is_srgb())
        {
            config.format = *f;
        }
        surface.configure(&device, &config);
        let lost = Arc::new(AtomicBool::new(false));
        let flag = lost.clone();
        device.set_device_lost_callback(move |_, _| {
            flag.store(true, Ordering::Relaxed);
        });
        let atlas = texture(
            &device,
            "content atlas",
            slots,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        );
        let cache = texture(
            &device,
            "display tiles",
            256,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
        );
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
            cache_map: BTreeMap::new(),
            cache_slots: vec![None; 256],
            composite,
            downsample,
            present,
            brush,
            sampler,
            lost,
            frame: 0,
            stroke: None,
            adapter_name: adapter.get_info().name,
            budget_bytes: budget_mb.max(128) as u64 * 1048576,
            m5: None,
            vector: None,
            glyph_atlas: true,
            profiling: false,
            profile_now: || 0.0,
            profile: RenderProfile::default(),
        })
    }
    pub fn resize(&mut self, w: u32, h: u32) {
        if self.config.width != w.max(1) || self.config.height != h.max(1) {
            self.config.width = w.max(1);
            self.config.height = h.max(1);
            self.surface.configure(&self.device, &self.config);
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
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }
    pub fn invalidate(&mut self, bounds: Option<Rect>) {
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
        for k in keys {
            if let Some(c) = self.cache_map.remove(&k) {
                self.cache_slots[c.slot as usize] = None;
            }
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
        if let Some(&slot) = self.atlas_map.get(&hash) {
            if self.profiling {
                self.profile.atlas_hits += 1;
            }
            self.atlas_slots[slot as usize]
                .as_mut()
                .ok_or("INTERNAL: atlas slot")?
                .used = self.frame;
            pinned.insert(slot);
            return Ok(slot);
        }
        let get_start = if self.profiling {
            (self.profile_now)()
        } else {
            0.0
        };
        let pixels = if previews.contains_key(&hash) {
            None
        } else {
            store.get_checked(&hash)?
        };
        if self.profiling {
            self.profile.get_ms += (self.profile_now)() - get_start;
        }
        if pixels.is_none() && !previews.contains_key(&hash) {
            return Ok(u32::MAX);
        }
        let slot = self
            .atlas_slots
            .iter()
            .position(Option::is_none)
            .or_else(|| {
                self.atlas_slots
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !pinned.contains(&(*i as u32)))
                    .min_by_key(|(_, s)| s.as_ref().map_or(0, |s| s.used))
                    .map(|(i, _)| i)
            })
            .ok_or("GPU budget too small for one display tile")?;
        if let Some(old) = self.atlas_slots[slot].take() {
            self.atlas_map.remove(&old.hash);
        }
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
        } else if let Some(tile) = pixels {
            let write_start = if self.profiling {
                (self.profile_now)()
            } else {
                0.0
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
                &rgba_pixels(&tile),
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
                self.profile.write_ms += (self.profile_now)() - write_start;
                self.profile.upload_bytes += (256 * 256 * 4) as u64;
            }
        }
        if self.profiling {
            self.profile.uploads += 1;
        }
        self.atlas_slots[slot] = Some(AtlasSlot {
            hash,
            used: self.frame,
        });
        self.atlas_map.insert(hash, slot as u32);
        pinned.insert(slot as u32);
        Ok(slot as u32)
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
            region: [f64; 4],
            overrides: &'a BTreeMap<(NodeId, TileKey), ContentHash>,
        }
        #[allow(clippy::too_many_arguments)] // Recursive scene compiler keeps GPU upload and plan outputs in one traversal.
        fn walk(
            r: &mut Renderer,
            b: &Build<'_>,
            idx: usize,
            world: [f64; 6],
            depth: u32,
            commands: &mut Vec<Command>,
            entries: &mut Vec<Entry>,
            store: &dyn TileStore,
            encoder: &mut wgpu::CommandEncoder,
            pinned: &mut HashSet<u32>,
            previews: &HashMap<ContentHash, wgpu::Texture>,
        ) -> Result<()> {
            let n = b.scene.nodes.get(idx).ok_or("INTERNAL: scene node")?;
            if !n.visible || n.opacity <= 0.0 {
                return Ok(());
            }
            let world = multiply(world, n.transform);
            match &n.content {
                RContent::Group { children, isolated } => {
                    let isolate = *isolated || n.opacity < 1.0 || n.blend != RenderBlend::Normal;
                    if isolate {
                        if depth >= MAX_ISOLATED_GROUPS {
                            return Err(format!(
                                "INVALID_ARGUMENT: group nesting exceeds {MAX_ISOLATED_GROUPS}"
                            ));
                        }
                        commands.push(Command {
                            inv0: [0.0; 4],
                            inv1: [0.0; 4],
                            info: [1, 0, 0, 0],
                        });
                    }
                    let mut start = 0;
                    for (i, &child) in children.iter().enumerate().rev() {
                        let cn = &b.scene.nodes[child];
                        if !cn.visible || cn.opacity != 1.0 || cn.blend != RenderBlend::Normal {
                            continue;
                        }
                        if let RContent::Tiles(ts) = &cn.content {
                            let Some(inv) = inverse(multiply(world, cn.transform)) else {
                                continue;
                            };
                            let corners = [
                                [b.region[0], b.region[1]],
                                [b.region[2], b.region[1]],
                                [b.region[0], b.region[3]],
                                [b.region[2], b.region[3]],
                            ]
                            .map(|p| point(inv, p));
                            let x0 = (corners.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min)
                                / 256.0)
                                .floor() as i32;
                            let y0 = (corners.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min)
                                / 256.0)
                                .floor() as i32;
                            let x1 = (corners
                                .iter()
                                .map(|p| p[0])
                                .fold(f64::NEG_INFINITY, f64::max)
                                / 256.0)
                                .ceil() as i32;
                            let y1 = (corners
                                .iter()
                                .map(|p| p[1])
                                .fold(f64::NEG_INFINITY, f64::max)
                                / 256.0)
                                .ceil() as i32;
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
                        });
                    }
                }
                RContent::Tiles(tiles) => {
                    let Some(inv) = inverse(world) else {
                        return Ok(());
                    };
                    let corners = [
                        [b.region[0], b.region[1]],
                        [b.region[2], b.region[1]],
                        [b.region[0], b.region[3]],
                        [b.region[2], b.region[3]],
                    ]
                    .map(|p| point(inv, p));
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
                    let mut map: BTreeMap<TileKey, ContentHash> =
                        tiles.iter().map(|t| (t.key, t.hash)).collect();
                    for ((layer, k), hash) in b.overrides {
                        if *layer == n.key {
                            map.insert(*k, *hash);
                        }
                    }
                    let start = entries.len() as u32;
                    for (k, hash) in map {
                        if k.tx as f64 * 256.0 >= maxx
                            || (k.tx + 1) as f64 * 256.0 <= minx
                            || k.ty as f64 * 256.0 >= maxy
                            || (k.ty + 1) as f64 * 256.0 <= miny
                        {
                            continue;
                        }
                        let slot = r.upload(hash, store, pinned, encoder, previews)?;
                        entries.push(Entry {
                            xy: [k.tx, k.ty],
                            slot,
                            pad: 0,
                        });
                    }
                    commands.push(Command {
                        inv0: [inv[0] as f32, inv[1] as f32, inv[2] as f32, inv[3] as f32],
                        inv1: [inv[4] as f32, inv[5] as f32, n.opacity, mode(n.blend)],
                        info: [0, start, entries.len() as u32 - start, 0],
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
                region,
                overrides: &overrides,
            },
            scene.root,
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
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
    fn compose(&mut self, scene: &RenderScene, key: CacheKey, store: &dyn TileStore) -> Result<()> {
        let slot = if let Some(i) = self.cache_slots.iter().position(Option::is_none) {
            i
        } else {
            let old = *self
                .cache_map
                .iter()
                .min_by_key(|(_, v)| v.used)
                .ok_or("INTERNAL: cache empty")?
                .0;
            let c = self
                .cache_map
                .remove(&old)
                .ok_or("INTERNAL: cache missing")?;
            c.slot as usize
        };
        if scene.nodes.iter().any(|n| {
            n.features.vector_mask.is_some()
                || n.features.clip_path.is_some()
                || matches!(n.content, RContent::Mesh(_) | RContent::Text(_))
                || n.features.mask.is_some()
                || n.features.clip
                || !n.features.effects.is_empty()
                || matches!(n.content, RContent::Adjustment(_))
        }) {
            self.compose_m5(scene, key, slot as u32, store)?;
            self.cache_slots[slot] = Some(key);
            self.cache_map.insert(
                key,
                Cached {
                    slot: slot as u32,
                    used: self.frame,
                },
            );
            return Ok(());
        }
        self.m5 = None;
        self.vector = None;
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
                },
            );
            return Ok(());
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("compose tile"),
            });
        let commands_start = if self.profiling {
            (self.profile_now)()
        } else {
            0.0
        };
        let (commands, entries) = self.commands(scene, key, store, &mut encoder)?;
        if self.profiling {
            self.profile.commands_ms += (self.profile_now)() - commands_start;
        }
        let cb = buffer(
            &self.device,
            "scene commands",
            &commands,
            wgpu::BufferUsages::STORAGE,
        );
        let tb = buffer(
            &self.device,
            "sparse tile index",
            &entries,
            wgpu::BufferUsages::STORAGE,
        );
        let scale = 2f32.powi(key.0);
        let ub = buffer(
            &self.device,
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
        );
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compose"),
            layout: &self.composite.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.cache_view),
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
            pass.set_pipeline(&self.composite);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(16, 16, 1);
        }
        self.queue.submit(Some(encoder.finish()));
        self.cache_slots[slot] = Some(key);
        self.cache_map.insert(
            key,
            Cached {
                slot: slot as u32,
                used: self.frame,
            },
        );
        Ok(())
    }
    pub fn render(
        &mut self,
        scene: &RenderScene,
        store: &dyn TileStore,
        view: Viewport,
        budget_ms: f64,
        now: fn() -> f64,
    ) -> Result<FrameReport> {
        if self.is_lost() {
            return Err("GPU_LOST".into());
        }
        self.frame += 1;
        self.profile_now = now;
        self.profile = RenderProfile::default();
        let start = now();
        let zoom = view.zoom.max(0.01);
        let vector_scene = scene.nodes.iter().any(|n| {
            matches!(n.content, RContent::Mesh(_) | RContent::Text(_))
                || n.features.vector_mask.is_some()
                || n.features.clip_path.is_some()
        });
        let min_level = if vector_scene { -6.0 } else { 0.0 };
        let level = (1.0 / zoom).log2().floor().clamp(min_level, 12.0) as i32;
        let scale = 2f64.powi(level);
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
        let mut required = Vec::new();
        for y in (y0 / (256.0 * scale)).floor() as i32..(y1 / (256.0 * scale)).ceil() as i32 {
            for x in (x0 / (256.0 * scale)).floor() as i32..(x1 / (256.0 * scale)).ceil() as i32 {
                required.push((level, x, y));
            }
        }
        if required.len() > 256 {
            return Err("GPU budget: viewport needs more than 256 display tiles".into());
        }
        let mut composed = 0;
        for key in &required {
            if !self.cache_map.contains_key(key) {
                if composed > 0 && now() - start >= budget_ms {
                    break;
                }
                let compose_start = if self.profiling { now() } else { 0.0 };
                self.compose(scene, *key, store)?;
                if self.profiling {
                    self.profile.compose_ms += now() - compose_start;
                }
                composed += 1;
            }
        }
        let pending = required
            .iter()
            .filter(|k| !self.cache_map.contains_key(k))
            .count() as u32;
        let mut visible = Vec::new();
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
                entry.used = self.frame;
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
        );
        let tb = buffer(
            &self.device,
            "visible tiles",
            &tiles,
            wgpu::BufferUsages::STORAGE,
        );
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("present"),
            layout: &self.present.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.cache_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ub.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: tb.as_entire_binding(),
                },
            ],
        });
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(FrameReport::default());
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(FrameReport::default());
            }
            _ => return Err("GPU_LOST: surface".into()),
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
            pass.set_pipeline(&self.present);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..6, 0..tiles.len() as u32);
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        Ok(FrameReport {
            composed,
            pending,
            profile: self.profiling.then_some(self.profile),
            gpu_bytes: self.vector_bytes()
                + self.m5.as_ref().map_or(0, |m| m.bytes())
                + (self.atlas_slots.len() as u64 + 256) * 262144
                + self.stroke.as_ref().map_or(0, |s| {
                    s.tiles
                        .values()
                        .map(|t| 786432 + if t.selection.is_some() { 262144 } else { 0 })
                        .sum::<u64>()
                }),
        })
    }
    pub fn begin_stroke(
        &mut self,
        layer: NodeId,
        color: [f32; 4],
        hardness: f32,
        opacity: f32,
        erase: bool,
        mask: bool,
    ) -> Result<()> {
        if self.stroke.is_some() {
            return Err("CONFLICT: stroke already active".into());
        }
        self.stroke = Some(GpuStroke {
            layer,
            tiles: BTreeMap::new(),
            color,
            hardness: hardness.clamp(0.0, 0.999),
            opacity: opacity.clamp(0.0, 1.0),
            erase,
            epoch: 0,
            mask,
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
        let mut stroke = self.stroke.take().ok_or("CONFLICT: no stroke")?;
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
        let frame = self.m5.as_ref().map_or(0, |m| m.bytes())
            + (stroke.tiles.len() + new_tiles) as u64 * tile_bytes;
        let fixed = (self.atlas_slots.len() as u64 + 256) * 262144 + self.vector_fixed_bytes();
        let Some(allowance) = crate::cache_allowance(self.budget_bytes, frame, fixed) else {
            self.stroke = Some(stroke);
            return Err("INVALID_ARGUMENT: stroke exceeds GPU memory budget".into());
        };
        if self.vector_mesh_bytes() > allowance
            && let Some(vector) = self.vector.as_mut()
        {
            vector.discard_meshes();
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("brush"),
            });
        let mut selection_tiles = HashMap::new();
        if let Some((m, _, _)) = selection {
            for (key, hash) in &m.tiles {
                if let Some(tile) = store.get_checked(hash)? {
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
                );
                let output = texture(
                    &self.device,
                    "stroke output",
                    1,
                    wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                );
                let coverage = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("stroke coverage"),
                    size: 262144,
                    usage: wgpu::BufferUsages::STORAGE,
                    mapped_at_creation: false,
                });
                let before = tiles.get(&key).copied();
                let previous = before.map(|h| store.get_checked(&h)).transpose()?.flatten();
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
                let selected = selection.map(|(_, world, size)| {
                    let target = texture(
                        &self.device,
                        "stroke selection",
                        1,
                        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    );
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
                            let value =
                                if px < 0 || py < 0 || px >= size[0] as i32 || py >= size[1] as i32
                                {
                                    0
                                } else {
                                    selection_tiles
                                        .get(&TileKey {
                                            tx: px.div_euclid(256),
                                            ty: py.div_euclid(256),
                                        })
                                        .map_or(0, |t| {
                                            t.pixels[(py.rem_euclid(256) * 256 + px.rem_euclid(256))
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
                    target
                });
                entry.insert(StrokeTile {
                    base,
                    output,
                    coverage,
                    before,
                    selection: selected,
                });
            }
            let tile = stroke.tiles.get(&key).ok_or("INTERNAL: stroke tile")?;
            let db = buffer(&self.device, "dabs", &dabs, wgpu::BufferUsages::STORAGE);
            let ub = buffer(
                &self.device,
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
            );
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
        stroke.epoch += 1;
        self.stroke = Some(stroke);
        self.invalidate(Some(bounds));
        Ok(Some(bounds))
    }
    pub async fn finish_stroke(&mut self) -> Result<Vec<(TileKey, Option<ContentHash>, TileData)>> {
        let stroke = self.stroke.take().ok_or("CONFLICT: no stroke")?;
        let mut buffers = Vec::new();
        let mut encoder = self.device.create_command_encoder(&Default::default());
        for (key, tile) in stroke.tiles {
            let b = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size: 262144,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                tile.output.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &b,
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
            buffers.push((key, tile.before, b));
        }
        self.queue.submit(Some(encoder.finish()));
        let mut waits = Vec::new();
        for (_, _, b) in &buffers {
            let (tx, rx) = futures_channel::oneshot::channel();
            b.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            waits.push(rx);
        }
        for w in waits {
            w.await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        }
        let mut out = Vec::new();
        for (k, before, b) in buffers {
            let mapped = b.slice(..).get_mapped_range().map_err(|e| e.to_string())?;
            let data = TileData::new(
                if stroke.mask {
                    alto_core::PixelFormat::R8
                } else {
                    alto_core::PixelFormat::Rgba8
                },
                if stroke.mask {
                    mapped.as_chunks::<4>().0.iter().map(|p| p[0]).collect()
                } else {
                    mapped.to_vec()
                },
            )
            .map_err(|_| "CORRUPT_DATA: readback")?;
            drop(mapped);
            b.unmap();
            out.push((k, before, data));
        }
        self.invalidate(None);
        Ok(out)
    }
    pub fn cancel_stroke(&mut self) {
        self.stroke = None;
        self.invalidate(None);
    }
}
impl Renderer {
    pub async fn read_tile(
        &mut self,
        scene: &RenderScene,
        store: &dyn TileStore,
        key: CacheKey,
    ) -> Result<Vec<u8>> {
        if !self.cache_map.contains_key(&key) {
            self.compose(scene, key, store)?;
        }
        let slot = self.cache_map.get(&key).ok_or("INTERNAL: cache tile")?.slot;
        let b = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("probe"),
            size: 262144,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
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
                buffer: &b,
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
        self.queue.submit(Some(e.finish()));
        let (tx, rx) = futures_channel::oneshot::channel();
        b.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        rx.await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let view = b.slice(..).get_mapped_range().map_err(|e| e.to_string())?;
        let data = view.to_vec();
        drop(view);
        b.unmap();
        Ok(data)
    }
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
        let scratch = self.device.create_texture(&wgpu::TextureDescriptor {
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
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let mut e = self.device.create_command_encoder(&Default::default());
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
                    texture: &scratch,
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
        let input = scratch.create_view(&Default::default());
        let ub = buffer(
            &self.device,
            "pyramid slot",
            &[[slot, 0, 0, 0]],
            wgpu::BufferUsages::UNIFORM,
        );
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("display pyramid"),
            layout: &self.downsample.get_bind_group_layout(0),
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
            p.set_pipeline(&self.downsample);
            p.set_bind_group(0, &bg, &[]);
            p.dispatch_workgroups(16, 16, 1);
        }
        self.queue.submit(Some(e.finish()));
        Ok(true)
    }
}

#[cfg(test)]
mod shader_tests {
    #[test]
    fn shaders_validate() {
        for (name, text) in [
            ("composite", include_str!("../shaders/composite.wgsl")),
            ("present", include_str!("../shaders/present.wgsl")),
            ("brush", include_str!("../shaders/brush.wgsl")),
            ("downsample", include_str!("../shaders/downsample.wgsl")),
            ("effects", include_str!("../shaders/effects.wgsl")),
            ("vector", include_str!("../shaders/vector.wgsl")),
            ("vector-paint", include_str!("../shaders/vector-paint.wgsl")),
            ("glyph-atlas", include_str!("../shaders/glyph-atlas.wgsl")),
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
