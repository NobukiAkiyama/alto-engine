//! Layer properties, blending, clipping and effect stages.
use super::*;

fn unsharp_is_identity(kind: &REffectKind) -> bool {
    matches!(
        *kind,
        REffectKind::UnsharpMask { radius_q8, amount_q8, threshold_q14, .. }
            if radius_q8 == 0 || amount_q8 == 0 || threshold_q14 >= 16384
    )
}

fn smoothing_radius(kind: &REffectKind) -> Option<(f32, u32)> {
    match *kind {
        REffectKind::GaussianBlur { radius, radius_q8 }
        | REffectKind::HighPass { radius, radius_q8 } => Some((radius, radius_q8)),
        REffectKind::UnsharpMask {
            radius, radius_q8, ..
        } if !unsharp_is_identity(kind) => Some((radius, radius_q8)),
        REffectKind::MotionBlur { distance_q8, .. } if distance_q8 != 0 => {
            let half_q8 = distance_q8.div_ceil(2);
            Some((half_q8 as f32 / 256.0, half_q8))
        }
        _ => None,
    }
}

fn needs_original(effect: &REffect) -> bool {
    effect.selection.is_some()
        || matches!(
            effect.kind,
            REffectKind::HighPass { .. } | REffectKind::UnsharpMask { .. }
        )
}

fn motion_blur_settings(kind: &REffectKind, sample: RoiQ) -> Result<[i32; 4]> {
    let REffectKind::MotionBlur {
        angle_q16,
        distance_q8,
    } = *kind
    else {
        unreachable!("motion blur settings requested for another effect")
    };
    let half_q8 = distance_q8.div_ceil(2);
    let (radius, rho8) = sample.taps(half_q8)?;
    if radius == 0 {
        return Ok([0, 0, angle_q16, 0]);
    }
    let spacing_q8 = (rho8 + radius / 2) / radius;
    Ok([radius as i32, spacing_q8 as i32, angle_q16, 0])
}

pub(super) fn needs_backdrop_halo(effect: &REffect, exact: bool) -> bool {
    smoothing_radius(&effect.kind).is_some_and(
        |(radius, radius_q8)| {
            if exact { radius_q8 != 0 } else { radius > 0.0 }
        },
    )
}

fn smoothing_grid(effect: &REffect, desired: RoiQ) -> Result<Option<(RoiQ, RoiQ)>> {
    let Some((radius, radius_q8)) = smoothing_radius(&effect.kind) else {
        return Ok(None);
    };
    let level =
        alto_core::blur::blur_coarsen_k(radius).map_or(desired.level, |k| k.max(desired.level));
    let sample = desired.coarsen(level)?;
    Ok(Some((sample, sample.grow(radius_q8)?)))
}

#[derive(Clone, Copy)]
struct PlaneRequest {
    grid: RoiQ,
    uses: usize,
}

fn request_plane(planes: &mut BTreeMap<i32, PlaneRequest>, grid: RoiQ) -> Result<()> {
    use std::collections::btree_map::Entry;
    match planes.entry(grid.level) {
        Entry::Occupied(mut entry) => {
            let plane = entry.get_mut();
            plane.grid = plane.grid.union(grid)?;
            plane.uses += 1;
        }
        Entry::Vacant(entry) => {
            entry.insert(PlaneRequest { grid, uses: 1 });
        }
    }
    Ok(())
}

// At each prefix, all same-level requests use one document-aligned ROI. Coarse
// Gaussian sampling adds its interpolation halo before joining requests.
// Separate levels stay separate because resampling the original changes pixels.
fn stack_plan(effects: &[REffect], desired: RoiQ) -> Result<Vec<BTreeMap<i32, PlaneRequest>>> {
    let mut plan = vec![BTreeMap::new(); effects.len() + 1];
    request_plane(plan.last_mut().expect("root prefix"), desired)?;
    for (prefix, effect) in effects.iter().enumerate().rev() {
        let (before, after) = plan.split_at_mut(prefix + 1);
        let requests = &mut before[prefix];
        for plane in after[0].values() {
            let desired = plane.grid;
            let input = smoothing_grid(effect, desired)?.map_or(desired, |(_, input)| input);
            request_plane(requests, input)?;
            if input.level != desired.level && needs_original(effect) {
                request_plane(requests, desired)?;
            }
        }
    }
    Ok(plan)
}

type Planes = BTreeMap<i32, (Target, usize)>;

fn use_plane(planes: &mut Planes, level: i32) -> Target {
    let (target, uses) = planes.get_mut(&level).expect("planned input level");
    *uses -= 1;
    if *uses == 0 {
        planes.remove(&level).expect("planned input level").0
    } else {
        target.clone()
    }
}

impl Context<'_> {
    pub(super) fn apply_props(
        &mut self,
        source: Target,
        node: &RNode,
        world: WorldTransform,
    ) -> Result<Target> {
        // Pixel-mask/opacity is applied first, then one vector coverage product.
        if self.resources.exact {
            if node.features.mask.is_none()
                && node.features.vector_mask.is_none()
                && node.features.clip_path.is_none()
            {
                return Ok(source);
            }
            let out = self.alloc(source.roi)?;
            let (entries, transform) = if let Some(mask) = &node.features.mask {
                let (entries, transform) = self.mask_entries(
                    mask,
                    world,
                    source.roi,
                    if self.mask_preview {
                        Some(node.key)
                    } else {
                        None
                    },
                )?;
                (entries, Some(transform))
            } else {
                (vec![], None)
            };
            let p = Self::mask_params(
                21,
                transform,
                source.roi,
                source.roi,
                source.roi,
                [
                    crate::composite_opacity_q14(node.opacity),
                    i32::from(node.features.mask.is_some()),
                    0,
                    0,
                ],
            )?;
            self.dispatch(&out, &source, &source, &source, p, &entries)?;
            if let Some(coverage) = self.vector_coverage(node, world, source.roi)? {
                let masked = self.alloc(source.roi)?;
                let p = Self::q14_params(
                    26,
                    source.roi,
                    [Some(out.roi), Some(coverage.roi)],
                    [0; 4],
                    None,
                )?;
                self.dispatch(&masked, &out, &coverage, &out, p, &[])?;
                return Ok(masked);
            }
            return Ok(out);
        }
        if node.features.mask.is_none()
            && node.features.vector_mask.is_none()
            && node.features.clip_path.is_none()
            && node.opacity == 1.0
        {
            return Ok(source);
        }
        let out = self.alloc(source.roi)?;
        let mut p = Self::params(4, world.legacy()?);
        p.settings = [node.opacity, 0.0, 0.0, 0.0];
        let mut entries = vec![];
        if let Some(mask) = &node.features.mask {
            let (e, transform) = self.mask_entries(
                mask,
                world,
                source.roi,
                if self.mask_preview {
                    Some(node.key)
                } else {
                    None
                },
            )?;
            entries = e;
            p = Self::params(4, transform.legacy()?);
            p.settings = [node.opacity, 1.0, 0.0, 0.0];
        }
        self.dispatch(&out, &source, &source, &source, p, &entries)?;
        if let Some(mask) = self.vector_coverage(node, world, source.roi)? {
            let masked = self.alloc(source.roi)?;
            self.dispatch(
                &masked,
                &out,
                &mask,
                &out,
                Self::params(11, world.legacy()?),
                &[],
            )?;
            Ok(masked)
        } else {
            Ok(out)
        }
    }
    pub(super) fn blend(
        &mut self,
        dst: Target,
        src: Target,
        node: &RNode,
        clip: Option<Target>,
    ) -> Result<Target> {
        let out = self.alloc(dst.roi)?;
        let mut p;
        if self.resources.exact {
            // Version 3 mode 19: the Q14 layer composite with the host-quantized
            // opacity. Clipping is refused. A masked node's opacity was consumed
            // by `apply_props` (mode 21), so it composites at opacity 1.
            let opacity = if node.features.mask.is_some()
                || node.features.vector_mask.is_some()
                || node.features.clip_path.is_some()
            {
                1.0
            } else {
                node.opacity
            };
            p = Self::q14_params(
                19,
                dst.roi,
                [Some(dst.roi), Some(src.roi)],
                [
                    crate::composite_opacity_q14(opacity),
                    node.blend as i32,
                    0,
                    0,
                ],
                None,
            )?;
        } else {
            p = Self::params(1, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            p.settings = [1.0, mode(node.blend), f32::from(clip.is_some()), 0.0];
        }
        self.dispatch(&out, &dst, &src, clip.as_ref().unwrap_or(&dst), p, &[])?;
        Ok(out)
    }
    pub(super) fn clip_blend(
        &mut self,
        base: Target,
        source: Target,
        node: &RNode,
    ) -> Result<Target> {
        let out = self.alloc(base.roi)?;
        let p = if self.resources.exact {
            let opacity = if node.features.mask.is_some()
                || node.features.vector_mask.is_some()
                || node.features.clip_path.is_some()
            {
                1.0
            } else {
                node.opacity
            };
            Self::q14_params(
                27,
                base.roi,
                [Some(base.roi), Some(source.roi)],
                [
                    crate::composite_opacity_q14(opacity),
                    node.blend as i32,
                    0,
                    0,
                ],
                None,
            )?
        } else {
            let mut p = Self::params(9, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
            p.settings[1] = mode(node.blend);
            p
        };
        self.dispatch(&out, &base, &source, &base, p, &[])?;
        Ok(out)
    }
    fn gaussian(
        &mut self,
        input: &Target,
        roi: Roi,
        world: WorldTransform,
        radius: f32,
        radius_q8: u32,
    ) -> Result<Target> {
        let filtered = if self.resources.exact {
            let grid = RoiQ::from_roi(roi)?;
            let (r, rho8) = grid.taps(radius_q8)?;
            if r == 0 {
                input.clone()
            } else {
                let weights = super::blur_kernel_q16(r, rho8).ok_or_else(|| {
                    RenderError::InvalidArgument("v3 blur kernel out of range".into())
                })?;
                let middle_roi = grid.expand(0, r)?.to_roi()?;
                let middle = self.alloc(middle_roi)?;
                let horizontal = Self::blur_params(middle_roi, input.roi, r, &weights, 0)?;
                self.dispatch(&middle, input, input, input, horizontal, &[])?;
                let out = self.alloc(roi)?;
                let vertical = Self::blur_params(roi, middle_roi, r, &weights, 1)?;
                self.dispatch(&out, &middle, &middle, &middle, vertical, &[])?;
                out
            }
        } else {
            let r = (radius / roi.scale).ceil() as u32;
            if r == 0 {
                input.clone()
            } else {
                let middle = self.alloc(roi.expand(0, r))?;
                let mut p = Self::params(2, world.legacy()?);
                p.settings = [(radius / roi.scale / 3.0).max(0.333333), 0.0, 0.0, r as f32];
                self.dispatch(&middle, input, input, input, p, &[])?;
                let out = self.alloc(roi)?;
                p.info[3] = 1;
                self.dispatch(&out, &middle, &middle, &middle, p, &[])?;
                out
            }
        };
        Ok(filtered)
    }
    pub(super) fn stage(
        &mut self,
        input: Target,
        effect: &REffect,
        roi: Roi,
        world: WorldTransform,
    ) -> Result<Target> {
        let filtered = match effect.kind {
            REffectKind::GaussianBlur { radius, radius_q8 } => {
                self.gaussian(&input, roi, world, radius, radius_q8)?
            }
            REffectKind::UnsharpMask { .. } if unsharp_is_identity(&effect.kind) => {
                return self.crop(input, roi);
            }
            REffectKind::MotionBlur { distance_q8: 0, .. } => return self.crop(input, roi),
            REffectKind::MotionBlur { .. } => {
                return Err(RenderError::Internal(
                    "motion blur requires an integer motion-blur stage".into(),
                ));
            }
            REffectKind::Noise { .. } if !self.resources.exact => {
                return Err(RenderError::Internal(
                    "noise requires an integer noise stage".into(),
                ));
            }
            REffectKind::Noise {
                seed,
                amount_q14,
                monochrome,
            } => {
                let grid = RoiQ::from_roi(roi)?;
                if grid.level > 0 {
                    return Err(RenderError::Internal(
                        "noise is evaluated on level zero or zoom-in grids".into(),
                    ));
                }
                if grid
                    .ox
                    .checked_add(i64::from(grid.w.saturating_sub(1)))
                    .is_none()
                    || grid
                        .oy
                        .checked_add(i64::from(grid.h.saturating_sub(1)))
                        .is_none()
                {
                    return Err(RenderError::InvalidArgument(
                        "noise ROI coordinate overflow".into(),
                    ));
                }
                let shift = u32::try_from(-grid.level)
                    .map_err(|_| RenderError::InvalidArgument("noise zoom level".into()))?;
                let mut params = Self::q14_params(
                    37,
                    roi,
                    [Some(input.roi), None],
                    [
                        i32::from_ne_bytes(seed.to_ne_bytes()),
                        amount_q14 as i32,
                        i32::from(monochrome),
                        shift as i32,
                    ],
                    None,
                )?;
                let words: &mut [u32] = bytemuck::cast_slice_mut(&mut params.curve);
                let ox = grid.ox as u64;
                let oy = grid.oy as u64;
                words[24] = ox as u32;
                words[25] = (ox >> 32) as u32;
                words[26] = oy as u32;
                words[27] = (oy >> 32) as u32;
                let out = self.alloc(roi)?;
                self.dispatch(&out, &input, &input, &input, params, &[])?;
                out
            }
            REffectKind::HighPass { .. } | REffectKind::UnsharpMask { .. } => {
                return Err(RenderError::Internal(
                    "effect requires a smoothing stack".into(),
                ));
            }
            REffectKind::ColorAdjust {
                brightness,
                contrast,
                saturation,
            } => {
                let out = self.alloc(roi)?;
                // Version 1 and 2 keep the float formula in mode 3. Version 3
                // uses the Q14 formula in mode 17, with the Q14 settings in `grid`.
                let p = if self.resources.exact {
                    Self::q14_params(
                        17,
                        roi,
                        [Some(input.roi), None],
                        [
                            super::color_adjust_q14(brightness),
                            super::color_adjust_q14(contrast),
                            super::color_adjust_q14(saturation),
                            0,
                        ],
                        None,
                    )?
                } else {
                    let mut p = Self::params(3, world.legacy()?);
                    p.settings = [brightness, contrast, saturation, 0.0];
                    p
                };
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
            REffectKind::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
            } => {
                let out = self.alloc(roi)?;
                let p = if self.resources.exact {
                    let gamma_q = super::levels_gamma_q14(gamma);
                    let inverse_gamma_q = ((i64::from(1_i32 << 14).pow(2) + i64::from(gamma_q / 2))
                        / i64::from(gamma_q)) as i32;
                    let mut p = Self::q14_params(
                        32,
                        roi,
                        [Some(input.roi), None],
                        [
                            super::color_adjust_q14(in_black),
                            super::color_adjust_q14(in_white),
                            inverse_gamma_q,
                            super::color_adjust_q14(out_black),
                        ],
                        None,
                    )?;
                    let words: &mut [u32] = bytemuck::cast_slice_mut(&mut p.curve);
                    words[24] = super::color_adjust_q14(out_white) as u32;
                    p
                } else {
                    let mut p = Self::params(13, world.legacy()?);
                    p.settings = [in_black, in_white, gamma, out_black];
                    p.extra[0] = out_white;
                    p
                };
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
            REffectKind::HueSaturation {
                hue,
                saturation,
                lightness,
            } => {
                let out = self.alloc(roi)?;
                let p = if self.resources.exact {
                    let hue_q16 = crate::raster_q8::q_from_f32_bits(hue.to_bits(), 16)
                        .and_then(|q| i32::try_from(q).ok())
                        .ok_or_else(|| {
                            RenderError::InvalidArgument("v3 hue out of range".into())
                        })?;
                    Self::q14_params(
                        33,
                        roi,
                        [Some(input.roi), None],
                        [
                            hue_q16,
                            super::color_adjust_q14(saturation),
                            super::color_adjust_q14(lightness),
                            0,
                        ],
                        None,
                    )?
                } else {
                    let mut p = Self::params(15, world.legacy()?);
                    p.settings = [hue, saturation, lightness, 0.0];
                    p
                };
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
            REffectKind::Invert => {
                let out = self.alloc(roi)?;
                let p = if self.resources.exact {
                    Self::q14_params(30, roi, [Some(input.roi), None], [0; 4], None)?
                } else {
                    Self::params(14, world.legacy()?)
                };
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
            REffectKind::Curves {
                ref points,
                channels,
            } => {
                let out = self.alloc(roi)?;
                // Bit i set: the curve maps color channel i (red, green, blue).
                let mask = channels
                    .iter()
                    .enumerate()
                    .fold(0, |m, (i, on)| m | (i32::from(*on) << i));
                let p = if self.resources.exact {
                    let mut p = Self::q14_params(
                        31,
                        roi,
                        [Some(input.roi), None],
                        [points.len().min(16) as i32, mask, 0, 0],
                        None,
                    )?;
                    let words: &mut [u32] = bytemuck::cast_slice_mut(&mut p.curve);
                    for (i, (x, y)) in points.iter().take(16).enumerate() {
                        words[24 + i * 2] = super::color_adjust_q14(*x) as u32;
                        words[25 + i * 2] = super::color_adjust_q14(*y) as u32;
                    }
                    p
                } else {
                    let mut p = Self::params(16, world.legacy()?);
                    for (i, (x, y)) in points.iter().take(16).enumerate() {
                        p.curve[i * 4] = *x;
                        p.curve[i * 4 + 1] = *y;
                    }
                    p.extra[0] = points.len().min(16) as f32;
                    p.extra[1] = mask as f32;
                    p
                };
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
        };
        self.select_effect(&input, filtered, effect, roi, world)
    }
    fn select_effect(
        &mut self,
        original: &Target,
        filtered: Target,
        effect: &REffect,
        roi: Roi,
        world: WorldTransform,
    ) -> Result<Target> {
        if let Some(mask) = &effect.selection {
            let out = self.alloc(roi)?;
            let (entries, transform) = self.mask_entries(mask, world, roi, None)?;
            let p = if self.resources.exact {
                Self::mask_params(22, Some(transform), roi, original.roi, filtered.roi, [0; 4])?
            } else {
                Self::params(5, transform.legacy()?)
            };
            self.dispatch(&out, original, &filtered, original, p, &entries)?;
            Ok(out)
        } else {
            Ok(filtered)
        }
    }
    pub(super) fn effects(
        &mut self,
        input: Target,
        effects: &[REffect],
        desired: Roi,
        world: WorldTransform,
    ) -> Result<Target> {
        let mut outputs = vec![desired; effects.len()];
        let mut roi = desired;
        for (i, e) in effects.iter().enumerate().rev() {
            outputs[i] = roi;
            if let REffectKind::GaussianBlur { radius, radius_q8 } = e.kind {
                if self.resources.exact {
                    let grid = RoiQ::from_roi(roi)?;
                    let (r, _) = grid.taps(radius_q8)?;
                    roi = grid.expand(r, r)?.to_roi()?;
                } else {
                    let r = (radius / roi.scale).ceil() as u32;
                    roi = roi.expand(r, r);
                }
            }
        }
        let mut source = input;
        for (e, roi) in effects.iter().zip(outputs) {
            source = self.stage(source, e, roi, world)?;
        }
        Ok(source)
    }
    pub(super) fn stack(
        &mut self,
        input: StackInput<'_>,
        effects: &[REffect],
        desired: Roi,
        world: WorldTransform,
    ) -> Result<Target> {
        if effects.is_empty() {
            return self.stack_input(input, desired);
        }
        if !self.resources.exact {
            return self.legacy_stack(input, effects, desired, world);
        }

        let desired = RoiQ::from_roi(desired)?;
        let plan = stack_plan(effects, desired)?;
        let mut previous = Planes::new();
        for plane in plan[0].values() {
            let grid = plane.grid;
            previous.insert(
                grid.level,
                (self.stack_input(input, grid.to_roi()?)?, plane.uses),
            );
        }
        for (effect, requests) in effects.iter().zip(&plan[1..]) {
            let mut current = Planes::new();
            for plane in requests.values() {
                let grid = plane.grid;
                let roi = grid.to_roi()?;
                let filtered = if let Some((sample, _)) = smoothing_grid(effect, grid)? {
                    let source = use_plane(&mut previous, sample.level);
                    if matches!(effect.kind, REffectKind::MotionBlur { .. }) {
                        let sample_roi = sample.to_roi()?;
                        let output = self.alloc(sample_roi)?;
                        let params = Self::q14_params(
                            36,
                            sample_roi,
                            [Some(source.roi), None],
                            motion_blur_settings(&effect.kind, sample)?,
                            None,
                        )?;
                        self.dispatch(&output, &source, &source, &source, params, &[])?;
                        let filtered = self.crop(output, roi)?;
                        if needs_original(effect) {
                            let original = if sample.level == grid.level {
                                self.crop(source, roi)?
                            } else {
                                use_plane(&mut previous, grid.level)
                            };
                            self.select_effect(&original, filtered, effect, roi, world)?
                        } else {
                            filtered
                        }
                    } else {
                        let (radius, radius_q8) =
                            smoothing_radius(&effect.kind).expect("smoothing effect");
                        let blurred =
                            self.gaussian(&source, sample.to_roi()?, world, radius, radius_q8)?;
                        let original = (sample.level == grid.level && needs_original(effect))
                            .then_some(source);
                        let blurred = self.crop(blurred, roi)?;
                        if needs_original(effect) {
                            let original =
                                original.unwrap_or_else(|| use_plane(&mut previous, grid.level));
                            let combine = match effect.kind {
                                REffectKind::HighPass { .. } => Some((34, [0; 4])),
                                REffectKind::UnsharpMask {
                                    amount_q8,
                                    threshold_q14,
                                    ..
                                } => Some((35, [amount_q8 as i32, threshold_q14 as i32, 0, 0])),
                                _ => None,
                            };
                            let filtered = if let Some((mode, settings)) = combine {
                                let output = self.alloc(roi)?;
                                let params = Self::q14_params(
                                    mode,
                                    roi,
                                    [Some(original.roi), Some(blurred.roi)],
                                    settings,
                                    None,
                                )?;
                                self.dispatch(
                                    &output,
                                    &original,
                                    &blurred,
                                    &original,
                                    params,
                                    &[],
                                )?;
                                output
                            } else {
                                blurred
                            };
                            self.select_effect(&original, filtered, effect, roi, world)?
                        } else {
                            blurred
                        }
                    }
                } else {
                    self.stage(use_plane(&mut previous, grid.level), effect, roi, world)?
                };
                current.insert(grid.level, (filtered, plane.uses));
            }
            debug_assert!(previous.is_empty());
            previous = current;
        }
        Ok(use_plane(&mut previous, desired.level))
    }

    fn stack_input(&mut self, input: StackInput<'_>, desired: Roi) -> Result<Target> {
        match input {
            StackInput::Node {
                index,
                world,
                depth,
            } => self.raw_node(index, desired, world, depth),
            #[cfg(test)]
            StackInput::Siblings {
                children,
                world,
                depth,
            } => {
                let blank = self.zero(desired)?;
                self.children(children, blank, world, depth)
            }
            StackInput::Planes(planes) => {
                let plane = planes
                    .get(&super::graph::scale_key(desired))
                    .filter(|plane| super::graph::covers(plane.roi, desired))
                    .ok_or(RenderError::Internal("unplanned backdrop region".into()))?;
                self.crop(plane.clone(), desired)
            }
        }
    }

    /// The inputs `stack` reads for `effects` at `desired`, one ROI per
    /// sampling scale.
    pub(super) fn stack_inputs(&self, effects: &[REffect], desired: Roi) -> Result<Vec<Roi>> {
        if effects.is_empty() {
            return Ok(vec![desired]);
        }
        if self.resources.exact {
            let plan = stack_plan(effects, RoiQ::from_roi(desired)?)?;
            return plan[0].values().map(|plane| plane.grid.to_roi()).collect();
        }
        // The requests of `legacy_stack`, joined per scale at each prefix.
        let mut requests = super::graph::Requests::new();
        requests.insert(super::graph::scale_key(desired), desired);
        for effect in effects.iter().rev() {
            let mut inputs = super::graph::Requests::new();
            for &roi in requests.values() {
                if let REffectKind::GaussianBlur { radius, .. } = effect.kind {
                    let sample = roi.coarsen(radius / 64.0);
                    let halo = (radius / sample.scale).ceil() as u32;
                    self.request(&mut inputs, sample.expand(halo, halo))?;
                    if effect.selection.is_some() && sample.scale != roi.scale {
                        self.request(&mut inputs, roi)?;
                    }
                } else {
                    self.request(&mut inputs, roi)?;
                }
            }
            requests = inputs;
        }
        Ok(requests.into_values().collect())
    }

    fn legacy_stack(
        &mut self,
        input: StackInput<'_>,
        effects: &[REffect],
        desired: Roi,
        world: WorldTransform,
    ) -> Result<Target> {
        let Some((effect, previous)) = effects.split_last() else {
            return self.stack_input(input, desired);
        };
        if let REffectKind::GaussianBlur { radius, radius_q8 } = effect.kind {
            let sample = desired.coarsen(radius / 64.0);
            let halo = (radius / sample.scale).ceil() as u32;
            let source = self.legacy_stack(input, previous, sample.expand(halo, halo), world)?;
            let filtered = self.gaussian(&source, sample, world, radius, radius_q8)?;
            let filtered = self.crop(filtered, desired)?;
            if effect.selection.is_some() {
                let original = if sample.scale == desired.scale {
                    source
                } else {
                    self.legacy_stack(input, previous, desired, world)?
                };
                return self.select_effect(&original, filtered, effect, desired, world);
            }
            Ok(filtered)
        } else {
            let source = self.legacy_stack(input, previous, desired, world)?;
            self.stage(source, effect, desired, world)
        }
    }
    pub(super) fn input_roi(effects: &[REffect], mut roi: Roi) -> Roi {
        for effect in effects.iter().rev() {
            if let Some((radius, _)) = smoothing_radius(&effect.kind) {
                let halo = (radius / roi.scale).ceil() as u32;
                roi = roi.expand(halo, halo);
            }
        }
        roi
    }

    pub(super) fn input_roi_v3(effects: &[REffect], roi: Roi) -> Result<Roi> {
        let mut grid = RoiQ::from_roi(roi)?;
        for effect in effects.iter().rev() {
            if let Some((_, radius_q8)) = smoothing_radius(&effect.kind) {
                grid = grid.grow(radius_q8)?;
            }
        }
        grid.to_roi()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_pass_input_roi_matches_blur() {
        let desired = RoiQ {
            level: -4,
            ox: -65,
            oy: 40,
            w: 256,
            h: 256,
        };
        let radius = 13.5;
        let radius_q8 = 3456;
        let blur = REffect {
            kind: REffectKind::GaussianBlur { radius, radius_q8 },
            selection: None,
        };
        let high_pass = REffect {
            kind: REffectKind::HighPass { radius, radius_q8 },
            selection: None,
        };
        assert_eq!(
            smoothing_grid(&blur, desired).unwrap(),
            smoothing_grid(&high_pass, desired).unwrap()
        );
    }

    #[test]
    fn motion_blur_roi_expands_by_half_distance() {
        let effect = REffect {
            kind: REffectKind::MotionBlur {
                angle_q16: 45 * 65536,
                distance_q8: 32 * 256,
            },
            selection: None,
        };
        let desired = Roi {
            x: 0.0,
            y: 0.0,
            scale: 1.0,
            w: 256,
            h: 256,
        };
        let input = Context::input_roi_v3(std::slice::from_ref(&effect), desired).unwrap();
        assert_eq!(
            (input.x, input.y, input.w, input.h),
            (-16.0, -16.0, 288, 288)
        );
        let grid = RoiQ::from_roi(desired).unwrap();
        assert_eq!(
            motion_blur_settings(&effect.kind, grid).unwrap(),
            [16, 256, 45 * 65536, 0]
        );
    }

    #[test]
    fn selected_blur_stack_evaluates_each_prefix_once_per_level() {
        let mask = alto_scene::RMask {
            tiles: vec![],
            linked: false,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        };
        let effects: Vec<_> = [64.0, 32.0, 16.0, 8.0, 4.0, 2.0, 1.0, 0.5]
            .into_iter()
            .cycle()
            .take(32)
            .map(|radius| REffect {
                kind: REffectKind::GaussianBlur {
                    radius,
                    radius_q8: alto_core::blur::blur_radius_q8_f32(radius).unwrap(),
                },
                selection: Some(mask.clone()),
            })
            .collect();
        for level in -6..=0 {
            let plan = stack_plan(
                &effects,
                RoiQ {
                    level,
                    ox: -17,
                    oy: 31,
                    w: 256,
                    h: 256,
                },
            )
            .unwrap();
            assert_eq!(plan.len(), 33);
            let plane_count: usize = plan.iter().map(BTreeMap::len).sum();
            assert!(plane_count <= 33 * (1 - level) as usize);
            // A shared request must be retained across its earlier consumers.
            if level < 0 {
                assert!(plan.iter().flatten().any(|(_, plane)| plane.uses > 1));
            }
        }
    }

    #[test]
    fn effect_stack_releases_each_lease_at_its_last_consumer() {
        let mut planes = Planes::new();
        let lease = Arc::new(());
        let weak = Arc::downgrade(&lease);
        let target = Target {
            index: 1,
            _lease: lease,
            roi: RoiQ {
                level: 0,
                ox: 0,
                oy: 0,
                w: 1,
                h: 1,
            }
            .to_roi()
            .unwrap(),
        };
        planes.insert(0, (target, 2));
        drop(use_plane(&mut planes, 0));
        assert_eq!(weak.strong_count(), 1);
        drop(use_plane(&mut planes, 0));
        assert_eq!(weak.strong_count(), 0);
        assert!(planes.is_empty());
    }
}
