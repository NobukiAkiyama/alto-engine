//! Opt-in GPU timestamps for the passes that composite parent display tiles.
//!
//! Timing is a diagnostic: values never feed pixels, and a pass that cannot be
//! timed renders exactly as it does without the feature.
use super::*;
use std::sync::atomic::{AtomicU8, Ordering};

/// GPU time one parent tile pass took, in nanoseconds. `x`, `y` and `level` are
/// the tile's cache key; a level above zero names the 2x2 children it was
/// downsampled from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileGpuTimestamp {
    pub x: i32,
    pub y: i32,
    pub level: u8,
    pub ns: f64,
}

/// A pass in flight holds its query set until its map completes. Eight is a fixed
/// bound: a burst of parent tiles beyond it is simply left untimed, so panning and
/// drawing never wait for the GPU to catch up.
const CAPTURES: usize = 8;
const QUERY_BYTES: u64 = 16;
const PENDING: u8 = 0;
const MAPPED: u8 = 1;
const FAILED: u8 = 2;

struct Capture {
    queries: wgpu::QuerySet,
    resolved: wgpu::Buffer,
    mapped: wgpu::Buffer,
    state: Arc<AtomicU8>,
    key: Option<CacheKey>,
    /// Disabled while this pass was in flight: its sample is dropped, never mixed
    /// with samples taken after timestamps are enabled again.
    discard: bool,
}

/// The captures of one enabled renderer. Buffers are tiny and fixed; they are
/// outside the texture budget on purpose and never evict or delay a frame.
pub(super) struct GpuTimestamps {
    captures: Vec<Capture>,
    period: f32,
    enabled: bool,
}

#[derive(Clone, Copy)]
pub(super) struct TimestampToken(usize);

impl GpuTimestamps {
    pub(super) fn new(device: &wgpu::Device, period: f32) -> Self {
        let captures = (0..CAPTURES)
            .map(|_| Capture {
                queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("parent tile timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 2,
                }),
                resolved: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("parent tile timestamp values"),
                    size: QUERY_BYTES,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                mapped: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("parent tile timestamp map"),
                    size: QUERY_BYTES,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                state: Arc::new(AtomicU8::new(PENDING)),
                key: None,
                discard: false,
            })
            .collect();
        Self {
            captures,
            period,
            enabled: true,
        }
    }
    pub(super) fn set_enabled(&mut self, enabled: bool) {
        if self.enabled && !enabled {
            // Captures already submitted are not returned; they keep their slot
            // until the map finishes so a new pass cannot reuse a pending buffer.
            for capture in &mut self.captures {
                if capture.key.is_some() {
                    capture.discard = true;
                }
            }
        }
        self.enabled = enabled;
    }
    /// Claims a capture for one parent tile pass, if one is free.
    pub(super) fn begin(&mut self, key: CacheKey) -> Option<TimestampToken> {
        if !self.enabled {
            return None;
        }
        let i = self.captures.iter_mut().position(|capture| {
            if capture.key.is_some() && capture.discard {
                let state = capture.state.load(Ordering::Acquire);
                if state == MAPPED {
                    capture.mapped.unmap();
                }
                if state != PENDING {
                    capture.key = None;
                }
            }
            capture.key.is_none()
        })?;
        let capture = &mut self.captures[i];
        capture.state.store(PENDING, Ordering::Release);
        capture.key = Some(key);
        capture.discard = false;
        Some(TimestampToken(i))
    }
    pub(super) fn writes(&self, token: TimestampToken) -> wgpu::ComputePassTimestampWrites<'_> {
        wgpu::ComputePassTimestampWrites {
            query_set: &self.captures[token.0].queries,
            beginning_of_pass_write_index: Some(0),
            end_of_pass_write_index: Some(1),
        }
    }
    /// Resolves the two timestamps into the capture's map buffer. Called before
    /// the encoder is submitted; nothing here waits for the GPU.
    pub(super) fn resolve(&self, encoder: &mut wgpu::CommandEncoder, token: TimestampToken) {
        let capture = &self.captures[token.0];
        encoder.resolve_query_set(&capture.queries, 0..2, &capture.resolved, 0);
        encoder.copy_buffer_to_buffer(&capture.resolved, 0, &capture.mapped, 0, QUERY_BYTES);
    }
    /// Asks for the map after submission. The callback only flips a flag; the
    /// owner reads the buffer on a later `collect`.
    pub(super) fn submitted(&self, token: TimestampToken) {
        let capture = &self.captures[token.0];
        let state = capture.state.clone();
        capture
            .mapped
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                state.store(
                    if result.is_ok() { MAPPED } else { FAILED },
                    Ordering::Release,
                );
            });
    }
    /// Non-blocking: polls once, reads only captures whose map already finished,
    /// and never waits for the GPU. A failed or unsupported map is dropped.
    pub(super) fn collect(&mut self, device: &wgpu::Device) -> Vec<TileGpuTimestamp> {
        if self.captures.iter().all(|c| c.key.is_none()) {
            return Vec::new();
        }
        let _ = device.poll(wgpu::PollType::Poll);
        let mut out = Vec::new();
        for capture in &mut self.captures {
            let Some(key) = capture.key else {
                continue;
            };
            match capture.state.load(Ordering::Acquire) {
                MAPPED => {}
                FAILED => {
                    // The device is lost or the map was rejected: free the capture.
                    capture.key = None;
                    continue;
                }
                _ => continue,
            }
            let elapsed = if capture.discard {
                None
            } else {
                capture
                    .mapped
                    .slice(..QUERY_BYTES)
                    .get_mapped_range()
                    .ok()
                    .and_then(|view| {
                        let (start, rest) = view.split_first_chunk::<8>()?;
                        let end = rest.first_chunk::<8>()?;
                        u64::from_le_bytes(*end).checked_sub(u64::from_le_bytes(*start))
                    })
            };
            capture.mapped.unmap();
            capture.key = None;
            if let Some(elapsed) = elapsed {
                out.push(TileGpuTimestamp {
                    x: key.1,
                    y: key.2,
                    level: key.0 as u8,
                    ns: elapsed as f64 * f64::from(self.period),
                });
            }
        }
        out
    }
}

impl Renderer {
    /// Opt into GPU timestamps on adapters that support them. A no-op elsewhere:
    /// `take_gpu_timestamps` then returns `None` and rendering is unchanged.
    pub fn set_gpu_timestamps(&mut self, enabled: bool) {
        let Some(period) = self.gpu_timestamp_period else {
            return;
        };
        match &mut self.gpu_timestamps {
            Some(timestamps) => timestamps.set_enabled(enabled),
            None if enabled => {
                self.gpu_timestamps = Some(GpuTimestamps::new(&self.device, period));
            }
            None => {}
        }
    }
    /// Drains completed parent-tile timings. `None` means the adapter has no
    /// timestamp-query support; an empty vector means support but nothing has
    /// finished. Pending maps are left for a later call, never awaited here.
    pub fn take_gpu_timestamps(&mut self) -> Option<Vec<TileGpuTimestamp>> {
        self.gpu_timestamp_period?;
        Some(match &mut self.gpu_timestamps {
            Some(timestamps) => timestamps.collect(&self.device),
            None => Vec::new(),
        })
    }
}
