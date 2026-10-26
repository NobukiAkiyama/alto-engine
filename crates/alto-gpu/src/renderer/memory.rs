//! Managed allocation estimates. Completion only reclaims the ledger, never gates queue-ordered reuse.
use super::{RenderError, Result, SCRATCH_BYTES, TILE_RGBA_BYTES};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuBytes {
    pub fixed: u64,
    /// Occupied subset of fixed, not additive.
    pub live_fixed: u64,
    pub effects: u64,
    pub stroke: u64,
    pub vector: u64,
    /// Retired subset of fixed, not additive.
    pub retired_fixed: u64,
    /// Retired subset of total, not additive.
    pub retired: u64,
    pub effects_allocations: u64,
    pub effects_reuses: u64,
    pub atlas_layers: u64,
    pub display_layers: u64,
    pub overview_tiles: u64,
    pub overview_ready: u64,
}
impl GpuBytes {
    pub fn total(self) -> u64 {
        self.fixed + self.effects + self.stroke + self.vector
    }
    pub(super) fn add(&mut self, other: Self) {
        self.fixed += other.fixed;
        self.effects += other.effects;
        self.stroke += other.stroke;
        self.vector += other.vector;
        self.retired_fixed += other.retired_fixed;
        self.retired += other.retired;
    }
}
pub(super) fn cache_layers(budget_mb: u32, max_layers: u32) -> Result<(u32, u32)> {
    if budget_mb < 64 {
        return Err(RenderError::InvalidArgument(
            "GPU memory budget must be at least 64 MiB".into(),
        ));
    }
    if max_layers < 64 {
        return Err(RenderError::GpuUnsupported(
            "GPU needs at least 64 array layers".into(),
        ));
    }
    let rest = (budget_mb as u64 * 1048576 / 2).saturating_sub(SCRATCH_BYTES);
    let atlas = ((rest * 2 / 3) / TILE_RGBA_BYTES).clamp(64, max_layers.min(512) as u64);
    let display =
        ((rest - atlas * TILE_RGBA_BYTES) / TILE_RGBA_BYTES).clamp(32, max_layers.min(256) as u64);
    Ok((atlas as u32, display as u32))
}
#[derive(Default)]
pub(super) struct Ledger {
    // A new Renderer owns a new Arc: callbacks from a lost device cannot affect recovery.
    completed: Arc<AtomicU64>,
    submitted: u64,
    entries: VecDeque<(u64, GpuBytes)>,
}
impl Ledger {
    pub fn bytes(&self) -> GpuBytes {
        let done = self.completed.load(Ordering::Acquire);
        let mut out = GpuBytes::default();
        for (_, bytes) in self.entries.iter().filter(|(n, _)| *n > done) {
            out.add(*bytes);
        }
        out
    }
    pub fn submitted(&mut self, queue: &wgpu::Queue, mut bytes: GpuBytes) {
        let done = self.completed.load(Ordering::Acquire);
        while self.entries.front().is_some_and(|(n, _)| *n <= done) {
            self.entries.pop_front();
        }
        if bytes.total() == 0 {
            return;
        }
        self.submitted += 1;
        let serial = self.submitted;
        bytes.retired_fixed = bytes.fixed;
        bytes.retired = bytes.total();
        self.entries.push_back((serial, bytes));
        let completed = self.completed.clone();
        queue.on_submitted_work_done(move || {
            completed.fetch_max(serial, Ordering::Release);
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn native_renderer_remains_transferable_to_its_worker() {
        fn assert_send<T: Send>() {}
        assert_send::<super::super::Renderer>();
    }
    #[test]
    fn budget_reservations_fit_half_and_preserve_default() {
        for (mb, expected) in [
            (512, (512, 256)),
            (256, (338, 170)),
            (128, (168, 84)),
            (64, (82, 42)),
        ] {
            let (a, d) = cache_layers(mb, 1024).unwrap();
            assert_eq!((a, d), expected);
            assert!(
                (a as u64 + d as u64) * TILE_RGBA_BYTES + SCRATCH_BYTES <= mb as u64 * 1048576 / 2
            );
        }
        assert!(cache_layers(63, 1024).is_err());
        assert!(cache_layers(64, 32).is_err());
        assert_eq!(cache_layers(512, 256).unwrap(), (256, 256));
    }
    #[test]
    fn completion_counts_once_and_old_epoch_is_isolated() {
        let mut old = Ledger::default();
        let mut next = Ledger::default();
        let b = GpuBytes {
            effects: 64,
            retired: 64,
            ..Default::default()
        };
        old.entries.push_back((1, b));
        next.entries.push_back((1, b));
        assert_eq!(old.bytes().total(), 64);
        old.completed.fetch_max(1, Ordering::Release);
        assert_eq!(old.bytes().total(), 0);
        assert_eq!(old.bytes().total(), 0);
        assert_eq!(next.bytes().total(), 64);
    }
    #[test]
    fn occupied_and_retired_subsets_are_not_added_twice() {
        let b = GpuBytes {
            fixed: 193,
            live_fixed: 3,
            effects: 8,
            stroke: 4,
            vector: 2,
            retired_fixed: 1,
            retired: 5,
            ..Default::default()
        };
        assert_eq!(b.total(), 207);
    }
}
