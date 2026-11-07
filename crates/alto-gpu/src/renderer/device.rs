//! Creating the renderer on a device, and its settings and lifecycle.
use super::*;

impl Renderer {
    pub fn take_gpu_wait_requested(&mut self) -> bool {
        let requested = std::mem::take(&mut self.gpu_wait_requested);
        if requested {
            self.pyramid_submissions_since_wait = 0;
        }
        requested
    }
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
    pub(super) async fn with_adapter(
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
            &governor,
            &device,
            "content atlas",
            slots,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
        )?;
        // New documents are render version 3, so the display cache starts in its
        // uint format. Starting in Rgba8Unorm made the first v3 frame create a second
        // cache and scratch while the first stayed charged until the GPU finished:
        // about 11.5 MiB over a 64 MiB budget, which a slow adapter could exceed.
        let cache = memory::create_texture(
            &governor,
            &device,
            &wgpu::TextureDescriptor {
                label: Some("display tiles"),
                size: wgpu::Extent3d {
                    width: TILE,
                    height: TILE,
                    depth_or_array_layers: display_layers,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Uint,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
            Class::Fixed,
        )?;
        let scratch = memory::create_texture(
            &governor,
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
                format: wgpu::TextureFormat::Rgba8Uint,
                usage: wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            },
            Class::Fixed,
        )?;
        let atlas_view = array_view(&atlas);
        let cache_view = array_view(&cache);
        let composite_q14 = compute(
            &device,
            "integer tile compositor",
            &crate::integer_composite_shader(),
        );
        let downsample_q14 = compute(
            &device,
            "integer display pyramid",
            include_str!("../../shaders/downsample-q14.wgsl"),
        );
        let present_q14 = integer_present(&device, config.format);
        let downsample = compute(
            &device,
            "display pyramid",
            include_str!("../../shaders/downsample.wgsl"),
        );
        let composite_src = crate::shader_with_blend(include_str!("../../shaders/composite.wgsl"));
        let composite = compute(&device, "tile compositor", &composite_src);
        let brush = compute(
            &device,
            "brush coverage",
            include_str!("../../shaders/brush.wgsl"),
        );
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("present"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../../shaders/present.wgsl").into()),
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
            pyramid_submissions_since_wait: 0,
            gpu_wait_requested: false,
            prefetch_stats: PrefetchStatus::default(),
            prefetch_job: None,
            visible_job: None,
            visible_tile_jobs: true,
            composite,
            composite_q14: OnceLock::from(composite_q14),
            downsample,
            downsample_q14: OnceLock::from(downsample_q14),
            integer_cache: true,
            present_q14: OnceLock::from(present_q14),
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
            tile_renderer: None,
            vector: None,
            glyph_atlas: true,
            profiling: false,
            decoder: None,
            retired: Retired::new(governor.clone()),
            governor,
            readback: Some(readback.shared()),
            readback_fault: None,
            presentation: Default::default(),
            clock: || 0.0,
            profile: RenderProfile::default(),
            uniform_pyramid: true,
            uniform: None,
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
}
