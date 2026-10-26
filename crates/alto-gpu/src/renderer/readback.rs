//! A single bounded readback buffer per device. Not an estimate of driver VRAM.
use super::*;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc as PoolRef;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc as PoolRef;
pub(super) type SharedPool = PoolRef<Pool>;
pub(super) fn capacity(budget: u64) -> u64 {
    (8 * 1048576).min(budget / 16) / TILE_RGBA_BYTES * TILE_RGBA_BYTES
}
pub(super) struct Pool {
    pub buffer: wgpu::Buffer,
    pub bytes: u64,
    busy: AtomicBool,
    batches: AtomicU64,
    completed: AtomicU64,
    mapped: AtomicU64,
    #[cfg(not(target_arch = "wasm32"))]
    _poller: NativePoller,
}
impl Pool {
    pub(super) fn shared(self) -> SharedPool {
        PoolRef::new(self)
    }
    pub async fn new(device: &wgpu::Device, g: &Arc<Governor>) -> Result<Self> {
        let mut bytes = capacity(g.snapshot().budget_limit);
        loop {
            g.shrink_readback(bytes);
            let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
            let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let buffer = memory::create_buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("bounded readback pool"),
                    size: bytes,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
                Class::Readback,
                false,
            )?;
            let v = validation.pop();
            let o = oom.pop();
            if let Some(e) = v.await {
                memory::destroy_buffer(&buffer, true);
                return Err(RenderError::Internal(e.to_string()));
            }
            if o.await.is_none() {
                return Ok(Self {
                    buffer,
                    bytes,
                    busy: AtomicBool::new(false),
                    batches: AtomicU64::new(0),
                    completed: AtomicU64::new(0),
                    mapped: AtomicU64::new(0),
                    #[cfg(not(target_arch = "wasm32"))]
                    _poller: NativePoller::new(device.clone()),
                });
            }
            memory::destroy_buffer(&buffer, true);
            if bytes == TILE_RGBA_BYTES {
                return Err(RenderError::Budget(
                    "cannot allocate one readback tile".into(),
                ));
            }
            bytes = (bytes / 2 / TILE_RGBA_BYTES).max(1) * TILE_RGBA_BYTES;
        }
    }
}

use std::sync::atomic::{AtomicU8, AtomicU64};
#[derive(Default)]
pub struct FinishControl(
    Arc<AtomicU8>,
    Arc<std::sync::Mutex<Option<std::task::Waker>>>,
);
impl Clone for FinishControl {
    fn clone(&self) -> Self {
        Self(self.0.clone(), self.1.clone())
    }
}
impl FinishControl {
    pub fn cancel(&self) -> bool {
        let changed = self
            .0
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if let Some(w) = self.1.lock().unwrap().take() {
            w.wake();
        }
        changed
    }
    pub fn begin(&self) {
        self.0.store(1, Ordering::Release);
    }
    pub fn commit(&self) -> bool {
        self.0
            .compare_exchange(1, 3, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire) == 2
    }
}
pub(super) struct Lease(SharedPool);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.buffer.unmap();
        self.0.mapped.store(0, Ordering::Release);
        self.0.busy.store(false, Ordering::Release);
    }
}
#[derive(Default, Clone, Copy, Debug)]
pub struct ReadbackStats {
    pub capacity: u64,
    pub batches: u64,
    pub completed: u64,
    pub mapped: u64,
}
pub struct StrokeReadback {
    pool: Lease,
    device: wgpu::Device,
    queue: wgpu::Queue,
    governor: Arc<Governor>,
    remaining: std::collections::btree_map::IntoIter<TileKey, StrokeTile>,
    control: FinishControl,
    mask: bool,
    pub damage: Option<Rect>,
    fail_after: Option<u64>,
}
impl StrokeReadback {
    pub async fn next_batch(
        &mut self,
    ) -> Result<Option<Vec<(TileKey, Option<ContentHash>, TileData)>>> {
        if self.control.cancelled() {
            return Err(RenderError::Conflict(
                "stroke cancelled before commit".into(),
            ));
        }
        if self.remaining.len() == 0 {
            return Ok(None);
        }
        let completed = self.pool.0.completed.load(Ordering::Acquire);
        if self.fail_after.is_some_and(|n| completed >= n) {
            return Err(RenderError::Internal(
                "injected readback batch failure".into(),
            ));
        }
        let count = (self.pool.0.bytes / TILE_RGBA_BYTES) as usize;
        let batch = PendingBatch {
            tiles: self.remaining.by_ref().take(count).collect(),
            governor: self.governor.clone(),
            queue: self.queue.clone(),
            complete: false,
        };
        let mut e = self.device.create_command_encoder(&Default::default());
        for (i, (_, tile)) in batch.tiles.iter().enumerate() {
            e.copy_texture_to_buffer(
                tile.output.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &self.pool.0.buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: i as u64 * TILE_RGBA_BYTES,
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
        }
        self.queue.submit(Some(e.finish()));
        self.pool
            .0
            .map_controlled(
                batch.tiles.len() as u64 * TILE_RGBA_BYTES,
                Some(&self.control),
            )
            .await?;
        let mut batch = batch;
        batch.complete = true;
        let mapped = self
            .pool
            .0
            .buffer
            .slice(..batch.tiles.len() as u64 * TILE_RGBA_BYTES)
            .get_mapped_range()
            .map_err(|e| RenderError::Internal(e.to_string()))?;
        let mut out = Vec::with_capacity(batch.tiles.len());
        for (i, (key, tile)) in batch.tiles.iter().enumerate() {
            let rgba = &mapped[i * TILE_RGBA_BYTES as usize..(i + 1) * TILE_RGBA_BYTES as usize];
            let data = TileData::new(
                if self.mask {
                    alto_core::PixelFormat::R8
                } else {
                    alto_core::PixelFormat::Rgba8
                },
                if self.mask {
                    rgba.as_chunks::<4>().0.iter().map(|p| p[0]).collect()
                } else {
                    rgba.to_vec()
                },
            )
            .map_err(|e| RenderError::CorruptData(e.to_string()))?;
            out.push((*key, tile.before, data));
        }
        drop(mapped);
        self.pool.0.buffer.unmap();
        self.pool.0.mapped.store(0, Ordering::Release);
        drop(batch);
        self.pool
            .0
            .completed
            .fetch_add(out.len() as u64, Ordering::AcqRel);
        if self.control.cancelled() {
            return Err(RenderError::Conflict(
                "stroke cancelled before commit".into(),
            ));
        }
        Ok(Some(out))
    }
    fn retire_tile(&self, tile: StrokeTile) {
        memory::destroy_texture(&tile.output, false);
        memory::destroy_texture(&tile.base, false);
        memory::destroy_buffer(&tile.coverage, false);
        if let Some(t) = tile.selection {
            memory::destroy_texture(&t, false);
        }
        self.governor.submitted(&self.queue);
    }
}
struct PendingBatch {
    tiles: Vec<(TileKey, StrokeTile)>,
    governor: Arc<Governor>,
    queue: wgpu::Queue,
    complete: bool,
}
impl Drop for PendingBatch {
    fn drop(&mut self) {
        for (_, tile) in self.tiles.drain(..) {
            memory::destroy_texture(&tile.output, self.complete);
            memory::destroy_texture(&tile.base, self.complete);
            memory::destroy_buffer(&tile.coverage, self.complete);
            if let Some(t) = tile.selection {
                memory::destroy_texture(&t, self.complete);
            }
        }
        self.governor.submitted(&self.queue);
    }
}
impl Drop for StrokeReadback {
    fn drop(&mut self) {
        while let Some((_, tile)) = self.remaining.next() {
            self.retire_tile(tile);
        }
    }
}
impl Pool {
    pub fn acquire(self: &SharedPool) -> Result<Lease> {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| RenderError::Conflict("readback pool busy".into()))?;
        Ok(Lease(self.clone()))
    }
    pub fn stats(&self) -> ReadbackStats {
        ReadbackStats {
            capacity: self.bytes,
            batches: self.batches.load(Ordering::Acquire),
            completed: self.completed.load(Ordering::Acquire),
            mapped: self.mapped.load(Ordering::Acquire),
        }
    }
    pub(super) async fn map(&self, bytes: u64) -> Result<()> {
        self.map_controlled(bytes, None).await
    }
    async fn map_controlled(&self, bytes: u64, control: Option<&FinishControl>) -> Result<()> {
        use std::{future::Future, task::Poll};
        self.mapped.store(bytes, Ordering::Release);
        self.batches.fetch_add(1, Ordering::AcqRel);
        let (tx, rx) = futures_channel::oneshot::channel();
        self.buffer
            .slice(..bytes)
            .map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
        let mut rx = std::pin::pin!(rx);
        let timeout = deadline::Deadline::new();
        let result = std::future::poll_fn(|cx| {
            if let Some(c) = control {
                *c.1.lock().unwrap() = Some(cx.waker().clone());
                if c.cancelled() {
                    return Poll::Ready(Err(RenderError::Conflict(
                        "stroke cancelled before commit".into(),
                    )));
                }
            }
            if timeout.expired(cx.waker()) {
                return Poll::Ready(Err(RenderError::Internal(
                    "readback map timed out after 30 seconds".into(),
                )));
            }
            rx.as_mut().poll(cx).map(|r| {
                r.map_err(|e| RenderError::Internal(e.to_string()))?
                    .map_err(|e| RenderError::Internal(format!("readback map: {e}")))
            })
        })
        .await;
        if let Some(c) = control {
            c.1.lock().unwrap().take();
        }
        if result.is_err() {
            self.buffer.unmap();
            self.mapped.store(0, Ordering::Release);
        }
        result
    }
}
impl Renderer {
    pub fn readback_capacity(&self) -> u64 {
        self.readback.as_ref().map_or(0, |p| p.bytes)
    }
    pub fn readback_stats(&self) -> ReadbackStats {
        self.readback
            .as_ref()
            .map_or(ReadbackStats::default(), |p| p.stats())
    }
    pub fn begin_finish_stroke(&mut self, control: FinishControl) -> Result<StrokeReadback> {
        let pool = self
            .readback
            .as_ref()
            .ok_or(RenderError::Internal("readback pool missing".into()))?
            .acquire()?;
        let stroke = self
            .stroke
            .take()
            .ok_or(RenderError::Conflict("no stroke".into()))?;
        pool.0.completed.store(0, Ordering::Release);
        pool.0.batches.store(0, Ordering::Release);
        control.begin();
        Ok(StrokeReadback {
            pool,
            device: self.device.clone(),
            queue: self.queue.clone(),
            governor: self.governor.clone(),
            remaining: stroke.tiles.into_iter(),
            control,
            mask: stroke.mask,
            damage: stroke.damage,
            fail_after: self.readback_fault.take(),
        })
    }
    pub fn inject_readback_failure(&mut self, after_tiles: Option<u64>) {
        self.readback_fault = after_tiles;
    }
    pub async fn finish_stroke(&mut self) -> Result<Vec<(TileKey, Option<ContentHash>, TileData)>> {
        let total = self.stroke.as_ref().map_or(0, |s| s.tiles.len());
        if total as u64 * TILE_RGBA_BYTES > 512 * 1048576 {
            return Err(RenderError::Budget(
                "stroke CPU readback exceeds 512 MiB".into(),
            ));
        }
        let mut reader = self.begin_finish_stroke(FinishControl::default())?;
        let mut out = Vec::new();
        while let Some(mut tiles) = reader.next_batch().await? {
            out.append(&mut tiles);
        }
        if let Some(d) = reader.damage {
            self.invalidate(Some(d));
        }
        Ok(out)
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct NativePoller(Arc<AtomicBool>);
#[cfg(not(target_arch = "wasm32"))]
impl NativePoller {
    fn new(device: wgpu::Device) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                let _ = device.poll(wgpu::PollType::Poll);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        });
        Self(stop)
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl Drop for NativePoller {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

mod deadline {
    use std::sync::{Arc, Mutex};
    use std::task::Waker;
    pub(super) struct Deadline {
        state: Arc<Mutex<(bool, Option<Waker>)>>,
        #[cfg(not(target_arch = "wasm32"))]
        thread: std::thread::JoinHandle<()>,
        #[cfg(target_arch = "wasm32")]
        timer: i32,
        #[cfg(target_arch = "wasm32")]
        _callback: wasm_bindgen::closure::Closure<dyn FnMut()>,
    }
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen::prelude::wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name=setTimeout)]
        fn set_timeout(f: &js_sys::Function, ms: i32) -> i32;
        #[wasm_bindgen(js_name=clearTimeout)]
        fn clear_timeout(id: i32);
    }
    impl Deadline {
        pub(super) fn new() -> Self {
            let state = Arc::new(Mutex::new((false, None::<Waker>)));
            let signal = state.clone();
            #[cfg(not(target_arch = "wasm32"))]
            {
                let thread = std::thread::spawn(move || {
                    std::thread::park_timeout(std::time::Duration::from_secs(30));
                    let mut s = signal.lock().unwrap();
                    s.0 = true;
                    if let Some(w) = s.1.take() {
                        w.wake();
                    }
                });
                Self { state, thread }
            }
            #[cfg(target_arch = "wasm32")]
            {
                use wasm_bindgen::JsCast;
                let callback = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
                    let mut s = signal.lock().unwrap();
                    s.0 = true;
                    if let Some(w) = s.1.take() {
                        w.wake();
                    }
                })
                    as Box<dyn FnMut()>);
                let timer = set_timeout(callback.as_ref().unchecked_ref(), 30000);
                Self {
                    state,
                    timer,
                    _callback: callback,
                }
            }
        }
        pub(super) fn expired(&self, w: &Waker) -> bool {
            let mut s = self.state.lock().unwrap();
            s.1 = Some(w.clone());
            s.0
        }
    }
    impl Drop for Deadline {
        fn drop(&mut self) {
            #[cfg(not(target_arch = "wasm32"))]
            self.thread.thread().unpark();
            #[cfg(target_arch = "wasm32")]
            clear_timeout(self.timer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capacity_and_commit_cancellation_boundary_are_fixed() {
        for (mb, tiles) in [(64, 16), (128, 32), (256, 32), (512, 32)] {
            assert_eq!(capacity(mb * 1048576), tiles * TILE_RGBA_BYTES);
        }
        let c = FinishControl::default();
        c.begin();
        let sibling = c.clone();
        assert!(sibling.cancel());
        assert!(!c.cancel());
        assert!(!c.commit());
        c.begin();
        assert!(c.commit());
        assert!(!sibling.cancel());
    }
}
