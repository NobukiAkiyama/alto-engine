//! Rendering a frame and reporting on it.
use super::*;

impl Renderer {
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
        if let Some(tiles) = &mut self.tile_renderer {
            tiles.expire(self.frame, &mut self.retired);
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
                    self.pyramid_submissions_since_wait += 1;
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
                    if self.pyramid_submissions_since_wait >= MAX_PYRAMID_STEPS_IN_FLIGHT {
                        self.gpu_wait_requested = true;
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
            &self.governor,
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
            &self.governor,
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
    pub(super) fn gpu_bytes(&self) -> GpuBytes {
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
        gpu.effects_allocations = self.tile_renderer.as_ref().map_or(0, |m| m.allocations);
        gpu.effects_reuses = self.tile_renderer.as_ref().map_or(0, |m| m.reuses);
        gpu
    }
    /// Called after submit or after discarding an encoder; never while a consumer is unsubmitted.
    pub(super) fn release_retired(&mut self) {
        // Mesh pressure can retire vector resources even when this tile never
        // renders a vector node. Account for them before after_submit clears them.
        if let Some(vector) = &mut self.vector {
            self.retired.take(&mut vector.retired);
        }
        self.retired.release();
        self.governor.submitted(&self.queue);
    }
    pub(super) fn frame_report(&self, composed: u32, pending: u32) -> Result<FrameReport> {
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
}
