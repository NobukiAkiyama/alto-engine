//! Reading composited tiles back from the display cache.
use super::*;

impl Renderer {
    pub async fn read_tile(
        &mut self,
        scene: &RenderScene,
        store: &dyn TileStore,
        key: CacheKey,
    ) -> Result<Vec<u8>> {
        self.read_tiles(scene, store, &[key]).await
    }
    /// Display tiles, 256×256 RGBA premultiplied each, back to back in `keys` order,
    /// with one map for all. Each copy is submitted right after its tile is composed:
    /// composing a later tile may reuse an earlier tile's cache slot, and the queue
    /// runs the copy before that.
    pub async fn read_tiles(
        &mut self,
        scene: &RenderScene,
        store: &dyn TileStore,
        keys: &[CacheKey],
    ) -> Result<Vec<u8>> {
        self.prepare_scene(scene)?;
        const TILE_BYTES: u64 = TILE_RGBA_BYTES;
        let total = TILE_BYTES
            .checked_mul(keys.len() as u64)
            .ok_or(RenderError::Budget("readback size overflow".into()))?;
        if total > 512 * 1048576 {
            return Err(RenderError::Budget("CPU readback exceeds 512 MiB".into()));
        }
        let pool = self
            .readback
            .as_ref()
            .ok_or(RenderError::Internal("readback pool missing".into()))?
            .clone();
        let _lease = pool.acquire()?;
        let mut out = Vec::new();
        out.try_reserve_exact(total as usize)
            .map_err(|_| RenderError::Budget("CPU readback allocation failed".into()))?;
        for batch in keys.chunks((pool.bytes / TILE_BYTES) as usize) {
            for (i, key) in batch.iter().enumerate() {
                self.copy_display_tile(scene, store, *key, &pool.buffer, i as u64 * TILE_BYTES)?;
            }
            pool.map(&self.queue, batch.len() as u64 * TILE_BYTES)
                .await?;
            let view = pool
                .buffer
                .slice(..batch.len() as u64 * TILE_BYTES)
                .get_mapped_range()
                .map_err(|e| RenderError::Internal(e.to_string()))?;
            out.extend_from_slice(&view);
            drop(view);
            pool.unmap();
        }
        Ok(out)
    }
    pub(super) fn copy_display_tile(
        &mut self,
        scene: &RenderScene,
        store: &dyn TileStore,
        key: CacheKey,
        buffer: &wgpu::Buffer,
        offset: u64,
    ) -> Result<()> {
        if !self.cache_map.contains_key(&key) {
            self.compose(scene, key, store, false)?;
        }
        let slot = self
            .cache_map
            .get(&key)
            .ok_or(RenderError::Internal("cache tile".into()))?
            .slot;
        let mut e = self.device.create_command_encoder(&Default::default());
        e.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.cache,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: slot,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset,
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
        self.queue.submit(Some(e.finish()));
        self.release_retired();
        Ok(())
    }
}
