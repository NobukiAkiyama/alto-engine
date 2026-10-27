//! Viewport-aware eviction and bounded background overview construction.
use super::*;
pub(super) fn overview_level(w: u32, h: u32) -> i32 {
    (w.max(h).max(1) as f64 / 256.0)
        .log2()
        .ceil()
        .clamp(0.0, 12.0) as i32
}
impl Renderer {
    pub(super) fn eviction_rank(&self, key: CacheKey) -> (u8, bool, f64) {
        let span = 256.0 * 2f64.powi(key.0);
        let dx = (key.1 as f64 + 0.5) * span - self.eviction_view.center[0];
        let dy = (key.2 as f64 + 0.5) * span - self.eviction_view.center[1];
        let c = self.eviction_view.rotation.cos();
        let s = self.eviction_view.rotation.sin();
        let x = (c * dx - s * dy).abs();
        let y = (s * dx + c * dy).abs();
        let near = (key.0 == self.ideal_level || key.0 == self.ideal_level + 1)
            && x <= 1.5 * self.config.width as f64 / self.eviction_view.zoom.max(0.01) + span / 2.0
            && y <= 1.5 * self.config.height as f64 / self.eviction_view.zoom.max(0.01)
                + span / 2.0;
        (
            if near { 1 } else { 2 },
            key.0 != self.ideal_level,
            dx * dx + dy * dy,
        )
    }
    pub(super) fn update_atlas_distances(&mut self, scene: &RenderScene, view: Viewport) {
        self.atlas_distances.clear();
        let mut pending = vec![(scene.root, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0])];
        while let Some((i, parent)) = pending.pop() {
            let n = &scene.nodes[i];
            if !n.visible {
                continue;
            }
            let world = multiply(parent, n.transform);
            let mut add = |tiles: &[crate::RenderTile], m| {
                for t in tiles {
                    let p = point(
                        m,
                        [
                            (t.key.tx as f64 + 0.5) * 256.0,
                            (t.key.ty as f64 + 0.5) * 256.0,
                        ],
                    );
                    let d = (p[0] - view.center[0]).powi(2) + (p[1] - view.center[1]).powi(2);
                    self.atlas_distances
                        .entry(t.hash)
                        .and_modify(|old| *old = f64::min(*old, d))
                        .or_insert(d);
                }
            };
            match &n.content {
                RContent::Tiles(tiles) => add(tiles, world),
                RContent::Group { children, .. } => {
                    pending.extend(children.iter().map(|i| (*i, world)))
                }
                _ => {}
            }
            if let Some(mask) = &n.features.mask {
                add(
                    &mask.tiles,
                    if mask.linked {
                        multiply(world, mask.transform)
                    } else {
                        mask.transform
                    },
                );
            }
        }
    }
    /// One leaf or parent per call. Parent children remain pinned across Worker yields.
    /// Building from cached children avoids requiring all document inputs in the atlas.
    pub(super) fn overview_step(
        &mut self,
        scene: &RenderScene,
        store: &dyn TileStore,
    ) -> Result<JobStep> {
        self.overview_pins.clear();
        let Some(key) = self
            .overview_keys
            .iter()
            .filter(|k| !self.cache_map.contains_key(k))
            .min()
            .copied()
        else {
            return Ok(JobStep::Idle);
        };
        self.hierarchy_step(scene, store, key)
    }
    pub(super) fn hierarchy_step(
        &mut self,
        scene: &RenderScene,
        store: &dyn TileStore,
        mut key: CacheKey,
    ) -> Result<JobStep> {
        self.overview_pins.clear();
        loop {
            if key.0 == 0 {
                break;
            }
            let mut missing = None;
            for y in 0..2 {
                for x in 0..2 {
                    let child = (key.0 - 1, key.1 * 2 + x, key.2 * 2 + y);
                    let span = 256i64 << child.0;
                    if child.1 as i64 * span >= scene.canvas.x1 as i64
                        || child.2 as i64 * span >= scene.canvas.y1 as i64
                    {
                        continue;
                    }
                    if self.cache_map.contains_key(&child) {
                        self.overview_pins.insert(child);
                    } else if missing.is_none() {
                        missing = Some(child);
                    }
                }
            }
            if let Some(child) = missing {
                key = child;
            } else {
                break;
            }
        }
        if key.0 > 0 {
            let Some(slot) = self.cache_slot(key, true)? else {
                return Ok(JobStep::Blocked);
            };
            if !self.downsample_cached(scene, key, slot as u32)? {
                return Err(RenderError::Internal("overview children missing".into()));
            }
            self.cache_slots[slot] = Some(key);
            self.cache_map.insert(
                key,
                Cached {
                    slot: slot as u32,
                    used: self.frame,
                    prefetched: true,
                },
            );
            self.overview_pins.clear();
            return Ok(JobStep::Progress);
        }
        if !self.resumable_scene(scene) {
            self.compose(scene, key, store, true)?;
            return Ok(JobStep::Progress);
        }
        if self.prefetch_job.as_ref().is_some_and(|j| j.key != key) {
            self.cancel_prefetch();
        }
        let mut job = self
            .prefetch_job
            .take()
            .unwrap_or_else(|| TileJob::new(scene, key));
        let result = self.advance_tile_job(&mut job, scene, store, true)?;
        if result != JobStep::Ready {
            self.prefetch_job = Some(job);
        }
        Ok(if result == JobStep::Ready {
            JobStep::Progress
        } else {
            result
        })
    }
}

#[cfg(test)]
mod memory_cache_tests {
    use super::*;
    #[test]
    fn overview_uses_one_slot_until_level_twelve() {
        assert_eq!(overview_level(3840, 2160), 4);
        assert_eq!(overview_level(64, 64), 0);
        assert_eq!(overview_level(1_048_576, 1), 12);
        assert_eq!(overview_level(u32::MAX, 1), 12);
    }
}
