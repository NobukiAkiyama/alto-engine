//! Publication certificates are geometric coverage, never alpha coverage.
use super::*;
use std::sync::atomic::AtomicU64;
static EPOCH: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, Debug, Default)]
pub struct DisplayReport {
    /// 0 preparing, 1 holding, 2 coarse/refining, 3 exact.
    pub phase: u32,
    pub generation: u64,
    pub session: u64,
    pub device_epoch: u64,
    pub surface: u64,
    pub requested_level: i32,
    pub admitted_level: i32,
    pub stale: bool,
    pub interaction_ready: bool,
    pub presented: Viewport,
    pub has_presented: bool,
    pub presented_generation: u64,
    pub presented_view_version: u32,
}
struct Overview {
    generation: u64,
    items: Vec<(CacheKey, u32)>,
}
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Signature {
    view: Viewport,
    version: u32,
    generation: u64,
    surface: u64,
    pending: u32,
    root: u64,
}
pub(super) struct Presentation {
    generation: u64,
    session: u64,
    epoch: u64,
    surface: u64,
    active: Option<Overview>,
    root_serial: u64,
    last: Option<Signature>,
    acknowledged: Option<Signature>,
    requested: Viewport,
    version: u32,
    pending: u32,
    requested_level: i32,
    admitted_level: i32,
    pub(super) hierarchy: Option<CacheKey>,
}
impl Default for Presentation {
    fn default() -> Self {
        Self {
            generation: 1,
            session: 1,
            epoch: EPOCH.fetch_add(1, Ordering::Relaxed),
            surface: 1,
            active: None,
            root_serial: 0,
            last: None,
            acknowledged: None,
            requested: Viewport::default(),
            version: 0,
            pending: 1,
            requested_level: 0,
            admitted_level: 0,
            hierarchy: None,
        }
    }
}
impl Presentation {
    pub(super) fn protects(&self, slot: u32) -> bool {
        self.active
            .as_ref()
            .is_some_and(|a| a.items.iter().any(|(_, s)| *s == slot))
    }
    pub(super) fn changed(&mut self) {
        self.generation += 1;
        self.hierarchy = None;
    }
    pub(super) fn resized(&mut self) {
        self.surface += 1;
    }
    pub(super) fn request(&mut self, view: Viewport, requested: i32, admitted: i32) {
        self.requested = view;
        self.requested_level = requested;
        self.admitted_level = admitted;
    }
    pub(super) fn set_pending(&mut self, n: u32) {
        self.pending = n;
    }
    pub(super) fn signature(&self) -> Signature {
        Signature {
            view: self.requested,
            version: self.version,
            generation: self.generation,
            surface: self.surface,
            pending: self.pending,
            root: if self.pending > 0 {
                self.root_serial
            } else {
                0
            },
        }
    }
    pub(super) fn unchanged(&self) -> bool {
        self.last == Some(self.signature())
    }
    pub(super) fn published(&mut self) {
        self.last = Some(self.signature());
    }
    pub(super) fn covers(&self) -> bool {
        self.pending == 0 || self.active.is_some()
    }
    pub(super) fn roots(&self) -> Vec<(CacheKey, u32)> {
        self.active
            .as_ref()
            .map_or_else(Vec::new, |a| a.items.clone())
    }
    pub(super) fn needs_overview(&self) -> bool {
        self.active
            .as_ref()
            .is_none_or(|a| a.generation != self.generation)
    }
    pub(super) fn acknowledge(&mut self) {
        self.acknowledged = self.last;
    }
    fn report(&self, in_flight: bool) -> DisplayReport {
        let displayed = if in_flight {
            self.acknowledged
        } else {
            self.last
        };
        let current = displayed.is_some_and(|s| {
            s.view == self.requested
                && s.version == self.version
                && s.generation == self.generation
                && s.surface == self.surface
        });
        let has = displayed.is_some_and(|s| s.surface == self.surface);
        DisplayReport {
            phase: if !has {
                0
            } else if !current {
                1
            } else if self.pending > 0 {
                2
            } else {
                3
            },
            generation: self.generation,
            session: self.session,
            device_epoch: self.epoch,
            surface: self.surface,
            requested_level: self.requested_level,
            admitted_level: self.admitted_level,
            stale: self.pending > 0
                && self
                    .active
                    .as_ref()
                    .is_some_and(|a| a.generation != self.generation),
            interaction_ready: current && self.pending == 0 && !in_flight,
            presented: displayed.map_or(Viewport::default(), |s| s.view),
            has_presented: has,
            presented_generation: displayed.map_or(0, |s| s.generation),
            presented_view_version: displayed.map_or(0, |s| s.version),
        }
    }
}
impl Renderer {
    pub fn set_view_version(&mut self, version: u32) {
        self.presentation.version = version;
    }
    pub fn display_report(&self) -> DisplayReport {
        self.presentation
            .report(self.present_in_flight.load(Ordering::Acquire))
    }
    pub fn reset_presentation(&mut self) {
        if let Some(old) = self.presentation.active.take() {
            for (_, slot) in old.items {
                if !self.cache_map.values().any(|c| c.slot == slot) {
                    self.cache_slots[slot as usize] = None;
                }
            }
        }
        self.presentation.last = None;
        self.presentation.acknowledged = None;
        self.presentation.session += 1;
        self.presentation.changed();
    }
    pub(super) fn capture_overview(&mut self) {
        if !self.presentation.needs_overview() || self.overview_keys.is_empty() {
            return;
        }
        let items: Option<Vec<_>> = self
            .overview_keys
            .iter()
            .map(|k| self.cache_map.get(k).map(|c| (*k, c.slot)))
            .collect();
        let Some(items) = items else {
            return;
        };
        if let Some(old) = self.presentation.active.replace(Overview {
            generation: self.presentation.generation,
            items,
        }) {
            for (_, slot) in old.items {
                if !self.presentation.protects(slot)
                    && !self.cache_map.values().any(|c| c.slot == slot)
                {
                    self.cache_slots[slot as usize] = None;
                }
            }
        }
        self.presentation.root_serial += 1;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_and_held_views_are_never_interactive() {
        let mut p = Presentation::default();
        assert_eq!(p.report(false).phase, 0);
        p.pending = 0;
        p.published();
        assert!(p.report(false).interaction_ready);
        assert!(!p.report(true).interaction_ready);
        p.requested.zoom = 2.0;
        assert_eq!(p.report(false).phase, 1);
        assert!(!p.report(false).interaction_ready);
        p.resized();
        assert_eq!(p.report(false).phase, 0);
    }
    #[test]
    fn transparent_pixels_do_not_determine_coverage() {
        let mut p = Presentation::default();
        assert!(!p.covers());
        p.active = Some(Overview {
            generation: 1,
            items: vec![((0, 0, 0), 2)],
        });
        assert!(p.covers());
        p.changed();
        assert!(p.covers());
        p.published();
        assert!(p.report(false).stale);
        assert!(!p.report(false).interaction_ready);
        p.pending = 0;
        p.published();
        assert!(!p.report(false).stale);
        assert!(p.report(false).interaction_ready);
    }
}
