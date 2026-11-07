//! Building and submitting the composite commands for a cached tile.
use super::*;

impl Renderer {
    pub(super) fn commands(
        &mut self,
        scene: &RenderScene,
        key: CacheKey,
        store: &dyn TileStore,
        encoder: &mut wgpu::CommandEncoder,
    ) -> Result<(Vec<Command>, Vec<Entry>)> {
        let mut previews = HashMap::new();
        let mut overrides = BTreeMap::new();
        if let Some(stroke) = &self.stroke
            && !stroke.mask
        {
            for (k, t) in &stroke.tiles {
                let hash = ContentHash::of(
                    format!(
                        "preview:{:?}:{},{}:{}",
                        stroke.layer, k.tx, k.ty, stroke.epoch
                    )
                    .as_bytes(),
                );
                previews.insert(hash, t.output.clone());
                overrides.insert((stroke.layer, *k), hash);
            }
        }
        let mut commands = Vec::new();
        let mut entries = Vec::new();
        let mut pinned = HashSet::new();
        let s = 2f64.powi(key.0);
        // The composite shader writes only inside the canvas, so content outside it is
        // never sampled. Clipping here also lets opaque layers hide lower ones in edge tiles.
        let region = [
            (key.1 as f64 * 256.0 * s).max(scene.canvas.x0 as f64),
            (key.2 as f64 * 256.0 * s).max(scene.canvas.y0 as f64),
            ((key.1 + 1) as f64 * 256.0 * s).min(scene.canvas.x1 as f64),
            ((key.2 + 1) as f64 * 256.0 * s).min(scene.canvas.y1 as f64),
        ];
        struct Build<'a> {
            scene: &'a RenderScene,
            key: CacheKey,
            region: [f64; 4],
            overrides: &'a BTreeMap<(NodeId, TileKey), ContentHash>,
        }
        #[allow(clippy::too_many_arguments)] // Recursive scene compiler keeps GPU upload and plan outputs in one traversal.
        fn walk(
            r: &mut Renderer,
            b: &Build<'_>,
            idx: usize,
            world: WorldTransform,
            depth: u32,
            commands: &mut Vec<Command>,
            entries: &mut Vec<Entry>,
            store: &dyn TileStore,
            encoder: &mut wgpu::CommandEncoder,
            pinned: &mut HashSet<u32>,
            previews: &HashMap<ContentHash, wgpu::Texture>,
        ) -> Result<()> {
            let n = b
                .scene
                .nodes
                .get(idx)
                .ok_or(RenderError::Internal("scene node".into()))?;
            if !n.visible || n.opacity <= 0.0 {
                return Ok(());
            }
            let world = world.concat(n.transform)?;
            match &n.content {
                RContent::Group { children, isolated } => {
                    let isolate = *isolated || n.opacity < 1.0 || n.blend != RenderBlend::Normal;
                    if isolate {
                        if depth >= MAX_ISOLATED_GROUPS {
                            return Err(RenderError::InvalidArgument(format!(
                                "group nesting exceeds {MAX_ISOLATED_GROUPS}"
                            )));
                        }
                        commands.push(Command {
                            inv0: [0.0; 4],
                            inv1: [0.0; 4],
                            info: [1, 0, 0, 0],
                            trans: [0; 4],
                        });
                    }
                    let mut start = 0;
                    for (i, &child) in children.iter().enumerate().rev() {
                        let cn = &b.scene.nodes[child];
                        if !cn.visible || cn.opacity != 1.0 || cn.blend != RenderBlend::Normal {
                            continue;
                        }
                        if let RContent::Tiles(ts) = &cn.content {
                            let child_world = world.concat(cn.transform)?;
                            // Version 3 hides lower layers only behind the tiles the
                            // integer sampler actually reads.
                            let (x0, y0, x1, y1) = if let WorldTransform::V3(fixed) = child_world {
                                let Some(inv) = v3_inverse(fixed)? else {
                                    continue;
                                };
                                match plain_draw_v3(
                                    inv,
                                    b.key,
                                    canvas_end(b.scene),
                                    ts.iter().map(|t| t.key).chain(
                                        b.overrides
                                            .keys()
                                            .filter(|(l, _)| *l == cn.key)
                                            .map(|(_, k)| *k),
                                    ),
                                )?
                                .keys
                                {
                                    Some([x0, y0, x1, y1]) => (x0, y0, x1 + 1, y1 + 1),
                                    None => continue,
                                }
                            } else {
                                let Some(inv) = inverse(child_world.legacy()?) else {
                                    continue;
                                };
                                let (min, max) = bounds_through(inv, b.region);
                                (
                                    (min[0] / 256.0).floor() as i32,
                                    (min[1] / 256.0).floor() as i32,
                                    (max[0] / 256.0).ceil() as i32,
                                    (max[1] / 256.0).ceil() as i32,
                                )
                            };
                            let mut opaque =
                                x1 > x0 && y1 > y0 && (x1 - x0) as i64 * (y1 - y0) as i64 <= 1024;
                            for x in x0..x1 {
                                for y in y0..y1 {
                                    let k = TileKey { tx: x, ty: y };
                                    opaque &= !b.overrides.contains_key(&(cn.key, k))
                                        && ts
                                            .binary_search_by_key(&k, |t| t.key)
                                            .ok()
                                            .is_some_and(|j| store.is_opaque(&ts[j].hash));
                                }
                            }
                            if opaque {
                                start = i;
                                break;
                            }
                        }
                    }
                    for &child in &children[start..] {
                        walk(
                            r,
                            b,
                            child,
                            world,
                            depth + u32::from(isolate),
                            commands,
                            entries,
                            store,
                            encoder,
                            pinned,
                            previews,
                        )?;
                    }
                    if isolate {
                        commands.push(Command {
                            inv0: [0.0; 4],
                            inv1: [0.0, 0.0, n.opacity, mode(n.blend)],
                            info: [2, 0, 0, 0],
                            trans: [0; 4],
                        });
                    }
                }
                RContent::Tiles(tiles) => {
                    let (inv, minx, maxx, miny, maxy) = match world {
                        WorldTransform::Legacy(world) => {
                            let Some(inv) = inverse(world) else {
                                return Ok(());
                            };
                            let (min, max) = bounds_through(inv, b.region);
                            (inv, min[0], max[0], min[1], max[1])
                        }
                        WorldTransform::V3(_) => ([0.0; 6], 0.0, 0.0, 0.0, 0.0),
                    };
                    let mut map: BTreeMap<TileKey, ContentHash> =
                        tiles.iter().map(|t| (t.key, t.hash)).collect();
                    for ((layer, k), hash) in b.overrides {
                        if *layer == n.key {
                            map.insert(*k, *hash);
                        }
                    }
                    let v3 = if let WorldTransform::V3(fixed) = world {
                        let Some(q) = v3_inverse(fixed)? else {
                            return Ok(());
                        };
                        Some(plain_draw_v3(
                            q,
                            b.key,
                            canvas_end(b.scene),
                            map.keys().copied(),
                        )?)
                    } else {
                        None
                    };
                    let start = entries.len() as u32;
                    for (k, hash) in map {
                        let skip = match v3 {
                            Some(d) => !d.reads(k),
                            None => {
                                k.tx as f64 * 256.0 >= maxx
                                    || (k.tx + 1) as f64 * 256.0 <= minx
                                    || k.ty as f64 * 256.0 >= maxy
                                    || (k.ty + 1) as f64 * 256.0 <= miny
                            }
                        };
                        if skip {
                            continue;
                        }
                        let slot = r.upload(hash, store, pinned, encoder, previews)?;
                        entries.push(Entry {
                            xy: [k.tx, k.ty],
                            slot,
                            pad: 0,
                        });
                    }
                    if let Some(d) = v3 {
                        // A draw that reaches none of its tiles is skipped (DQ3-4).
                        if d.keys.is_some() {
                            let count = entries.len() as u32 - start;
                            commands.push(d.command(n.opacity, n.blend, start, count));
                        }
                        return Ok(());
                    }
                    let packed = crate::sample::pack_inverse(inv, b.scene.render_version);
                    commands.push(Command {
                        inv0: packed.inv0,
                        inv1: [
                            packed.translation[0],
                            packed.translation[1],
                            n.opacity,
                            mode(n.blend),
                        ],
                        info: [0, start, entries.len() as u32 - start, packed.sample_mode],
                        trans: packed.trans,
                    });
                }
                _ => {}
            }
            Ok(())
        }
        walk(
            self,
            &Build {
                scene,
                key,
                region,
                overrides: &overrides,
            },
            scene.root,
            WorldTransform::root(scene.render_version),
            0,
            &mut commands,
            &mut entries,
            store,
            encoder,
            &mut pinned,
            &previews,
        )?;
        Ok((commands, entries))
    }
    pub(super) fn cache_slot(&mut self, key: CacheKey, prefetch: bool) -> Result<Option<usize>> {
        let slot = if let Some(i) = self.cache_slots.iter().position(Option::is_none) {
            i
        } else {
            self.cache_pressure = true;
            let Some((&old, _)) = self
                .cache_map
                .iter()
                .filter(|(cached, entry)| {
                    !self.presentation.protects(entry.slot)
                        && !self.visible_keys.contains(cached)
                        && !self.overview_keys.contains(cached)
                        && !self.overview_pins.contains(cached)
                        && !self.pyramid_pins.contains(cached)
                        && !(prefetch
                            && key.0 > 0
                            && cached.0 == key.0 - 1
                            && cached.1.div_euclid(2) == key.1
                            && cached.2.div_euclid(2) == key.2)
                })
                .min_by(|(ak, a), (bk, b)| {
                    let (ab, ai, ad) = self.eviction_rank(**ak);
                    let (bb, bi, bd) = self.eviction_rank(**bk);
                    // Higher bucket / nonideal / distance is evicted first; older breaks ties.
                    bb.cmp(&ab)
                        .then(bi.cmp(&ai))
                        .then(bd.total_cmp(&ad))
                        .then(a.used.cmp(&b.used))
                })
            else {
                return if prefetch {
                    Ok(None)
                } else {
                    Err(RenderError::Budget(
                        "GPU budget: no evictable display tile".into(),
                    ))
                };
            };
            let c = self
                .cache_map
                .remove(&old)
                .ok_or(RenderError::Internal("cache missing".into()))?;
            // A resumable caller may defer submission or cancel. Keep the
            // reverse map consistent even before a replacement is published.
            self.cache_slots[c.slot as usize] = None;
            c.slot as usize
        };
        Ok(Some(slot))
    }
    pub(super) fn compose(
        &mut self,
        scene: &RenderScene,
        key: CacheKey,
        store: &dyn TileStore,
        prefetch: bool,
    ) -> Result<bool> {
        if self.integer_cache && key.0 > 0 {
            return self.compose_from_children(scene, key, store, prefetch);
        }
        let Some(slot) = self.cache_slot(key, prefetch)? else {
            return Ok(false);
        };
        if !crate::damage::plain_scene(scene) {
            self.compose_tiles(scene, key, slot as u32, store)?;
            self.cache_slots[slot] = Some(key);
            self.cache_map.insert(
                key,
                Cached {
                    slot: slot as u32,
                    used: self.frame,
                    prefetched: prefetch,
                },
            );
            return Ok(true);
        }
        // A plain scene needs neither; their GPU objects are destroyed, not dropped.
        if let Some(tiles) = self.tile_renderer.take() {
            tiles.retire(&mut self.retired);
        }
        if let Some(vector) = self.vector.take() {
            vector.retire(&mut self.retired);
        }
        self.release_retired();
        if self.downsample_cached(scene, key, slot as u32)? {
            if self.profiling {
                self.profile.pyramid_hits += 1;
            }
            self.cache_slots[slot] = Some(key);
            self.cache_map.insert(
                key,
                Cached {
                    slot: slot as u32,
                    used: self.frame,
                    prefetched: prefetch,
                },
            );
            return Ok(true);
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("compose tile"),
            });
        let commands_start = (self.clock)();
        let (commands, entries) = self.commands(scene, key, store, &mut encoder)?;
        if self.profiling {
            self.profile.commands_ms += (self.clock)() - commands_start;
        }
        self.submit_composite(scene, key, slot, commands, entries, encoder, prefetch)
    }
    /// Render version 3 defines a level >= 1 tile only as the integer downsample of
    /// its children, so a cold request builds the missing children first and its
    /// bytes equal the warm (cached-children) result. Determinism over speed.
    /// Only explicit readback (`read_tiles`) reaches this synchronously; display
    /// and prefetch build v3 parents step by step through `hierarchy_step`.
    pub(super) fn compose_from_children(
        &mut self,
        scene: &RenderScene,
        key: CacheKey,
        store: &dyn TileStore,
        prefetch: bool,
    ) -> Result<bool> {
        let pins = self.pyramid_pins.len();
        let result = self.compose_children_then_parent(scene, key, store, prefetch);
        self.pyramid_pins.truncate(pins);
        result
    }
    pub(super) fn compose_children_then_parent(
        &mut self,
        scene: &RenderScene,
        key: CacheKey,
        store: &dyn TileStore,
        prefetch: bool,
    ) -> Result<bool> {
        let mut warm = true;
        let uniform = self.uniform_subtree(scene, key);
        let mut children = pyramid_children(scene, key);
        if uniform {
            // Its four children are the same bytes: build the top-left one only.
            children.truncate(1);
        }
        for child in children {
            if !self.cache_map.contains_key(&child) {
                warm = false;
                if !self.compose(scene, child, store, prefetch)? {
                    return Ok(false);
                }
            }
            self.pyramid_pins.push(child);
        }
        if warm && self.profiling {
            self.profile.pyramid_hits += 1;
        }
        let Some(slot) = self.cache_slot(key, prefetch)? else {
            return Ok(false);
        };
        let built = if uniform {
            self.downsample_uniform(key, slot as u32)?
        } else {
            self.downsample_cached(scene, key, slot as u32)?
        };
        if !built {
            return Err(RenderError::Internal("v3 pyramid children missing".into()));
        }
        self.cache_slots[slot] = Some(key);
        self.cache_map.insert(
            key,
            Cached {
                slot: slot as u32,
                used: self.frame,
                prefetched: prefetch,
            },
        );
        Ok(true)
    }
    #[allow(clippy::too_many_arguments)] // Submission shares the visible and prefetch paths without copying GPU setup.
    pub(super) fn submit_composite(
        &mut self,
        scene: &RenderScene,
        key: CacheKey,
        slot: usize,
        mut commands: Vec<Command>,
        entries: Vec<Entry>,
        mut encoder: wgpu::CommandEncoder,
        prefetch: bool,
    ) -> Result<bool> {
        let integer = scene.render_version == 3;
        if integer {
            for c in &mut commands {
                // Every v3 draw is integer (`plain_draw_v3`); there is no f32 fetch.
                if c.info[0] == 0
                    && ![crate::sample::SAMPLE_INTEGER, crate::sample::SAMPLE_NEAREST]
                        .contains(&c.info[3])
                {
                    return Err(RenderError::Internal(
                        "v3 draw without an integer sampler".into(),
                    ));
                }
                c.inv1[2] = f32::from_bits(crate::composite_opacity_q14(c.inv1[2]) as u32);
                c.inv1[3] = f32::from_bits(c.inv1[3] as u32);
            }
        }
        let pipeline = if integer {
            self.composite_q14.get_or_init(|| {
                compute(
                    &self.device,
                    "integer tile compositor",
                    &crate::integer_composite_shader(),
                )
            })
        } else {
            &self.composite
        };
        let integer_output = if integer {
            Some(memory::create_buffer(
                &self.governor,
                &self.device,
                &wgpu::BufferDescriptor {
                    label: Some("integer composite bytes"),
                    size: 256 * 256 * 4,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                },
                Class::Fixed,
                true,
            )?)
        } else {
            None
        };
        let cb = buffer(
            &self.governor,
            &self.device,
            &self.queue,
            "scene commands",
            &commands,
            wgpu::BufferUsages::STORAGE,
        )?;
        self.retired.classified_buffer(&cb, MemoryClass::Fixed);
        let tb = buffer(
            &self.governor,
            &self.device,
            &self.queue,
            "sparse tile index",
            &entries,
            wgpu::BufferUsages::STORAGE,
        )?;
        self.retired.classified_buffer(&tb, MemoryClass::Fixed);
        let scale = 2f32.powi(key.0);
        let ub = if integer {
            // Level 0 only (`plain_draw_v3`): the origin is the key times 256.
            let origin = [key.1, key.2].map(|k| i64::from(k) * 256);
            let [Ok(ox), Ok(oy)] = origin.map(i32::try_from) else {
                return Err(RenderError::GpuUnsupported(
                    "v3 tile has no integer origin".into(),
                ));
            };
            buffer(
                &self.governor,
                &self.device,
                &self.queue,
                "integer tile config",
                &[IntegerConfig {
                    origin_scale: [ox, oy, 1, 0],
                    info: [commands.len() as u32, slot as u32, 0, 0],
                    span: resolve_span(
                        origin,
                        1,
                        [i64::from(scene.canvas.x1), i64::from(scene.canvas.y1)],
                    ),
                }],
                wgpu::BufferUsages::UNIFORM,
            )?
        } else {
            buffer(
                &self.governor,
                &self.device,
                &self.queue,
                "tile config",
                &[Config {
                    origin_scale: [
                        key.1 as f32 * 256.0 * scale,
                        key.2 as f32 * 256.0 * scale,
                        scale,
                        scene.canvas.x1 as f32,
                    ],
                    info: [
                        commands.len() as u32,
                        slot as u32,
                        scene.canvas.y1 as u32,
                        0,
                    ],
                }],
                wgpu::BufferUsages::UNIFORM,
            )?
        };
        self.retired.classified_buffer(&ub, MemoryClass::Fixed);
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compose"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: match &integer_output {
                        Some(out) => out.as_entire_binding(),
                        None => wgpu::BindingResource::TextureView(&self.cache_view),
                    },
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: cb.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: tb.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: ub.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("composite"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(16, 16, 1);
        }
        if let Some(out) = integer_output {
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &out,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(1024),
                        rows_per_image: Some(256),
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self.cache,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: slot as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: 256,
                    height: 256,
                    depth_or_array_layers: 1,
                },
            );
            self.retired.classified_buffer(&out, MemoryClass::Fixed);
        }
        self.queue.submit(Some(encoder.finish()));
        self.release_retired();
        self.cache_slots[slot] = Some(key);
        self.cache_map.insert(
            key,
            Cached {
                slot: slot as u32,
                used: self.frame,
                prefetched: prefetch,
            },
        );
        Ok(true)
    }
}
