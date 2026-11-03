//! A single bounded readback buffer per device. Not an estimate of driver VRAM.
use super::*;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc as PoolRef;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc as PoolRef;
pub(super) type SharedPool = PoolRef<Pool>;
pub(super) fn capacity(budget: u64) -> u64 {
    // At least one tile. A 0-byte pool makes `next_batch` take nothing, so
    // `finish_stroke` never drains `remaining`. Budgets under 64 MiB are
    // rejected earlier; this floor does not rely on that.
    let bytes = (8 * 1048576).min(budget / 16);
    (bytes / TILE_RGBA_BYTES).max(1) * TILE_RGBA_BYTES
}
/// How many tiles this pool can copy. Zero is an error: `take(0)` would not
/// drain the stroke, and the finish loop would spin.
fn tile_slots(pool_bytes: u64) -> Result<usize> {
    let count = (pool_bytes / TILE_RGBA_BYTES) as usize;
    if count == 0 {
        Err(RenderError::Internal("readback pool holds no tile".into()))
    } else {
        Ok(count)
    }
}
/// Runs `body`, then `release`, including when `body` returns an error or panics.
/// The mapped view has to be a local of `body` so it is dropped before unmap.
fn release_after<T>(release: impl FnOnce(), body: impl FnOnce() -> Result<T>) -> Result<T> {
    let guard = ReleaseOnDrop(Some(release));
    let result = body();
    drop(guard);
    result
}
struct ReleaseOnDrop<F: FnOnce()>(Option<F>);
impl<F: FnOnce()> Drop for ReleaseOnDrop<F> {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            release();
        }
    }
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
                g,
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
                memory::destroy_buffer(g, &buffer, true);
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
            memory::destroy_buffer(g, &buffer, true);
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
        self.0.unmap();
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
    /// Set after a copy error. A later `next_batch` must not `map_async` again.
    failed: bool,
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
        if self.failed {
            return Err(RenderError::Internal(
                "readback reader already failed".into(),
            ));
        }
        if self.remaining.len() == 0 {
            return Ok(None);
        }
        let completed = self.pool.0.completed.load(Ordering::Acquire);
        // This injection returns before `map_async`. It cannot leave the buffer mapped.
        if self.fail_after.is_some_and(|n| completed >= n) {
            return Err(RenderError::Internal(
                "injected readback batch failure".into(),
            ));
        }
        let count = match tile_slots(self.pool.0.bytes) {
            Ok(count) => count,
            Err(e) => {
                self.failed = true;
                return Err(e);
            }
        };
        let batch = PendingBatch {
            tiles: self.remaining.by_ref().take(count).collect(),
            governor: self.governor.clone(),
            queue: self.queue.clone(),
            complete: false,
        };
        if batch.tiles.is_empty() {
            self.failed = true;
            return Err(RenderError::Internal(
                "readback batch made no progress".into(),
            ));
        }
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
        let map_result = self
            .pool
            .0
            .map_controlled(
                &self.queue,
                batch.tiles.len() as u64 * TILE_RGBA_BYTES,
                Some(&self.control),
            )
            .await;
        if let Err(e) = map_result {
            // map_controlled unmaps on failure. The batch tiles are dropped with
            // this error, so a retry must not continue the stroke.
            self.failed = true;
            return Err(e);
        }
        let mut batch = batch;
        batch.complete = true;
        let copied = {
            let pool = &self.pool.0;
            let mask = self.mask;
            let tiles = &batch.tiles;
            // `fail_after` never reaches here. Unmap on every exit, then poison the
            // reader so a later `next_batch` cannot `map_async` a mapped buffer.
            release_after(
                || pool.unmap(),
                || {
                    let mapped = pool
                        .buffer
                        .slice(..tiles.len() as u64 * TILE_RGBA_BYTES)
                        .get_mapped_range()
                        .map_err(|e| RenderError::Internal(e.to_string()))?;
                    let out = tiles_from_mapped(&mapped, tiles, mask);
                    drop(mapped);
                    out
                },
            )
        };
        let out = match copied {
            Ok(out) => out,
            Err(e) => {
                self.failed = true;
                return Err(e);
            }
        };
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
        memory::destroy_texture(&self.governor, &tile.output, false);
        memory::destroy_texture(&self.governor, &tile.base, false);
        memory::destroy_buffer(&self.governor, &tile.coverage, false);
        if let Some(t) = tile.selection {
            memory::destroy_texture(&self.governor, &t, false);
        }
        self.governor.submitted(&self.queue);
    }
}
fn tiles_from_mapped(
    mapped: &[u8],
    tiles: &[(TileKey, StrokeTile)],
    mask: bool,
) -> Result<Vec<(TileKey, Option<ContentHash>, TileData)>> {
    let mut out = Vec::with_capacity(tiles.len());
    for (i, (key, tile)) in tiles.iter().enumerate() {
        let rgba = &mapped[i * TILE_RGBA_BYTES as usize..(i + 1) * TILE_RGBA_BYTES as usize];
        let data = TileData::new(
            if mask {
                alto_core::PixelFormat::R8
            } else {
                alto_core::PixelFormat::Rgba8
            },
            if mask {
                rgba.as_chunks::<4>().0.iter().map(|p| p[0]).collect()
            } else {
                rgba.to_vec()
            },
        )
        .map_err(|e| RenderError::CorruptData(e.to_string()))?;
        out.push((*key, tile.before, data));
    }
    Ok(out)
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
            memory::destroy_texture(&self.governor, &tile.output, self.complete);
            memory::destroy_texture(&self.governor, &tile.base, self.complete);
            memory::destroy_buffer(&self.governor, &tile.coverage, self.complete);
            if let Some(t) = tile.selection {
                memory::destroy_texture(&self.governor, &t, self.complete);
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
    pub(super) async fn map(&self, queue: &wgpu::Queue, bytes: u64) -> Result<()> {
        self.map_controlled(queue, bytes, None).await
    }
    async fn map_controlled(
        &self,
        queue: &wgpu::Queue,
        bytes: u64,
        control: Option<&FinishControl>,
    ) -> Result<()> {
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
        let mut timeout = deadline::MapDeadline::new();
        queue.on_submitted_work_done(timeout.queue_finished());
        let result = std::future::poll_fn(|cx| {
            if let Some(c) = control {
                *c.1.lock().unwrap() = Some(cx.waker().clone());
                if c.cancelled() {
                    return Poll::Ready(Err(RenderError::Conflict(
                        "stroke cancelled before commit".into(),
                    )));
                }
            }
            if let Some(reason) = timeout.expired(cx.waker()) {
                return Poll::Ready(Err(RenderError::Internal(reason.into())));
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
            self.unmap();
        }
        result
    }
    /// Unmaps once per `map`. Native wgpu reports a second unmap as an uncaptured
    /// validation error, which marks the renderer lost. `mapped` is set before
    /// `map_async`, so a pending map is cancelled here too.
    pub(super) fn unmap(&self) {
        if self.mapped.swap(0, Ordering::AcqRel) != 0 {
            self.buffer.unmap();
        }
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
            failed: false,
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
    #[cfg(not(target_arch = "wasm32"))]
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::Waker;
    use std::time::Duration;
    pub(super) const MAP_TIMED_OUT: &str = "readback map timed out after 30 seconds";
    pub(super) const QUEUE_TIMED_OUT: &str = "readback waited 120 seconds for the GPU queue";
    /// Bounds one readback map. `map_async` resolves after every earlier submission, and
    /// a software adapter compiles pipelines before that work: after GPU recovery the
    /// queue can be busy for most of 30 seconds on a slow host. So the map's own limit
    /// starts when the queue finishes, and a longer limit covers a queue that never does.
    pub(super) struct MapDeadline {
        queue: Deadline,
        map_limit: Duration,
        map: Option<Deadline>,
        finished: Arc<Mutex<(bool, Option<Waker>)>>,
    }
    impl MapDeadline {
        pub(super) fn new() -> Self {
            Self::with_limits(Duration::from_secs(120), Duration::from_secs(30))
        }
        pub(super) fn with_limits(queue: Duration, map: Duration) -> Self {
            Self {
                queue: Deadline::after(queue),
                map_limit: map,
                map: None,
                finished: Arc::new(Mutex::new((false, None))),
            }
        }
        /// For `Queue::on_submitted_work_done`.
        pub(super) fn queue_finished(&self) -> impl FnOnce() + Send + 'static {
            let finished = self.finished.clone();
            move || {
                let mut s = finished.lock().unwrap();
                s.0 = true;
                if let Some(w) = s.1.take() {
                    w.wake();
                }
            }
        }
        pub(super) fn expired(&mut self, w: &Waker) -> Option<&'static str> {
            if self.map.is_none() {
                let mut s = self.finished.lock().unwrap();
                if s.0 {
                    self.map = Some(Deadline::after(self.map_limit));
                } else {
                    s.1 = Some(w.clone());
                }
            }
            match &self.map {
                Some(map) => map.expired(w).then_some(MAP_TIMED_OUT),
                None => self.queue.expired(w).then_some(QUEUE_TIMED_OUT),
            }
        }
    }
    pub(super) struct Deadline {
        state: Arc<Mutex<(bool, Option<Waker>)>>,
        /// Set on drop so the waiter exits without reporting a timeout.
        /// `park_timeout` can return early; the thread loops until this flag
        /// or the real deadline. Up to two threads per map is the existing shape:
        /// finish maps a few batches, and drop unparks them when the map ends.
        #[cfg(not(target_arch = "wasm32"))]
        stop: Arc<AtomicBool>,
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
        pub(super) fn after(limit: Duration) -> Self {
            let state = Arc::new(Mutex::new((false, None::<Waker>)));
            let signal = state.clone();
            #[cfg(not(target_arch = "wasm32"))]
            {
                let stop = Arc::new(AtomicBool::new(false));
                let stop_flag = stop.clone();
                let thread = std::thread::spawn(move || {
                    let deadline = std::time::Instant::now() + limit;
                    loop {
                        if stop_flag.load(Ordering::Acquire) {
                            return;
                        }
                        let remaining =
                            deadline.saturating_duration_since(std::time::Instant::now());
                        if remaining.is_zero() {
                            break;
                        }
                        std::thread::park_timeout(remaining);
                    }
                    if stop_flag.load(Ordering::Acquire) {
                        return;
                    }
                    let mut s = signal.lock().unwrap();
                    s.0 = true;
                    if let Some(w) = s.1.take() {
                        w.wake();
                    }
                });
                Self {
                    state,
                    stop,
                    thread,
                }
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
                let timer =
                    set_timeout(callback.as_ref().unchecked_ref(), limit.as_millis() as i32);
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
        #[cfg(all(test, not(target_arch = "wasm32")))]
        pub(super) fn wake_thread_for_test(&self) {
            self.thread.thread().unpark();
        }
    }
    impl Drop for Deadline {
        fn drop(&mut self) {
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.stop.store(true, Ordering::Release);
                self.thread.thread().unpark();
            }
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
    #[test]
    fn tiny_budget_readback_capacity_is_one_tile() {
        // `budget / 16` below one tile used to truncate to 0 bytes.
        for budget in [0, 1, TILE_RGBA_BYTES - 1, 16 * TILE_RGBA_BYTES - 1] {
            assert_eq!(capacity(budget), TILE_RGBA_BYTES, "budget {budget}");
        }
        assert_eq!(capacity(16 * TILE_RGBA_BYTES), TILE_RGBA_BYTES);
        assert!(tile_slots(0).is_err());
        assert!(tile_slots(TILE_RGBA_BYTES - 1).is_err());
        assert_eq!(tile_slots(TILE_RGBA_BYTES).unwrap(), 1);
        assert_eq!(tile_slots(32 * TILE_RGBA_BYTES).unwrap(), 32);
    }
    #[test]
    fn mapped_copy_releases_the_buffer_when_decode_fails() {
        // The injected readback fault returns before map_async, so it cannot
        // observe a buffer left mapped. Copy errors go through release_after.
        let releases = std::cell::Cell::new(0);
        let failed = release_after(
            || releases.set(releases.get() + 1),
            || Err::<(), _>(RenderError::CorruptData("tile".into())),
        );
        assert!(failed.is_err());
        assert_eq!(releases.get(), 1);
        let ok = release_after(|| releases.set(releases.get() + 1), || Ok(4));
        assert_eq!(ok.unwrap(), 4);
        assert_eq!(releases.get(), 2);
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn deadline_waits_through_a_spurious_wake() {
        let deadline = super::deadline::Deadline::after(std::time::Duration::from_secs(30));
        std::thread::sleep(std::time::Duration::from_millis(50));
        deadline.wake_thread_for_test();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            !deadline.expired(std::task::Waker::noop()),
            "a single wake was treated as the 30s map timeout"
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn poll_until_expired(deadline: &mut super::deadline::MapDeadline) -> Option<&'static str> {
        let stop = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < stop {
            if let Some(reason) = deadline.expired(std::task::Waker::noop()) {
                return Some(reason);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn map_window_starts_when_the_queue_finishes() {
        use std::time::Duration;
        // A software adapter compiles pipelines before later queue work, so a long
        // queue must not use up the time the map itself is given.
        let mut deadline = super::deadline::MapDeadline::with_limits(
            Duration::from_secs(60),
            Duration::from_millis(50),
        );
        let finished = deadline.queue_finished();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            deadline.expired(std::task::Waker::noop()),
            None,
            "the map window ran while the queue was still busy"
        );
        finished();
        assert_eq!(
            poll_until_expired(&mut deadline),
            Some(super::deadline::MAP_TIMED_OUT),
            "the map window did not run after the queue finished"
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_queue_that_never_finishes_hits_the_total_limit() {
        use std::time::Duration;
        let mut deadline = super::deadline::MapDeadline::with_limits(
            Duration::from_millis(50),
            Duration::from_secs(60),
        );
        assert_eq!(
            poll_until_expired(&mut deadline),
            Some(super::deadline::QUEUE_TIMED_OUT)
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn queue_finish_wakes_the_waiter() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;
        struct Flag(AtomicBool);
        impl std::task::Wake for Flag {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::Release);
            }
        }
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let waker = std::task::Waker::from(flag.clone());
        let mut deadline = super::deadline::MapDeadline::with_limits(
            Duration::from_secs(60),
            Duration::from_secs(60),
        );
        assert_eq!(deadline.expired(&waker), None);
        deadline.queue_finished()();
        assert!(
            flag.0.load(Ordering::Acquire),
            "nothing would poll the map again to start its window"
        );
    }
}
