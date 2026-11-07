//! Layer properties, blending, clipping and effect stages.
use super::*;

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
    pub(super) fn stage(
        &mut self,
        input: Target,
        effect: &REffect,
        roi: Roi,
        world: WorldTransform,
    ) -> Result<Target> {
        // A selection on either integer stage is mode 22 below.
        if self.resources.exact
            && !matches!(
                effect.kind,
                REffectKind::ColorAdjust { .. }
                    | REffectKind::GaussianBlur { .. }
                    | REffectKind::Invert
                    | REffectKind::Curves { .. }
                    | REffectKind::Levels { .. }
                    | REffectKind::HueSaturation { .. }
            )
        {
            return Err(RenderError::Internal(
                "v3 effect without an integer stage reached the M5 renderer".into(),
            ));
        }
        let filtered = match effect.kind {
            REffectKind::GaussianBlur { radius, radius_q8 } => {
                if self.resources.exact {
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
                        self.dispatch(&middle, &input, &input, &input, horizontal, &[])?;
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
                        self.dispatch(&middle, &input, &input, &input, p, &[])?;
                        let out = self.alloc(roi)?;
                        p.info[3] = 1;
                        self.dispatch(&out, &middle, &middle, &middle, p, &[])?;
                        out
                    }
                }
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
            REffectKind::Curves { ref points } => {
                let out = self.alloc(roi)?;
                let p = if self.resources.exact {
                    let mut p = Self::q14_params(
                        31,
                        roi,
                        [Some(input.roi), None],
                        [points.len().min(16) as i32, 0, 0, 0],
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
                    p
                };
                self.dispatch(&out, &input, &input, &input, p, &[])?;
                out
            }
        };
        if let Some(mask) = &effect.selection {
            let out = self.alloc(roi)?;
            let (entries, transform) = self.mask_entries(mask, world, roi, None)?;
            let p = if self.resources.exact {
                Self::mask_params(22, Some(transform), roi, input.roi, filtered.roi, [0; 4])?
            } else {
                Self::params(5, transform.legacy()?)
            };
            self.dispatch(&out, &input, &filtered, &input, p, &entries)?;
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
        let Some((effect, previous)) = effects.split_last() else {
            return match input {
                StackInput::Node {
                    index,
                    world,
                    depth,
                } => self.raw_node(index, desired, world, depth),
                StackInput::Siblings {
                    children,
                    world,
                    depth,
                } => {
                    let blank = self.zero(desired)?;
                    self.children(children, blank, world, depth)
                }
            };
        };
        if let REffectKind::GaussianBlur { radius, .. } = effect.kind {
            let sample = if self.resources.exact {
                let grid = RoiQ::from_roi(desired)?;
                let target = alto_core::blur::blur_coarsen_k(radius)
                    .map(|k| k.max(grid.level))
                    .unwrap_or(grid.level);
                grid.coarsen(target)?.to_roi()?
            } else {
                desired.coarsen(radius / 64.0)
            };
            let source = self.stack(
                input,
                previous,
                if self.resources.exact {
                    Self::input_roi_v3(std::slice::from_ref(effect), sample)?
                } else {
                    Self::input_roi(std::slice::from_ref(effect), sample)
                },
                world,
            )?;
            let mut plain = effect.clone();
            plain.selection = None;
            let filtered = self.stage(source, &plain, sample, world)?;
            let filtered = self.crop(filtered, desired)?;
            if let Some(mask) = &effect.selection {
                let original = self.stack(input, previous, desired, world)?;
                let out = self.alloc(desired)?;
                let (entries, transform) = self.mask_entries(mask, world, desired, None)?;
                let p = if self.resources.exact {
                    Self::mask_params(
                        22,
                        Some(transform),
                        desired,
                        original.roi,
                        filtered.roi,
                        [0; 4],
                    )?
                } else {
                    Self::params(5, transform.legacy()?)
                };
                self.dispatch(&out, &original, &filtered, &original, p, &entries)?;
                Ok(out)
            } else {
                Ok(filtered)
            }
        } else {
            let source = self.stack(input, previous, desired, world)?;
            self.stage(source, effect, desired, world)
        }
    }
    pub(super) fn input_roi(effects: &[REffect], mut roi: Roi) -> Roi {
        for e in effects.iter().rev() {
            if let REffectKind::GaussianBlur { radius, .. } = e.kind {
                let r = (radius / roi.scale).ceil() as u32;
                roi = roi.expand(r, r);
            }
        }
        roi
    }
    pub(super) fn input_roi_v3(effects: &[REffect], roi: Roi) -> Result<Roi> {
        let mut grid = RoiQ::from_roi(roi)?;
        for effect in effects.iter().rev() {
            if let REffectKind::GaussianBlur { radius_q8, .. } = effect.kind {
                let (r, _) = grid.taps(radius_q8)?;
                grid = grid.expand(r, r)?;
            }
        }
        grid.to_roi()
    }
}
