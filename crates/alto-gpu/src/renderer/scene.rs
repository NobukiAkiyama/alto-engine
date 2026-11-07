//! Scene preparation and cache invalidation.
use super::*;

impl Renderer {
    // Validate before cache access or command construction. Every v3 scene that
    // passes writes integer bytes, plain or M5, so v3 always uses the uint cache.
    pub(super) fn prepare_scene(&mut self, scene: &RenderScene) -> Result<()> {
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
            &self.governor,
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
            &self.governor,
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
                    include_str!("../../shaders/downsample-q14.wgsl"),
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
    pub(super) fn reset_jobs(&mut self) {
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
    pub(super) fn discard(&mut self, keys: Vec<CacheKey>) {
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
    pub(super) fn invalidate_reach_fixed(&mut self, world: alto_core::FixedAffine, keys: [i32; 4]) {
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
    pub(super) fn cancel_prefetch(&mut self) {
        if self.prefetch_job.take().is_some() {
            self.prefetch_stats.cancelled += 1;
        }
    }
}
