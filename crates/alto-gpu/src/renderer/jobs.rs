//! Prefetch and resumable tile jobs.
use super::*;

impl TileJob {
    pub(super) fn new(scene: &RenderScene, key: CacheKey) -> Self {
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
    pub(super) fn plan_one(&mut self, scene: &RenderScene, store: &dyn TileStore) -> Result<()> {
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
    pub(super) fn resumable_scene(&self, scene: &RenderScene) -> bool {
        self.stroke.is_none() && crate::damage::plain_scene(scene)
    }
    pub(super) fn prefetch_keys(&self, scene: &RenderScene, view: Viewport) -> Vec<CacheKey> {
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
    pub(super) fn advance_tile_job(
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
}
