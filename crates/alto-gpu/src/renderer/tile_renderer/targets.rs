//! Pooled temporary targets, uploads and compute dispatch for one tile.
use super::*;

impl Context<'_> {
    pub(super) fn dummy(&self) -> Target {
        Target {
            index: 0,
            _lease: Arc::new(()),
            roi: Roi {
                x: 0.0,
                y: 0.0,
                scale: 1.0,
                w: 1,
                h: 1,
            },
        }
    }
    pub(super) fn zero(&mut self, roi: Roi) -> Result<Target> {
        let out = self.alloc(roi)?;
        let d = self.dummy();
        // Version 3 has no mode 6 and writes zero for it; it reads nothing.
        let p = if self.resources.exact {
            Self::q14_params(6, roi, [None, None], [0; 4], None)?
        } else {
            Self::params(6, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
        };
        self.dispatch(&out, &d, &d, &d, p, &[])?;
        Ok(out)
    }
    pub(super) fn crop(&mut self, input: Target, roi: Roi) -> Result<Target> {
        if input.roi.x == roi.x
            && input.roi.y == roi.y
            && input.roi.w == roi.w
            && input.roi.h == roi.h
            && input.roi.scale == roi.scale
        {
            return Ok(input);
        }
        let out = self.alloc(roi)?;
        let p =
            if self.resources.exact {
                let destination = RoiQ::from_roi(roi)?;
                let source = RoiQ::from_roi(input.roi)?;
                let m = source.level - destination.level;
                if !(0..=7).contains(&m) {
                    return Err(RenderError::InvalidArgument(
                        "v3 resample level ratio".into(),
                    ));
                }
                let factor = 1i64 << m;
                let dx = destination
                    .ox
                    .checked_sub(source.ox.checked_mul(factor).ok_or_else(|| {
                        RenderError::InvalidArgument("v3 resample x product".into())
                    })?)
                    .and_then(|v| i32::try_from(v).ok())
                    .ok_or_else(|| RenderError::InvalidArgument("v3 resample x offset".into()))?;
                let dy = destination
                    .oy
                    .checked_sub(source.oy.checked_mul(factor).ok_or_else(|| {
                        RenderError::InvalidArgument("v3 resample y product".into())
                    })?)
                    .and_then(|v| i32::try_from(v).ok())
                    .ok_or_else(|| RenderError::InvalidArgument("v3 resample y offset".into()))?;
                Params {
                    curve: super::mask_grid([dx, dy, m, 0], [0; 4], [0; 4], None),
                    info: [25, 0, 0, 0],
                    ..Params::zeroed()
                }
            } else {
                Self::params(8, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
            };
        self.dispatch(&out, &input, &input, &input, p, &[])?;
        Ok(out)
    }
    pub(super) fn alloc(&mut self, roi: Roi) -> Result<Target> {
        if roi.w == 0
            || roi.h == 0
            || roi.w > self.r.device.limits().max_texture_dimension_2d
            || roi.h > self.r.device.limits().max_texture_dimension_2d
        {
            return Err(RenderError::InvalidArgument(
                "effect ROI exceeds texture limits".into(),
            ));
        }
        let w = bucket(roi.w);
        let h = bucket(roi.h);
        if w > self.r.device.limits().max_texture_dimension_2d
            || h > self.r.device.limits().max_texture_dimension_2d
        {
            return Err(RenderError::InvalidArgument(
                "effect bucket exceeds texture limits".into(),
            ));
        }
        let lease = Arc::new(());
        // Weak leases track every retained ancestor and clipping backdrop. A slot is
        // reused only after its last CPU owner has encoded its last read.
        let reuse = self
            .resources
            .pool
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, p)| p.w == w && p.h == h && p.lease.strong_count() == 0)
            .map(|(i, _)| i);
        let index = if let Some(index) = reuse {
            self.resources.reuses += 1;
            index
        } else {
            let need = w as u64 * h as u64 * 8;
            let used = self.r.gpu_bytes().budget_used;
            if used + need > self.r.budget_bytes {
                // No premature destroy: unsubmitted targets are retained until the
                // encoder is submitted or discarded, and still count in the ledger.
                return Err(RenderError::Budget(
                    "effects exceed GPU memory budget (including retired allocations)".into(),
                ));
            }
            self.resources.allocations += 1;
            self.resources.pool.push(Pooled {
                texture: memory::create_texture(
                    &self.r.governor,
                    &self.r.device,
                    &wgpu::TextureDescriptor {
                        label: Some("M5 bucket target"),
                        size: wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: target_format(self.resources.exact),
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::STORAGE_BINDING
                            | wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    },
                    Class::Effects,
                )?,
                w,
                h,
                lease: Weak::new(),
                used: self.r.frame,
            });
            self.resources.pool.len() - 1
        };
        let pooled = &mut self.resources.pool[index];
        pooled.lease = Arc::downgrade(&lease);
        pooled.used = self.r.frame;
        let view = pooled.texture.create_view(&Default::default());
        // This GPU clear follows all old reads in this encoder. Queue writes here
        // would run ahead of those reads and corrupt the old owner.
        drop(self.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear whole M5 bucket"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        }));
        Ok(Target {
            index,
            roi,
            _lease: lease,
        })
    }

    pub(super) fn dispatch(
        &mut self,
        output: &Target,
        a: &Target,
        b: &Target,
        clip: &Target,
        mut params: Params,
        entries: &[Entry],
    ) -> Result<()> {
        let additional = (entries.len().max(1) * std::mem::size_of::<Entry>()
            + std::mem::size_of::<Params>()) as u64;
        if self.r.gpu_bytes().budget_used + additional > self.r.budget_bytes {
            return Err(RenderError::Budget(
                "effect command buffers exceed GPU memory budget".into(),
            ));
        }
        // Version 3 reads no float slot (`effects-q14.wgsl` `Config.unused`): its
        // regions are in `grid` (`q14_params`), and these stay zero.
        if !self.resources.exact {
            params.out_region = output.roi.uniform();
            params.region_a = a.roi.uniform();
            params.region_b = b.roi.uniform();
        }
        params.info[2] = entries.len() as u32;
        params.sizes = [a.roi.w, a.roi.h, b.roi.w, b.roi.h];
        params.output_clip = [output.roi.w, output.roi.h, clip.roi.w, clip.roi.h];
        debug_assert!(
            output.index != a.index && output.index != b.index && output.index != clip.index
        );
        let av = self.resources.pool[a.index]
            .texture
            .create_view(&Default::default());
        let bv = self.resources.pool[b.index]
            .texture
            .create_view(&Default::default());
        let cv = self.resources.pool[clip.index]
            .texture
            .create_view(&Default::default());
        let ov = self.resources.pool[output.index]
            .texture
            .create_view(&Default::default());
        let tiles = buffer(
            &self.r.governor,
            &self.r.device,
            &self.r.queue,
            "M5 sparse tiles",
            entries,
            wgpu::BufferUsages::STORAGE,
        )?;
        let uniform = buffer(
            &self.r.governor,
            &self.r.device,
            &self.r.queue,
            "M5 ROI",
            &[params],
            wgpu::BufferUsages::UNIFORM,
        )?;
        self.r
            .retired
            .classified_buffer(&tiles, MemoryClass::Effects);
        self.r
            .retired
            .classified_buffer(&uniform, MemoryClass::Effects);
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&av),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&bv),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&self.r.atlas_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&ov),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: tiles.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: uniform.as_entire_binding(),
            },
        ];
        entries.push(wgpu::BindGroupEntry {
            binding: 2,
            resource: wgpu::BindingResource::TextureView(&cv),
        });
        let bg = self.r.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("M5 stage"),
            layout: &self.resources.pipeline.get_bind_group_layout(0),
            entries: &entries,
        });
        let mut pass = self.encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&self.resources.pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(output.roi.w.div_ceil(16), output.roi.h.div_ceil(16), 1);
        Ok(())
    }
    pub(super) fn entries(
        &mut self,
        map: &[crate::RenderTile],
        world: [f64; 6],
        roi: Roi,
        layer: Option<NodeId>,
    ) -> Result<Vec<Entry>> {
        let Some(inv) = inverse(world) else {
            return Ok(vec![]);
        };
        let b = roi.bounds();
        let corners =
            [[b[0], b[1]], [b[2], b[1]], [b[0], b[3]], [b[2], b[3]]].map(|p| point(inv, p));
        let minx = corners.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
        let maxx = corners
            .iter()
            .map(|p| p[0])
            .fold(f64::NEG_INFINITY, f64::max);
        let miny = corners.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
        let maxy = corners
            .iter()
            .map(|p| p[1])
            .fold(f64::NEG_INFINITY, f64::max);
        self.upload_entries(map, layer, |key| {
            !(key.tx as f64 * 256.0 >= maxx
                || (key.tx + 1) as f64 * 256.0 <= minx
                || key.ty as f64 * 256.0 >= maxy
                || (key.ty + 1) as f64 * 256.0 <= miny)
        })
    }
    /// Uploads the tiles of `map` (with the stroke overrides of `layer`) that
    /// `keep` selects, in key order.
    pub(super) fn upload_entries(
        &mut self,
        map: &[crate::RenderTile],
        layer: Option<NodeId>,
        keep: impl Fn(TileKey) -> bool,
    ) -> Result<Vec<Entry>> {
        let mut map: BTreeMap<_, _> = map.iter().map(|t| (t.key, t.hash)).collect();
        if let Some(layer) = layer {
            for ((l, key), hash) in &self.overrides {
                if *l == layer {
                    map.insert(*key, *hash);
                }
            }
        }
        let mut entries = vec![];
        for (key, hash) in map {
            if !keep(key) {
                continue;
            }
            let slot = self.r.upload(
                hash,
                self.store,
                &mut self.pinned,
                &mut self.encoder,
                &self.previews,
            )?;
            entries.push(Entry {
                xy: [key.tx, key.ty],
                slot,
                pad: 0,
            });
        }
        Ok(entries)
    }
}
