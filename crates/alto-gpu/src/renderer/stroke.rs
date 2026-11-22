//! Live brush strokes: stamping dabs and invalidating the damage.
use super::*;

const MAX_AFFECTED_TILES: usize = 1 << 16;
const MAX_DAB_TILE_ASSOCIATIONS: u64 = 1 << 18;

impl Renderer {
    #[allow(clippy::too_many_arguments)] // One value per brush setting; the Worker passes them positionally.
    pub fn begin_stroke(
        &mut self,
        layer: NodeId,
        color: [f32; 4],
        hardness: f32,
        opacity: f32,
        erase: bool,
        mask: bool,
        world: [f64; 6],
        fixed_world: Option<alto_core::FixedAffine>,
        tip: Option<BrushImage>,
        pattern: Option<BrushPattern>,
    ) -> Result<()> {
        if self.stroke.is_some() {
            return Err(RenderError::Conflict("stroke already active".into()));
        }
        self.governor.reset_peak();
        let preview_id = self.next_preview_id;
        self.next_preview_id = self.next_preview_id.wrapping_add(1);
        self.stroke = Some(GpuStroke {
            tip,
            tip_texture: None,
            pattern: pattern.filter(|pattern| pattern.depth() > 0.0),
            pattern_texture: None,
            layer,
            tiles: BTreeMap::new(),
            color,
            hardness: hardness.clamp(0.0, 0.999),
            opacity: opacity.clamp(0.0, 1.0),
            erase,
            preview_id,
            epoch: 0,
            mask,
            world,
            fixed_world,
            damage: None,
        });
        Ok(())
    }
    pub fn stamp(
        &mut self,
        dabs: &[Dab],
        tiles: &BTreeMap<TileKey, ContentHash>,
        store: &dyn TileStore,
        selection: Option<(&alto_raster::TileMap, [f64; 6], [u32; 2])>,
    ) -> Result<Option<Rect>> {
        if dabs.is_empty() {
            return Ok(None);
        }
        let mut stroke = self
            .stroke
            .take()
            .ok_or(RenderError::Conflict("no stroke".into()))?;
        let mut affected: BTreeMap<TileKey, Vec<GpuDab>> = BTreeMap::new();
        let mut bounds = Rect {
            x0: f64::INFINITY,
            y0: f64::INFINITY,
            x1: f64::NEG_INFINITY,
            y1: f64::NEG_INFINITY,
        };
        let mut projected_associations = 0u64;
        for d in dabs {
            let r = if let Some(tip) = &stroke.tip {
                let [width, height] = tip.dimensions();
                d.radius.max(0.01) * 1.4143 * (1.0 + 1.0 / width.max(height) as f32)
            } else {
                d.radius
            };
            if !d.center.iter().all(|value| value.is_finite()) || !r.is_finite() {
                self.stroke = Some(stroke);
                return Err(RenderError::InvalidArgument("non-finite dab".into()));
            }
            let min_x = ((d.center[0] - r) / 256.0).floor() as f64;
            let max_x = ((d.center[0] + r) / 256.0).floor() as f64;
            let min_y = ((d.center[1] - r) / 256.0).floor() as f64;
            let max_y = ((d.center[1] + r) / 256.0).floor() as f64;
            let bounds = [min_x, max_x, min_y, max_y];
            if !bounds.iter().all(|value| {
                value.is_finite() && (i32::MIN as f64..=i32::MAX as f64).contains(value)
            }) {
                self.stroke = Some(stroke);
                return Err(RenderError::Budget(
                    "stroke dab footprint exceeds the tile budget".into(),
                ));
            }
            let span_x = (max_x - min_x + 1.0) as u64;
            let span_y = (max_y - min_y + 1.0) as u64;
            let Some(associations) = span_x.checked_mul(span_y) else {
                self.stroke = Some(stroke);
                return Err(RenderError::Budget(
                    "stroke dab footprint exceeds the tile budget".into(),
                ));
            };
            projected_associations = match projected_associations.checked_add(associations) {
                Some(total) => total,
                None => {
                    self.stroke = Some(stroke);
                    return Err(RenderError::Budget(
                        "stroke dab footprint exceeds the tile budget".into(),
                    ));
                }
            };
            if projected_associations > MAX_DAB_TILE_ASSOCIATIONS
                || associations > MAX_AFFECTED_TILES as u64
            {
                self.stroke = Some(stroke);
                return Err(RenderError::Budget(
                    "stroke dab footprint exceeds the tile budget".into(),
                ));
            }
        }
        let mut inserted_associations = 0u64;
        for d in dabs {
            // Bilinear filtering has a half-texel margin outside the image.
            let r = if let Some(tip) = &stroke.tip {
                let [width, height] = tip.dimensions();
                d.radius.max(0.01) * 1.4143 * (1.0 + 1.0 / width.max(height) as f32)
            } else {
                d.radius
            };
            let (sin, cos) = if stroke.tip.is_some() || d.roundness < 1.0 {
                d.angle.sin_cos()
            } else {
                (0.0, 1.0)
            };
            bounds.x0 = bounds.x0.min((d.center[0] - r) as f64);
            bounds.y0 = bounds.y0.min((d.center[1] - r) as f64);
            bounds.x1 = bounds.x1.max((d.center[0] + r) as f64);
            bounds.y1 = bounds.y1.max((d.center[1] + r) as f64);
            for y in ((d.center[1] - r) / 256.0).floor() as i32
                ..=((d.center[1] + r) / 256.0).floor() as i32
            {
                for x in ((d.center[0] - r) / 256.0).floor() as i32
                    ..=((d.center[0] + r) / 256.0).floor() as i32
                {
                    let key = TileKey { tx: x, ty: y };
                    if !affected.contains_key(&key) && affected.len() >= MAX_AFFECTED_TILES {
                        self.stroke = Some(stroke);
                        return Err(RenderError::Budget(
                            "stroke affected tile count exceeds the tile budget".into(),
                        ));
                    }
                    if inserted_associations >= MAX_DAB_TILE_ASSOCIATIONS {
                        self.stroke = Some(stroke);
                        return Err(RenderError::Budget(
                            "stroke dab count exceeds the tile budget".into(),
                        ));
                    }
                    affected.entry(key).or_default().push(GpuDab {
                        center_radius: [d.center[0], d.center[1], d.radius, d.opacity],
                        extra: [cos, sin, d.roundness, 0.0],
                    });
                    inserted_associations += 1;
                }
            }
        }
        let new_tiles = affected
            .keys()
            .filter(|k| !stroke.tiles.contains_key(k))
            .count();
        let tile_bytes = 786432 + if selection.is_some() { 262144 } else { 0 };
        let command_bytes = affected
            .values()
            .map(|d| {
                (d.len() * std::mem::size_of::<GpuDab>() + std::mem::size_of::<BrushUniform>())
                    as u64
            })
            .sum::<u64>();
        let pending_tip = stroke.tip.as_ref().filter(|_| stroke.tip_texture.is_none());
        let pending_pattern = stroke
            .pattern
            .as_ref()
            .filter(|_| stroke.pattern_texture.is_none());
        let image_bytes = pending_tip
            .into_iter()
            .chain(pending_pattern.map(BrushPattern::image))
            .map(|image| image.pixels().len() as u64 * 4)
            .sum::<u64>();
        let new_bytes = new_tiles as u64 * tile_bytes + command_bytes + image_bytes;
        let cpu_finish = (stroke.tiles.len() + new_tiles) as u64
            * (TILE_RGBA_BYTES + TILE_RGBA_BYTES / 255 + 256)
            * 4
            + self.readback_capacity()
            + 1048576;
        if self.gpu_bytes().budget_used.saturating_add(new_bytes) > self.budget_bytes
            || cpu_finish > 512 * 1048576
        {
            self.stroke = Some(stroke);
            return Err(RenderError::Budget(
                "stroke would consume its GPU or CPU finish reservation".into(),
            ));
        }
        let checkpoint = self.governor.checkpoint();
        let old_keys: HashSet<_> = stroke.tiles.keys().copied().collect();
        let mut new_tip_texture = None;
        let mut new_pattern_texture = None;
        let result = (|| -> Result<()> {
            if let Some(tip) = pending_tip {
                new_tip_texture = Some(self.upload_brush_image(tip, "brush tip")?);
            }
            if let Some(pattern) = pending_pattern {
                new_pattern_texture =
                    Some(self.upload_brush_image(pattern.image(), "brush texture")?);
            }
            let pattern_view = stroke
                .pattern_texture
                .as_ref()
                .or(new_pattern_texture.as_ref())
                .map(|texture| texture.create_view(&Default::default()));
            let (scale, depth) = stroke
                .pattern
                .as_ref()
                .map_or((0.0, 0.0), |pattern| (pattern.scale(), pattern.depth()));
            let tip_view = stroke
                .tip_texture
                .as_ref()
                .or(new_tip_texture.as_ref())
                .map(|texture| texture.create_view(&Default::default()));
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("brush"),
                });
            let mut selection_tiles = HashMap::new();
            if let Some((m, _, _)) = selection {
                for (key, hash) in &m.tiles {
                    if let Some(tile) = alto_raster::load(store, hash)? {
                        selection_tiles.insert(*key, tile);
                    }
                }
            }
            for (key, dabs) in affected {
                if let std::collections::btree_map::Entry::Vacant(entry) = stroke.tiles.entry(key) {
                    let base = texture(
                        &self.governor,
                        &self.device,
                        "stroke base",
                        1,
                        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    )?;
                    let output = texture(
                        &self.governor,
                        &self.device,
                        "stroke output",
                        1,
                        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                    )?;
                    let coverage = memory::create_buffer(
                        &self.governor,
                        &self.device,
                        &wgpu::BufferDescriptor {
                            label: Some("stroke coverage"),
                            size: 262144,
                            usage: wgpu::BufferUsages::STORAGE,
                            mapped_at_creation: false,
                        },
                        Class::Stroke,
                        false,
                    )?;
                    let before = tiles.get(&key).copied();
                    let previous = before
                        .map(|h| alto_raster::load(store, &h))
                        .transpose()?
                        .flatten();
                    let data = previous.unwrap_or_else(|| {
                        Arc::new(TileData {
                            format: if stroke.mask {
                                alto_core::PixelFormat::R8
                            } else {
                                alto_core::PixelFormat::Rgba8
                            },
                            pixels: vec![0; if stroke.mask { 65536 } else { 262144 }]
                                .into_boxed_slice(),
                        })
                    });
                    self.queue.write_texture(
                        base.as_image_copy(),
                        &rgba_pixels(&data),
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(1024),
                            rows_per_image: Some(256),
                        },
                        wgpu::Extent3d {
                            width: 256,
                            height: 256,
                            depth_or_array_layers: 1,
                        },
                    );
                    let selected = selection
                        .map(|(_, world, size)| -> Result<wgpu::Texture> {
                            let target = texture(
                                &self.governor,
                                &self.device,
                                "stroke selection",
                                1,
                                wgpu::TextureUsages::TEXTURE_BINDING
                                    | wgpu::TextureUsages::COPY_DST,
                            )?;
                            let mut pixels = vec![0u8; 262144];
                            for y in 0..256 {
                                for x in 0..256 {
                                    let p = point(
                                        world,
                                        [
                                            (key.tx * 256 + x) as f64 + 0.5,
                                            (key.ty * 256 + y) as f64 + 0.5,
                                        ],
                                    );
                                    let px = p[0].floor() as i32;
                                    let py = p[1].floor() as i32;
                                    let value = if px < 0
                                        || py < 0
                                        || px >= size[0] as i32
                                        || py >= size[1] as i32
                                    {
                                        0
                                    } else {
                                        selection_tiles
                                            .get(&TileKey {
                                                tx: px.div_euclid(256),
                                                ty: py.div_euclid(256),
                                            })
                                            .map_or(0, |t| {
                                                t.pixels[(py.rem_euclid(256) * 256
                                                    + px.rem_euclid(256))
                                                    as usize]
                                            })
                                    };
                                    pixels[(y * 256 + x) as usize * 4] = value;
                                }
                            }
                            self.queue.write_texture(
                                target.as_image_copy(),
                                &pixels,
                                wgpu::TexelCopyBufferLayout {
                                    offset: 0,
                                    bytes_per_row: Some(1024),
                                    rows_per_image: Some(256),
                                },
                                wgpu::Extent3d {
                                    width: 256,
                                    height: 256,
                                    depth_or_array_layers: 1,
                                },
                            );
                            Ok(target)
                        })
                        .transpose()?;
                    entry.insert(StrokeTile {
                        base,
                        output,
                        coverage,
                        before,
                        selection: selected,
                    });
                }
                let tile = stroke
                    .tiles
                    .get(&key)
                    .ok_or(RenderError::Internal("stroke tile".into()))?;
                let db = buffer(
                    &self.governor,
                    &self.device,
                    &self.queue,
                    "dabs",
                    &dabs,
                    wgpu::BufferUsages::STORAGE,
                )?;
                self.retired.classified_buffer(&db, MemoryClass::Fixed);
                let ub = buffer(
                    &self.governor,
                    &self.device,
                    &self.queue,
                    "brush config",
                    &[BrushUniform {
                        color: stroke.color,
                        params: [stroke.hardness, stroke.opacity, scale, depth],
                        origin: [key.tx as f32 * 256.0, key.ty as f32 * 256.0, 0.0, 0.0],
                        info: [
                            dabs.len() as u32,
                            u32::from(stroke.erase),
                            u32::from(tile.selection.is_some()),
                            u32::from(tip_view.is_some())
                                | (u32::from(pattern_view.is_some()) << 1),
                        ],
                    }],
                    wgpu::BufferUsages::UNIFORM,
                )?;
                self.retired.classified_buffer(&ub, MemoryClass::Fixed);
                let base = tile.base.create_view(&Default::default());
                let output = tile.output.create_view(&Default::default());
                let selected = tile
                    .selection
                    .as_ref()
                    .unwrap_or(&tile.base)
                    .create_view(&Default::default());
                let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("brush"),
                    layout: &self.brush.get_bind_group_layout(0),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&base),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&output),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: tile.coverage.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: db.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: ub.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 5,
                            resource: wgpu::BindingResource::TextureView(&selected),
                        },
                        wgpu::BindGroupEntry {
                            binding: 6,
                            resource: wgpu::BindingResource::TextureView(
                                tip_view.as_ref().unwrap_or(&base),
                            ),
                        },
                        wgpu::BindGroupEntry {
                            binding: 7,
                            resource: wgpu::BindingResource::TextureView(
                                pattern_view.as_ref().unwrap_or(&base),
                            ),
                        },
                    ],
                });
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&self.brush);
                pass.set_bind_group(0, &bg, &[]);
                pass.dispatch_workgroups(16, 16, 1);
            }
            self.queue.submit(Some(encoder.finish()));
            self.release_retired();
            Ok(())
        })();
        if let Err(e) = result {
            stroke.tiles.retain(|key, _| old_keys.contains(key));
            checkpoint.rollback(&self.queue);
            self.release_retired();
            self.stroke = Some(stroke);
            return Err(e);
        }
        if let Some(texture) = new_tip_texture {
            stroke.tip_texture = Some(texture);
        }
        if let Some(texture) = new_pattern_texture {
            stroke.pattern_texture = Some(texture);
        }
        stroke.epoch += 1;
        let damage = crate::damage::layer_rect_damage(stroke.world, bounds);
        stroke.damage = Some(
            stroke
                .damage
                .map_or(damage, |d| crate::damage::union(d, damage)),
        );
        let reach = stroke_reach(&stroke);
        self.stroke = Some(stroke);
        self.invalidate(Some(damage));
        if self.integer_cache
            && let Some((fixed_world, keys)) = reach
        {
            if let Some(fixed) = fixed_world {
                self.invalidate_reach_fixed(fixed, keys);
            } else {
                self.invalidate(None);
            }
        }
        Ok(Some(damage))
    }
    fn upload_brush_image(&self, image: &BrushImage, label: &str) -> Result<wgpu::Texture> {
        let [width, height] = image.dimensions();
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = memory::create_texture(
            &self.governor,
            &self.device,
            &wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            Class::Stroke,
        )?;
        let rgba: Vec<_> = image
            .pixels()
            .iter()
            .flat_map(|&v| [v, 255, 255, 255])
            .collect();
        self.queue.write_texture(
            texture.as_image_copy(),
            &rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            size,
        );
        Ok(texture)
    }

    pub(super) fn retire_brush_images(
        &mut self,
        tip: Option<wgpu::Texture>,
        pattern: Option<wgpu::Texture>,
    ) {
        self.retired.textures.extend(
            tip.into_iter()
                .chain(pattern)
                .map(|texture| (texture, MemoryClass::Stroke)),
        );
    }

    pub fn cancel_stroke(&mut self) {
        let Some(stroke) = self.stroke.take() else {
            return;
        };
        let reach = stroke_reach(&stroke);
        self.retire_brush_images(stroke.tip_texture, stroke.pattern_texture);
        for tile in stroke.tiles.into_values() {
            self.retired
                .textures
                .push((tile.output, MemoryClass::Stroke));
            self.retired.textures.push((tile.base, MemoryClass::Stroke));
            self.retired
                .classified_buffer(&tile.coverage, MemoryClass::Stroke);
            if let Some(t) = tile.selection {
                self.retired.textures.push((t, MemoryClass::Stroke));
            }
        }
        self.release_retired();
        if let Some(damage) = stroke.damage {
            self.invalidate(Some(damage));
        }
        if self.integer_cache
            && let Some((fixed_world, keys)) = reach
        {
            if let Some(fixed) = fixed_world {
                self.invalidate_reach_fixed(fixed, keys);
            } else {
                self.invalidate(None);
            }
        }
    }
    /// Discards the display tiles an edit can change. Jobs always restart: they index
    /// the previous scene.
    pub fn invalidate_damage(&mut self, damage: &crate::damage::Damage) {
        use crate::damage::Damage;
        match damage {
            Damage::None => self.reset_jobs(),
            Damage::Rects(rects) => {
                for r in rects {
                    self.invalidate(Some(*r));
                }
            }
            Damage::Sources { rects, sources } => {
                for r in rects {
                    self.invalidate(Some(*r));
                }
                for s in sources {
                    if let Some(fixed) = s.fixed_world {
                        self.invalidate_reach_fixed(fixed, s.keys);
                    } else {
                        self.invalidate_reach(s.world, s.keys);
                    }
                }
            }
            Damage::All => self.invalidate(None),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn failed_stamp_retires_both_images_and_can_be_retried() {
        let _gpu = crate::cpu_composite::GPU_RENDERER_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(mut renderer) = pollster::block_on(Renderer::headless(&instance, 64)) else {
            return;
        };
        let mut store = alto_raster::ObjectStore::default();
        let corrupt = ContentHash::of(b"invalid tile");
        store.insert_deferred(corrupt, alto_core::PixelFormat::Rgba8, vec![0; 4], false);
        let tiles = BTreeMap::from([(TileKey { tx: 0, ty: 0 }, corrupt)]);
        let dab = Dab {
            center: [128.0; 2],
            radius: 10.0,
            opacity: 1.0,
            angle: 0.0,
            roundness: 1.0,
        };
        let tip = BrushImage::new(17, 19, &[255; 17 * 19]).unwrap();
        let pattern =
            BrushPattern::new(BrushImage::new(19, 17, &[191; 17 * 19]).unwrap(), 1.0, 1.0).unwrap();
        let before = renderer.gpu_bytes().stroke;
        renderer
            .begin_stroke(
                NodeId::new(),
                [0.0, 0.0, 0.0, 1.0],
                0.8,
                1.0,
                false,
                false,
                [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                None,
                Some(tip),
                Some(pattern),
            )
            .unwrap();

        // The tile fails to decode after the tip upload and before the first submit.
        assert!(matches!(
            renderer.stamp(&[dab], &tiles, &store, None),
            Err(RenderError::CorruptData(_))
        ));
        let stroke = renderer.stroke.as_ref().unwrap();
        assert!(
            stroke.tip_texture.is_none()
                && stroke.pattern_texture.is_none()
                && stroke.tiles.is_empty()
        );
        renderer
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        assert_eq!(renderer.gpu_bytes().stroke, before);

        let tiles = BTreeMap::new();
        renderer.stamp(&[dab], &tiles, &store, None).unwrap();
        let texture = renderer
            .stroke
            .as_ref()
            .unwrap()
            .tip_texture
            .clone()
            .unwrap();
        let grain = renderer
            .stroke
            .as_ref()
            .unwrap()
            .pattern_texture
            .clone()
            .unwrap();
        renderer.stamp(&[dab], &tiles, &store, None).unwrap();
        assert_eq!(
            renderer.stroke.as_ref().unwrap().pattern_texture.as_ref(),
            Some(&grain)
        );
        assert_eq!(
            renderer.stroke.as_ref().unwrap().tip_texture.as_ref(),
            Some(&texture)
        );
        renderer.cancel_stroke();
        renderer
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        assert_eq!(renderer.gpu_bytes().stroke, before);
        assert!(!renderer.is_lost());
    }

    #[test]
    fn brush_image_budget_is_checked_before_the_first_stamp() {
        let _gpu = crate::cpu_composite::GPU_RENDERER_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(mut renderer) = pollster::block_on(Renderer::headless(&instance, 64)) else {
            return;
        };
        let store = alto_raster::ObjectStore::default();
        let tiles = BTreeMap::new();
        let dab = Dab {
            center: [128.0; 2],
            radius: 10.0,
            opacity: 1.0,
            angle: 0.0,
            roundness: 1.0,
        };
        let pixels = vec![255; 1024 * 1024];
        let tip = BrushImage::new(1024, 1024, &pixels).unwrap();
        let pattern =
            BrushPattern::new(BrushImage::new(1024, 1024, &pixels).unwrap(), 1.0, 1.0).unwrap();
        let before = renderer.gpu_bytes();
        renderer.budget_bytes = before.budget_used + 6 * 1048576;
        let layer = NodeId::new();
        let world = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

        renderer
            .begin_stroke(
                layer,
                [0.0, 0.0, 0.0, 1.0],
                0.8,
                1.0,
                false,
                false,
                world,
                None,
                Some(tip),
                Some(pattern),
            )
            .unwrap();
        assert_eq!(renderer.gpu_bytes().stroke, before.stroke);
        assert!(matches!(
            renderer.stamp(&[dab], &tiles, &store, None),
            Err(RenderError::Budget(_))
        ));
        let stroke = renderer.stroke.as_ref().unwrap();
        assert!(
            stroke.tip_texture.is_none()
                && stroke.pattern_texture.is_none()
                && stroke.tiles.is_empty()
        );
        assert_eq!(renderer.gpu_bytes().stroke, before.stroke);
        renderer.cancel_stroke();

        renderer
            .begin_stroke(
                layer,
                [0.0, 0.0, 0.0, 1.0],
                0.8,
                1.0,
                false,
                false,
                world,
                None,
                None,
                None,
            )
            .unwrap();
        assert!(
            renderer
                .stamp(&[dab], &tiles, &store, None)
                .unwrap()
                .is_some()
        );
        renderer.cancel_stroke();
    }

    #[test]
    fn oversized_dab_is_rejected_before_building_tile_map() {
        let _gpu = crate::cpu_composite::GPU_RENDERER_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(mut renderer) = pollster::block_on(Renderer::headless(&instance, 64)) else {
            return;
        };
        renderer
            .begin_stroke(
                NodeId::new(),
                [0.0, 0.0, 0.0, 1.0],
                0.8,
                1.0,
                false,
                false,
                [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                None,
                None,
                None,
            )
            .unwrap();
        let dab = Dab {
            center: [0.0, 0.0],
            radius: 1_000_000.0,
            opacity: 1.0,
            angle: 0.0,
            roundness: 1.0,
        };
        assert!(matches!(
            renderer.stamp(
                &[dab],
                &BTreeMap::new(),
                &alto_raster::ObjectStore::default(),
                None
            ),
            Err(RenderError::Budget(_))
        ));
        assert!(
            renderer
                .stroke
                .as_ref()
                .is_some_and(|stroke| stroke.tiles.is_empty())
        );
        renderer.cancel_stroke();
    }
}
