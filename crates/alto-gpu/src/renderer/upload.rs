//! Uploading decoded tiles into the atlas.
use super::*;

impl Renderer {
    pub(super) fn upload(
        &mut self,
        hash: ContentHash,
        store: &dyn TileStore,
        pinned: &mut HashSet<u32>,
        encoder: &mut wgpu::CommandEncoder,
        previews: &HashMap<ContentHash, wgpu::Texture>,
    ) -> Result<u32> {
        Ok(self.upload_timed(hash, store, pinned, encoder, previews)?.0)
    }
    /// The empty or least recently used atlas slot outside `pinned`, emptied.
    pub(super) fn free_atlas_slot(&mut self, pinned: &HashSet<u32>) -> Result<usize> {
        let slot = self
            .atlas_slots
            .iter()
            .position(Option::is_none)
            .or_else(|| {
                self.atlas_slots
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !pinned.contains(&(*i as u32)))
                    .min_by(|(_, a), (_, b)| {
                        let rank = |s: &Option<AtlasSlot>| {
                            s.as_ref().map(|s| {
                                (
                                    self.atlas_distances
                                        .get(&s.hash)
                                        .copied()
                                        .unwrap_or(f64::INFINITY),
                                    s.used,
                                )
                            })
                        };
                        let (ad, at) = rank(a).unwrap_or((f64::INFINITY, 0));
                        let (bd, bt) = rank(b).unwrap_or((f64::INFINITY, 0));
                        bd.total_cmp(&ad).then(at.cmp(&bt))
                    })
                    .map(|(i, _)| i)
            })
            .ok_or(RenderError::Budget(
                "GPU budget too small for one display tile".into(),
            ))?;
        if let Some(old) = self.atlas_slots[slot].take() {
            if self.profiling {
                self.profile.atlas_evictions += 1;
            }
            self.atlas_map.remove(&old.hash);
        }
        Ok(slot)
    }
    /// Places one source tile in the atlas. `u32::MAX` means the store has no such tile.
    pub(super) fn upload_timed(
        &mut self,
        hash: ContentHash,
        store: &dyn TileStore,
        pinned: &mut HashSet<u32>,
        encoder: &mut wgpu::CommandEncoder,
        previews: &HashMap<ContentHash, wgpu::Texture>,
    ) -> Result<(u32, UploadCost)> {
        let mut cost = UploadCost::default();
        if let Some(&slot) = self.atlas_map.get(&hash) {
            if self.profiling {
                self.profile.atlas_hits += 1;
            }
            self.atlas_slots[slot as usize]
                .as_mut()
                .ok_or(RenderError::Internal("atlas slot".into()))?
                .used = self.frame;
            pinned.insert(slot);
            return Ok((slot, cost));
        }
        let now = self.clock;
        let t = now();
        let pixels = if previews.contains_key(&hash) {
            None
        } else {
            alto_raster::load(store, &hash)?
        };
        cost.get_ms = now() - t;
        if pixels.is_none() && !previews.contains_key(&hash) {
            return Ok((u32::MAX, cost));
        }
        let slot = self.free_atlas_slot(pinned)?;
        let origin = wgpu::Origin3d {
            x: 0,
            y: 0,
            z: slot as u32,
        };
        let extent = wgpu::Extent3d {
            width: 256,
            height: 256,
            depth_or_array_layers: 1,
        };
        if let Some(preview) = previews.get(&hash) {
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: preview,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas,
                    mip_level: 0,
                    origin,
                    aspect: wgpu::TextureAspect::All,
                },
                extent,
            );
        } else if let Some(tile) = pixels {
            let t = now();
            let rgba = rgba_pixels(&tile);
            cost.convert_ms = now() - t;
            let t = now();
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas,
                    mip_level: 0,
                    origin,
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(1024),
                    rows_per_image: Some(256),
                },
                extent,
            );
            cost.write_ms = now() - t;
            cost.bytes = TILE_RGBA_BYTES;
        }
        if self.profiling {
            self.profile.uploads += 1;
            self.profile.get_ms += cost.get_ms;
            self.profile.write_ms += cost.write_ms;
            self.profile.upload_bytes += cost.bytes;
        }
        self.atlas_slots[slot] = Some(AtlasSlot {
            hash,
            used: self.frame,
        });
        self.atlas_map.insert(hash, slot as u32);
        pinned.insert(slot as u32);
        Ok((slot as u32, cost))
    }
}
