//! Live brush strokes: stamping dabs and invalidating the damage.
use super::*;

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
    ) -> Result<()> {
        if self.stroke.is_some() {
            return Err(RenderError::Conflict("stroke already active".into()));
        }
        self.governor.reset_peak();
        self.stroke = Some(GpuStroke {
            layer,
            tiles: BTreeMap::new(),
            color,
            hardness: hardness.clamp(0.0, 0.999),
            opacity: opacity.clamp(0.0, 1.0),
            erase,
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
        for d in dabs {
            let r = d.radius;
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
                    affected
                        .entry(TileKey { tx: x, ty: y })
                        .or_default()
                        .push(GpuDab {
                            center_radius: [d.center[0], d.center[1], r, d.opacity],
                            extra: [0.0; 4],
                        });
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
        let new_bytes = new_tiles as u64 * tile_bytes + command_bytes;
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
        let result = (|| -> Result<()> {
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
                        params: [stroke.hardness, stroke.opacity, 0.0, 0.0],
                        origin: [key.tx as f32 * 256.0, key.ty as f32 * 256.0, 0.0, 0.0],
                        info: [
                            dabs.len() as u32,
                            u32::from(stroke.erase),
                            u32::from(tile.selection.is_some()),
                            0,
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
    pub fn cancel_stroke(&mut self) {
        let Some(stroke) = self.stroke.take() else {
            return;
        };
        let reach = stroke_reach(&stroke);
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
