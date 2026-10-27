//! Managed allocation estimates. Completion only reclaims the ledger, never gates queue-ordered reuse.
use super::{RenderError, Result, SCRATCH_BYTES, TILE_RGBA_BYTES};
use std::sync::Arc;
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuBytes {
    pub fixed: u64,
    /// Occupied subset of fixed, not additive.
    pub live_fixed: u64,
    pub effects: u64,
    pub stroke: u64,
    pub vector: u64,
    pub readback: u64,
    pub reserved: u64,
    pub pending_allocation: u64,
    pub budget_used: u64,
    pub budget_limit: u64,
    pub operation_peak: u64,
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
        self.fixed + self.effects + self.stroke + self.vector + self.readback
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
        let old = Governor::new(64 * 1048576, 4 * 1048576);
        let next = Governor::new(64 * 1048576, 4 * 1048576);
        let a = old.reserve(64, Class::Vector, false).unwrap().install();
        let b = next.reserve(64, Class::Vector, false).unwrap().install();
        old.retire(a, true);
        old.retire(a, true);
        assert_eq!(old.snapshot().vector, 0);
        assert_eq!(next.snapshot().vector, 64);
        next.retire(b, true);
    }
    #[test]
    fn reservations_are_atomic_and_moves_do_not_release_allocations() {
        let g = Governor::new(64 * 1048576, 4 * 1048576);
        let initial = g.snapshot().budget_used;
        {
            let pending = g.reserve(2 * 1048576, Class::Stroke, false).unwrap();
            assert_eq!(g.snapshot().pending_allocation, 2 * 1048576);
            drop(pending);
        }
        assert_eq!(g.snapshot().budget_used, initial);
        let id = g
            .reserve(40 * 1048576, Class::Stroke, false)
            .unwrap()
            .install();
        let transferred = g.clone();
        drop(g);
        assert_eq!(transferred.snapshot().stroke, 40 * 1048576);
        assert!(
            transferred
                .reserve(30 * 1048576, Class::Effects, false)
                .is_err()
        );
        transferred.retire(id, false);
        assert_eq!(transferred.snapshot().retired, 40 * 1048576);
        assert!(
            transferred
                .reserve(30 * 1048576, Class::Effects, false)
                .is_err()
        );
        transferred.retire(id, true);
        assert_eq!(transferred.snapshot().budget_used, initial);
    }
    #[test]
    fn pre_reserved_readback_fits_even_when_general_quota_is_full() {
        for mb in [64, 128, 256, 512] {
            let limit = mb * 1048576;
            let pool = super::super::readback::capacity(limit);
            let g = Governor::new(limit, pool);
            let stroke = g
                .reserve(limit - pool - 1048576, Class::Stroke, false)
                .unwrap()
                .install();
            assert_eq!(g.snapshot().budget_used, limit);
            let readback = g.reserve(pool, Class::Readback, false).unwrap().install();
            assert_eq!(g.snapshot().budget_used, limit);
            assert_eq!(g.snapshot().readback, pool);
            assert!(g.reserve(1, Class::Effects, false).is_err());
            g.retire(stroke, false);
            assert_eq!(g.snapshot().budget_used, limit);
            g.retire(stroke, true);
            g.retire(readback, true);
            assert_eq!(g.snapshot().budget_used, 1048576);
            assert_eq!(g.snapshot().operation_peak, limit);
        }
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

// Engine-owned API request accounting. Native uses a shared registry so moving a
// renderer to another worker preserves its budget; WebGPU runs in its Worker.
use std::collections::HashMap;
use std::sync::{Mutex, Weak};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Class {
    Fixed,
    Effects,
    Stroke,
    Vector,
    Readback,
}
impl Class {
    fn charge(self, out: &mut GpuBytes, n: u64) {
        match self {
            Self::Fixed => out.fixed += n,
            Self::Effects => out.effects += n,
            Self::Stroke => out.stroke += n,
            Self::Vector => out.vector += n,
            Self::Readback => out.readback += n,
        }
    }
}
#[derive(Clone, Copy)]
struct Allocation {
    bytes: u64,
    class: Class,
    retired: bool,
    serial: u64,
    work_credit: u64,
}
struct State {
    next: u64,
    serial: u64,
    entries: HashMap<u64, Allocation>,
    pending: u64,
    work: u64,
    readback_reservation: u64,
    peak: u64,
}
pub(super) struct Governor {
    limit: u64,
    state: Mutex<State>,
}
fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
impl Governor {
    pub fn new(limit: u64, readback: u64) -> Arc<Self> {
        Arc::new(Self {
            limit,
            state: Mutex::new(State {
                next: 0,
                serial: 0,
                entries: HashMap::new(),
                pending: 0,
                work: 1048576,
                readback_reservation: readback,
                peak: 0,
            }),
        })
    }
    fn used(s: &State) -> u64 {
        s.entries.values().map(|a| a.bytes).sum::<u64>()
            + s.pending
            + s.work
            + s.readback_reservation
    }
    fn reserve(self: &Arc<Self>, bytes: u64, class: Class, work: bool) -> Result<Reservation> {
        let mut s = locked(&self.state);
        let credit = if class == Class::Readback {
            s.readback_reservation.min(bytes)
        } else if work {
            s.work.min(bytes)
        } else {
            0
        };
        if Self::used(&s).saturating_add(bytes).saturating_sub(credit) > self.limit {
            return Err(RenderError::Budget(
                "GPU allocation exceeds reserved budget".into(),
            ));
        }
        if class == Class::Readback {
            s.readback_reservation -= credit;
        } else {
            s.work -= credit;
        }
        s.pending += bytes;
        s.peak = s.peak.max(Self::used(&s));
        drop(s);
        Ok(Reservation {
            governor: self.clone(),
            bytes,
            class,
            credit,
            installed: false,
        })
    }
    pub fn snapshot(&self) -> GpuBytes {
        let s = locked(&self.state);
        let mut out = GpuBytes {
            reserved: s.work + s.readback_reservation,
            pending_allocation: s.pending,
            budget_used: Self::used(&s),
            budget_limit: self.limit,
            operation_peak: s.peak,
            ..Default::default()
        };
        for a in s.entries.values() {
            a.class.charge(&mut out, a.bytes);
            if a.retired {
                out.retired += a.bytes;
                if a.class == Class::Fixed {
                    out.retired_fixed += a.bytes;
                }
            }
        }
        out
    }
    pub fn reset_peak(&self) {
        let mut s = locked(&self.state);
        s.peak = Self::used(&s);
    }
    pub fn shrink_readback(&self, bytes: u64) {
        locked(&self.state).readback_reservation = bytes;
    }
    fn retire(&self, id: u64, complete: bool) {
        let mut s = locked(&self.state);
        if complete {
            if let Some(a) = s.entries.remove(&id) {
                s.work += a.work_credit;
            }
        } else if let Some(a) = s.entries.get_mut(&id) {
            a.retired = true;
        }
    }
    pub fn submitted(self: &Arc<Self>, queue: &wgpu::Queue) {
        let mut s = locked(&self.state);
        if !s.entries.values().any(|a| a.retired && a.serial == 0) {
            return;
        }
        s.serial += 1;
        let serial = s.serial;
        for a in s
            .entries
            .values_mut()
            .filter(|a| a.retired && a.serial == 0)
        {
            a.serial = serial;
        }
        drop(s);
        let weak = Arc::downgrade(self);
        queue.on_submitted_work_done(move || {
            if let Some(g) = weak.upgrade() {
                let mut s = locked(&g.state);
                let mut credit = 0;
                s.entries.retain(|_, a| {
                    let keep = !a.retired || a.serial == 0 || a.serial > serial;
                    if !keep {
                        credit += a.work_credit;
                    }
                    keep
                });
                s.work += credit;
            }
        });
    }
}
struct Reservation {
    governor: Arc<Governor>,
    bytes: u64,
    class: Class,
    credit: u64,
    installed: bool,
}
impl Reservation {
    fn install(mut self) -> u64 {
        let mut s = locked(&self.governor.state);
        s.pending -= self.bytes;
        s.next += 1;
        let id = s.next;
        s.entries.insert(
            id,
            Allocation {
                bytes: self.bytes,
                class: self.class,
                work_credit: if self.class == Class::Readback {
                    0
                } else {
                    self.credit
                },
                retired: false,
                serial: 0,
            },
        );
        self.installed = true;
        id
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.installed {
            let mut s = locked(&self.governor.state);
            s.pending -= self.bytes;
            if self.class == Class::Readback {
                s.readback_reservation += self.credit;
            } else {
                s.work += self.credit;
            }
        }
    }
}
#[derive(Default)]
struct Registry {
    devices: HashMap<wgpu::Device, Weak<Governor>>,
    buffers: HashMap<wgpu::Buffer, (Weak<Governor>, u64)>,
    textures: HashMap<wgpu::Texture, (Weak<Governor>, u64)>,
}
#[cfg(not(target_arch = "wasm32"))]
fn registry<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    static REGISTRY: std::sync::OnceLock<Mutex<Registry>> = std::sync::OnceLock::new();
    f(&mut locked(
        REGISTRY.get_or_init(|| Mutex::new(Registry::default())),
    ))
}
#[cfg(target_arch = "wasm32")]
fn registry<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    thread_local! { static REGISTRY:std::cell::RefCell<Registry>=std::cell::RefCell::new(Registry::default()); }
    REGISTRY.with(|r| f(&mut r.borrow_mut()))
}
pub(super) fn install(device: &wgpu::Device, g: &Arc<Governor>) {
    registry(|r| {
        r.devices.insert(device.clone(), Arc::downgrade(g));
    });
}
fn governor(device: &wgpu::Device) -> Option<Arc<Governor>> {
    registry(|r| r.devices.get(device).and_then(Weak::upgrade))
}
pub(super) fn create_buffer(
    device: &wgpu::Device,
    desc: &wgpu::BufferDescriptor<'_>,
    class: Class,
    work: bool,
) -> Result<wgpu::Buffer> {
    let reservation = governor(device)
        .map(|g| g.reserve(desc.size, class, work))
        .transpose()?;
    let b = device.create_buffer(desc);
    if let Some(q) = reservation {
        let weak = Arc::downgrade(&q.governor);
        let id = q.install();
        registry(|r| {
            r.buffers.insert(b.clone(), (weak, id));
        });
    }
    Ok(b)
}
pub(super) fn create_texture(
    device: &wgpu::Device,
    desc: &wgpu::TextureDescriptor<'_>,
    class: Class,
) -> Result<wgpu::Texture> {
    let block = desc.format.block_dimensions();
    let bytes = (0..desc.mip_level_count)
        .map(|m| {
            let w = (desc.size.width >> m).max(1).div_ceil(block.0) as u64;
            let h = (desc.size.height >> m).max(1).div_ceil(block.1) as u64;
            w * h
                * desc.size.depth_or_array_layers as u64
                * desc.sample_count as u64
                * desc.format.block_copy_size(None).unwrap_or(4) as u64
        })
        .sum();
    let reservation = governor(device)
        .map(|g| g.reserve(bytes, class, false))
        .transpose()?;
    let t = device.create_texture(desc);
    if let Some(q) = reservation {
        let weak = Arc::downgrade(&q.governor);
        let id = q.install();
        registry(|r| {
            r.textures.insert(t.clone(), (weak, id));
        });
    }
    Ok(t)
}
pub(super) fn destroy_buffer(b: &wgpu::Buffer, complete: bool) {
    if let Some((g, id)) = registry(|r| r.buffers.remove(b))
        && let Some(g) = g.upgrade()
    {
        g.retire(id, complete);
    }
    b.destroy();
}
pub(super) fn destroy_texture(t: &wgpu::Texture, complete: bool) {
    if let Some((g, id)) = registry(|r| r.textures.remove(t))
        && let Some(g) = g.upgrade()
    {
        g.retire(id, complete);
    }
    t.destroy();
}
impl Drop for Governor {
    fn drop(&mut self) {
        registry(|r| {
            r.devices.retain(|_, g| !std::ptr::eq(g.as_ptr(), self));
            r.buffers.retain(|b, (g, _)| {
                let keep = !std::ptr::eq(g.as_ptr(), self);
                if !keep {
                    b.destroy();
                }
                keep
            });
            r.textures.retain(|t, (g, _)| {
                let keep = !std::ptr::eq(g.as_ptr(), self);
                if !keep {
                    t.destroy();
                }
                keep
            });
        });
    }
}

/// Roll back only allocations created inside a synchronous, failed operation.
/// The caller drops its unsubmitted encoder before rolling back, then flushes queue writes.
pub(super) struct Checkpoint {
    governor: Arc<Governor>,
    after: u64,
}
impl Governor {
    pub fn checkpoint(self: &Arc<Self>) -> Checkpoint {
        Checkpoint {
            governor: self.clone(),
            after: locked(&self.state).next,
        }
    }
}
impl Checkpoint {
    pub fn rollback(self, queue: &wgpu::Queue) {
        queue.submit(std::iter::empty());
        let (buffers, textures) = registry(|r| {
            let owns = |g: &Weak<Governor>, id: u64| {
                std::ptr::eq(g.as_ptr(), Arc::as_ptr(&self.governor)) && id > self.after
            };
            (
                r.buffers
                    .iter()
                    .filter(|(_, (g, id))| owns(g, *id))
                    .map(|(b, _)| b.clone())
                    .collect::<Vec<_>>(),
                r.textures
                    .iter()
                    .filter(|(_, (g, id))| owns(g, *id))
                    .map(|(t, _)| t.clone())
                    .collect::<Vec<_>>(),
            )
        });
        for b in buffers {
            destroy_buffer(&b, false);
        }
        for t in textures {
            destroy_texture(&t, false);
        }
        self.governor.submitted(queue);
    }
}

pub(super) fn checkpoint(device: &wgpu::Device) -> Option<Checkpoint> {
    governor(device).map(|g| g.checkpoint())
}
