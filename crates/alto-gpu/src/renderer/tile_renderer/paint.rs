//! Vector and text content painted into tile targets, float and Q14.
use super::*;

const VECTOR_BUDGET: &str = "v3 vector coverage exceeds GPU budget";

impl Context<'_> {
    pub(super) fn vector_q14(
        &mut self,
        paths: &Arc<Vec<StyledPath>>,
        world: alto_core::FixedAffine,
        roi: Roi,
        keep: bool,
    ) -> Result<Target> {
        let [ox, oy, level] = crate::raster_q8::roi_grid(roi.x, roi.y, roi.scale)
            .ok_or_else(|| RenderError::GpuUnsupported("v3 vector ROI is not integer".into()))?;
        let level = i32::try_from(level)
            .ok()
            .filter(|l| (-6..=12).contains(l))
            .ok_or_else(|| RenderError::InvalidArgument("v3 vector level out of range".into()))?;
        // The mapped paths and stroke outlines do not depend on the tile, so
        // with `keep` they are kept for the node's other tiles (a vector mask's
        // one-path list is made per tile and is not). The entry is taken out
        // while this tile paints and put back after, whatever the result.
        let found = self
            .resources
            .vectors
            .iter()
            .position(|v| Arc::ptr_eq(&v.paths, paths) && v.world == world && v.level == level);
        let mut prepared = match found {
            Some(index) => self.resources.vectors.swap_remove(index),
            None => PreparedVector {
                paths: paths.clone(),
                world,
                level,
                used: 0,
                // VECTOR_TRANSFORM_RANGE: a world that puts a point of the node
                // outside the Q8 grid's integer range makes the whole node
                // transparent (not the tile refused), like TEXT_TRANSFORM_RANGE.
                // It depends on the node and level only, so every tile agrees.
                mapped: paths
                    .iter()
                    .map(|path| crate::raster_q8::map_path(&path.data, world, level))
                    .collect(),
                outlines: vec![None; paths.len()],
                bytes: 0,
            },
        };
        let others: usize = self.resources.vectors.iter().map(|v| v.bytes).sum();
        prepared.used = self.r.frame;
        let room = PREPARED_BYTES.saturating_sub(others);
        let result = self.vector_q14_prepared(&mut prepared, [ox, oy], level, roi, room);
        if !keep {
            return result;
        }
        if self.resources.vectors.len() >= PREPARED_VECTORS {
            let oldest = (0..self.resources.vectors.len())
                .min_by_key(|&i| self.resources.vectors[i].used)
                .expect("a full cache");
            self.resources.vectors.swap_remove(oldest);
        }
        self.resources.vectors.push(prepared);
        result
    }

    fn vector_q14_prepared(
        &mut self,
        prepared: &mut PreparedVector,
        [ox, oy]: [i64; 2],
        level: i32,
        roi: Roi,
        room: usize,
    ) -> Result<Target> {
        let Some(mapped) = &prepared.mapped else {
            return self.zero(roi);
        };
        let (paths, world) = (prepared.paths.clone(), prepared.world);
        // Each paint's coverage is cropped to its nonzero cells and painted in
        // order by `glyph-q14.wgsl`, many paints per dispatch. The shader's
        // formula is vector-q14.wgsl's, and a zero byte leaves a pixel as it
        // was in both, so the Q14 result is the same as one dispatch per paint.
        let window = [ox, oy, i64::from(roi.w), i64::from(roi.h)];
        let flush = 4 * bucket(roi.w) as usize * bucket(roi.h) as usize;
        let mut current = self.zero(roi)?;
        let mut pending: Vec<crate::glyph_q8::GlyphCrop> = vec![];
        let mut pending_bytes = 0;
        for (index, (path, commands)) in paths.iter().zip(mapped).enumerate() {
            // Fill precedes stroke for every styled path, including when one
            // paint has no visible alpha or exceeds the curve range.
            for paint in 0..2 {
                let paint_color = if paint == 0 {
                    path.fill
                } else {
                    path.stroke
                        .as_ref()
                        .filter(|stroke| crate::raster_q8::stroke_visible(stroke))
                        .map(|stroke| stroke.color)
                };
                let Some(paint_color) = paint_color else {
                    continue;
                };
                let color = crate::raster_q8::color_q14(paint_color).ok_or_else(|| {
                    RenderError::GpuUnsupported("v3 vector color out of range".into())
                })?;
                if color[3] == 0 {
                    continue;
                }
                let cropped = if paint == 0 {
                    let rule = match path.fill_rule {
                        FillRule::NonZero => crate::raster_q8::FillRule::NonZero,
                        FillRule::EvenOdd => crate::raster_q8::FillRule::EvenOdd,
                    };
                    crate::raster_q8::rasterize_crop(
                        commands,
                        rule,
                        window,
                        crate::raster_q8::CURVE_LIMIT,
                    )
                } else {
                    let built;
                    let outline = match &prepared.outlines[index] {
                        Some(outline) => outline,
                        None => {
                            let outline = crate::stroke_q8::outline(
                                &path.data,
                                path.stroke.as_ref().expect("visible stroke"),
                                world,
                                level,
                            )
                            .map(|o| o.map(|o| crate::raster_q8::Packed::new(&o)));
                            let size = outline
                                .as_ref()
                                .map_or(0, |o| o.as_ref().map_or(0, |o| o.bytes()));
                            if prepared.bytes + size <= room {
                                prepared.bytes += size;
                                prepared.outlines[index].insert(outline)
                            } else {
                                built = outline;
                                &built
                            }
                        }
                    };
                    match outline {
                        Ok(Some(outline)) => crate::raster_q8::rasterize_crop_packed(
                            outline,
                            crate::raster_q8::FillRule::NonZero,
                            window,
                            crate::raster_q8::CURVE_LIMIT,
                        ),
                        Ok(None) => Ok(None),
                        Err(error) => Err(*error),
                    }
                };
                // A path whose integer subdivision budget is exceeded is
                // skipped identically on every tile.
                let cropped = match cropped {
                    Ok(cropped) => cropped,
                    Err(crate::raster_q8::RasterError::CurveRange) => continue,
                    Err(crate::raster_q8::RasterError::Range) => {
                        return Err(RenderError::GpuUnsupported(
                            "v3 vector window out of range".into(),
                        ));
                    }
                };
                let Some(([x, y], w, h, bytes)) = cropped else {
                    continue;
                };
                pending_bytes += bytes.len();
                pending.push(crate::glyph_q8::GlyphCrop {
                    dst: [x, y],
                    w,
                    h,
                    bytes,
                    color,
                });
                if pending.len() >= GLYPH_Q14_CHUNK || pending_bytes >= flush {
                    current = self.paint_crops(current, &pending, roi, VECTOR_BUDGET)?;
                    pending.clear();
                    pending_bytes = 0;
                }
            }
        }
        if pending.is_empty() {
            return Ok(current);
        }
        self.paint_crops(current, &pending, roi, VECTOR_BUDGET)
    }

    /// Render version 3 text from its bake only: every glyph's coverage
    /// rasterized on the CPU and cropped to the ROI (`glyph_q8::glyph_bitmaps`),
    /// shelf-packed into fresh `R8Unorm` scratch textures, and painted in order
    /// by `glyph-q14.wgsl` in chunks that each read the previous chunk's Q14.
    /// There is no persistent atlas, so cache state never changes a pixel. An
    /// unbaked node is transparent (`TEXT_UNBAKED`).
    pub(super) fn text_q14(
        &mut self,
        bake: Option<&alto_scene::RTextBake>,
        world: alto_core::FixedAffine,
        roi: Roi,
    ) -> Result<Target> {
        let grid = RoiQ::from_roi(roi)?;
        let current = self.zero(roi)?;
        let Some(bake) = bake else {
            return Ok(current);
        };
        let crops = crate::glyph_q8::glyph_bitmaps(
            &bake.bytes,
            world,
            grid.level,
            [grid.ox, grid.oy, i64::from(roi.w), i64::from(roi.h)],
        );
        self.paint_crops(current, &crops, roi, "v3 glyph scratch exceeds GPU budget")
    }

    /// Paint coverage crops onto `current` in order with `glyph-q14.wgsl`:
    /// shelf-packed into fresh `R8Unorm` scratch textures, in chunks that each
    /// read the previous chunk's Q14. `budget` is the error when a chunk would
    /// exceed the GPU budget.
    fn paint_crops(
        &mut self,
        mut current: Target,
        crops: &[crate::glyph_q8::GlyphCrop],
        roi: Roi,
        budget: &str,
    ) -> Result<Target> {
        let (sw, sh) = (bucket(roi.w), bucket(roi.h));
        let mut next = 0;
        while next < crops.len() {
            // Pack as many crops as fit (each is at most the ROI) into one scratch.
            let mut packer = crate::glyph_atlas::GlyphAtlas::new(sw, sh);
            packer.begin(1);
            let mut batch = vec![];
            while next < crops.len() && batch.len() < GLYPH_Q14_CHUNK {
                let crop = &crops[next];
                let key = crate::glyph_atlas::GlyphKey {
                    raster: 1,
                    outline_hash: [0; 32],
                    outline: next as u64,
                    matrix: [0; 4],
                    phase: [0; 2],
                };
                let Some(rect) = packer.insert(key, crop.w, crop.h) else {
                    break;
                };
                batch.push((crop, rect));
                next += 1;
            }
            if batch.is_empty() {
                return Err(RenderError::Internal(
                    "v3 glyph crop larger than its scratch".into(),
                ));
            }
            let need = u64::from(sw) * u64::from(sh) + 48 * batch.len() as u64 + 16;
            if self.r.gpu_bytes().budget_used.saturating_add(need) > self.r.budget_bytes {
                return Err(RenderError::Budget(budget.into()));
            }
            let instances: Vec<GlyphQ14> = batch
                .iter()
                .map(|(crop, rect)| GlyphQ14 {
                    dst: [
                        crop.dst[0] as i32,
                        crop.dst[1] as i32,
                        crop.w as i32,
                        crop.h as i32,
                    ],
                    src: [rect.x, rect.y, 0, 0],
                    color: crop.color.map(u32::from),
                })
                .collect();
            // Every fallible step comes before the scratch texture, and the
            // buffers made before a failure are destroyed: an error never drops
            // a registered resource without releasing its budget.
            let output = self.alloc(roi)?;
            if self.resources.glyph_q14.is_none() {
                return Err(RenderError::Internal(
                    "v3 glyph pipeline was not created".into(),
                ));
            }
            let glyphs = buffer(
                &self.r.governor,
                &self.r.device,
                &self.r.queue,
                "glyph q14 instances",
                &instances,
                wgpu::BufferUsages::STORAGE,
            )?;
            let params = match buffer(
                &self.r.governor,
                &self.r.device,
                &self.r.queue,
                "glyph q14 params",
                &[GlyphQ14Params {
                    count: instances.len() as u32,
                    w: roi.w,
                    h: roi.h,
                    pad: 0,
                }],
                wgpu::BufferUsages::UNIFORM,
            ) {
                Ok(params) => params,
                Err(error) => {
                    memory::destroy_buffer(&self.r.governor, &glyphs, true);
                    return Err(error);
                }
            };
            let created = memory::create_texture(
                &self.r.governor,
                &self.r.device,
                &wgpu::TextureDescriptor {
                    label: Some("v3 glyph scratch"),
                    size: wgpu::Extent3d {
                        width: sw,
                        height: sh,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                Class::Vector,
            );
            let scratch = match created {
                Ok(scratch) => scratch,
                Err(error) => {
                    memory::destroy_buffer(&self.r.governor, &glyphs, true);
                    memory::destroy_buffer(&self.r.governor, &params, true);
                    return Err(error);
                }
            };
            for (crop, rect) in &batch {
                self.r.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &scratch,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: rect.x,
                            y: rect.y,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &crop.bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(crop.w),
                        rows_per_image: Some(crop.h),
                    },
                    wgpu::Extent3d {
                        width: crop.w,
                        height: crop.h,
                        depth_or_array_layers: 1,
                    },
                );
            }
            let pipeline = self.resources.glyph_q14.as_ref().expect("checked above");
            let scratch_view = scratch.create_view(&Default::default());
            let previous = self.resources.pool[current.index]
                .texture
                .create_view(&Default::default());
            let target = self.resources.pool[output.index]
                .texture
                .create_view(&Default::default());
            let group = self.r.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("v3 glyph paint"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: glyphs.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&scratch_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&previous),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&target),
                    },
                ],
            });
            {
                let mut pass = self.encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &group, &[]);
                pass.dispatch_workgroups(roi.w.div_ceil(8), roi.h.div_ceil(8), 1);
            }
            self.r.retired.effect_texture(scratch);
            self.r
                .retired
                .classified_buffer(&glyphs, MemoryClass::Vector);
            self.r
                .retired
                .classified_buffer(&params, MemoryClass::Vector);
            current = output;
        }
        Ok(current)
    }

    pub(super) fn vector(
        &mut self,
        content: VectorContent,
        world: [f64; 6],
        roi: Roi,
        batch: bool,
    ) -> Result<Target> {
        if self.resources.exact {
            return Err(RenderError::Internal(
                "v3 vector content reached the M5 renderer".into(),
            ));
        }
        let out = self.alloc(roi)?;
        let target = self.resources.pool[out.index].texture.clone();
        let mut resources = self.r.vector.take().unwrap_or_else(|| {
            self.r.pipelines_built = true;
            super::vector::VectorResources::new_with_governor(
                &self.r.device,
                wgpu::TextureFormat::Rgba16Float,
                self.r.governor.clone(),
            )
        });
        let usage = self.r.gpu_bytes();
        let budget = self
            .r
            .budget_bytes
            .saturating_sub(usage.budget_used - usage.vector);
        let roi = [
            roi.x as f64,
            roi.y as f64,
            roi.scale as f64,
            roi.w as f64,
            roi.h as f64,
        ];
        let result = match content {
            VectorContent::Paths(paths) if batch => resources.render(
                &self.r.device,
                &self.r.queue,
                &mut self.encoder,
                &target,
                &paths.iter().collect::<Vec<_>>(),
                world,
                roi,
                self.r.frame,
                budget,
            ),
            VectorContent::Paths(paths) => resources.render_legacy(
                &self.r.device,
                &self.r.queue,
                &mut self.encoder,
                &target,
                &paths.iter().collect::<Vec<_>>(),
                world,
                roi,
                self.r.frame,
                budget,
            ),
            VectorContent::Text(runs) => resources.render_text(
                &self.r.device,
                &self.r.queue,
                &mut self.encoder,
                &target,
                runs,
                world,
                roi,
                self.r.frame,
                budget,
                self.r.glyph_atlas,
            ),
        };
        self.r.retired.take(&mut resources.retired);
        self.r.pipelines_built |= resources.take_pipelines_built();
        self.r.vector = Some(resources);
        result?;
        Ok(out)
    }
    pub(super) fn path_coverage(
        &mut self,
        path: &alto_vector::PathData,
        rule: FillRule,
        world: [f64; 6],
        roi: Roi,
        feather: f32,
    ) -> Result<Target> {
        let sample = roi.coarsen(feather / 64.0);
        let effect = REffect {
            kind: REffectKind::GaussianBlur {
                radius: feather,
                radius_q8: alto_core::blur::blur_radius_q8_f32(feather)
                    .expect("validated vector feather"),
            },
            selection: None,
        };
        let input = Self::input_roi(std::slice::from_ref(&effect), sample);
        let coverage = self.vector(
            VectorContent::Paths(&[StyledPath {
                data: path.clone(),
                fill: Some([1.0; 4]),
                stroke: None,
                fill_rule: rule,
            }]),
            world,
            input,
            false,
        )?;
        let output = self.effects(coverage, &[effect], sample, WorldTransform::Legacy(world))?;
        self.crop(output, roi)
    }
    pub(super) fn path_coverage_q14(
        &mut self,
        path: &alto_vector::PathData,
        rule: FillRule,
        world: alto_core::FixedAffine,
        roi: Roi,
        feather: f32,
    ) -> Result<Target> {
        let radius_q8 = alto_core::blur::blur_radius_q8_f32(feather)
            .ok_or_else(|| RenderError::InvalidArgument("v3 vector feather radius".into()))?;
        let sample = if radius_q8 == 0 {
            roi
        } else {
            let grid = RoiQ::from_roi(roi)?;
            let k = alto_core::blur::blur_coarsen_k(feather)
                .ok_or_else(|| RenderError::InvalidArgument("v3 vector feather coarsen".into()))?;
            grid.coarsen(k.max(grid.level))?.to_roi()?
        };
        let effect = REffect {
            kind: REffectKind::GaussianBlur {
                radius: feather,
                radius_q8,
            },
            selection: None,
        };
        let input = if radius_q8 == 0 {
            sample
        } else {
            Self::input_roi_v3(std::slice::from_ref(&effect), sample)?
        };
        let coverage = self.vector_q14(
            &Arc::new(vec![StyledPath {
                data: path.clone(),
                fill: Some([1.0; 4]),
                stroke: None,
                fill_rule: rule,
            }]),
            world,
            input,
            false,
        )?;
        if radius_q8 == 0 {
            return Ok(coverage);
        }
        let blurred = self.stage(coverage, &effect, sample, WorldTransform::V3(world))?;
        self.crop(blurred, roi)
    }
    pub(super) fn vector_coverage(
        &mut self,
        node: &RNode,
        world: WorldTransform,
        roi: Roi,
    ) -> Result<Option<Target>> {
        if self.resources.exact {
            let mut result = None;
            if let Some(mask) = &node.features.vector_mask {
                let local =
                    alto_core::FixedAffine::from_affine(mask.transform).ok_or_else(|| {
                        RenderError::GpuUnsupported("v3 vector mask transform out of range".into())
                    })?;
                let transform = world.fixed()?.concat(local).ok_or_else(|| {
                    RenderError::GpuUnsupported("v3 vector mask world out of range".into())
                })?;
                result = Some(self.path_coverage_q14(
                    &mask.path,
                    mask.fill_rule,
                    transform,
                    roi,
                    mask.feather,
                )?);
            }
            if let Some(path) = &node.features.clip_path {
                let local =
                    alto_core::FixedAffine::from_affine(path.transform).ok_or_else(|| {
                        RenderError::GpuUnsupported("v3 clip path transform out of range".into())
                    })?;
                let transform = world.fixed()?.concat(local).ok_or_else(|| {
                    RenderError::GpuUnsupported("v3 clip path world out of range".into())
                })?;
                let coverage =
                    self.path_coverage_q14(&path.path, path.fill_rule, transform, roi, 0.0)?;
                result = Some(if let Some(mask) = result {
                    let out = self.alloc(roi)?;
                    let params = Self::q14_params(
                        26,
                        roi,
                        [Some(mask.roi), Some(coverage.roi)],
                        [0; 4],
                        None,
                    )?;
                    self.dispatch(&out, &mask, &coverage, &mask, params, &[])?;
                    out
                } else {
                    coverage
                });
            }
            return Ok(result);
        }
        let world = world.legacy()?;
        let mut result = None;
        if let Some(mask) = &node.features.vector_mask {
            result = Some(self.path_coverage(
                &mask.path,
                mask.fill_rule,
                multiply(world, mask.transform.0),
                roi,
                mask.feather,
            )?);
        }
        if let Some(path) = &node.features.clip_path {
            let coverage = self.path_coverage(
                &path.path,
                path.fill_rule,
                multiply(world, path.transform.0),
                roi,
                0.0,
            )?;
            if let Some(mask) = result {
                let out = self.alloc(roi)?;
                self.dispatch(&out, &mask, &coverage, &mask, Self::params(11, world), &[])?;
                result = Some(out);
            } else {
                result = Some(coverage);
            }
        }
        Ok(result)
    }
}
