//! The recursive sibling walk that the planned walk in `graph.rs` replaced.
//! Each backdrop adjustment renders its whole sibling prefix again, so its cost
//! grows exponentially with the number of backdrop blurs. Tests compare the
//! planned walk with it pixel for pixel.
use super::*;

impl Context<'_> {
    pub(super) fn children_reference(
        &mut self,
        children: &[usize],
        mut accum: Target,
        world: WorldTransform,
        depth: usize,
    ) -> Result<Target> {
        let mut base = None;
        let mut base_background = None;
        let mut base_node: Option<RNode> = None;
        let mut base_position = 0;
        for (position, &index) in children.iter().enumerate() {
            let node = self.scene.nodes[index].clone();
            if !node.visible {
                if !node.features.clip {
                    base = None;
                }
                continue;
            }
            let transform = world.concat(node.transform)?;
            if let RContent::Adjustment(effect) = &node.content {
                let mut stack = vec![effect.clone()];
                stack.extend(node.features.effects.clone());
                // A backdrop blur needs extra input from the same siblings. The caller renders that halo below.
                let input = if node.features.clip {
                    let Some(base) = base else {
                        continue;
                    };
                    base
                } else {
                    accum
                };
                let desired = input.roi;
                let filtered = if stack
                    .iter()
                    .any(|effect| super::stages::needs_backdrop_halo(effect, self.resources.exact))
                {
                    let prefix = if node.features.clip {
                        &children[base_position..position]
                    } else {
                        &children[..position]
                    };
                    self.stack(
                        StackInput::Siblings {
                            children: prefix,
                            world,
                            depth: depth + 1,
                        },
                        &stack,
                        desired,
                        transform,
                    )?
                } else {
                    self.effects(input.clone(), &stack, desired, transform)?
                };
                let mut mixed = filtered.clone();
                if self.resources.exact {
                    // Mode 23 combines opacity, blend, and an optional mask.
                    // Otherwise only the opacity mixes, in Q14 (mode 20).
                    if node.features.mask.is_some() || node.blend != RenderBlend::Normal {
                        let out = self.alloc(desired)?;
                        let (entries, mask_transform, has_mask) = match &node.features.mask {
                            Some(mask) => {
                                let (e, t) = self.mask_entries(
                                    mask,
                                    transform,
                                    desired,
                                    if self.mask_preview {
                                        Some(node.key)
                                    } else {
                                        None
                                    },
                                )?;
                                (e, Some(t), 1)
                            }
                            None => (vec![], None, 0),
                        };
                        let p = Self::mask_params(
                            23,
                            mask_transform,
                            desired,
                            input.roi,
                            filtered.roi,
                            [
                                crate::composite_opacity_q14(node.opacity),
                                has_mask,
                                node.blend as i32,
                                0,
                            ],
                        )?;
                        self.dispatch(&out, &input, &filtered, &input, p, &entries)?;
                        mixed = out;
                    } else if node.opacity != 1.0 {
                        let out = self.alloc(desired)?;
                        let p = Self::q14_params(
                            20,
                            desired,
                            [Some(input.roi), Some(filtered.roi)],
                            [crate::composite_opacity_q14(node.opacity), 0, 0, 0],
                            None,
                        )?;
                        self.dispatch(&out, &input, &filtered, &input, p, &[])?;
                        mixed = out;
                    }
                } else if node.features.mask.is_some()
                    || node.opacity != 1.0
                    || node.features.clip
                    || node.blend != RenderBlend::Normal
                {
                    let out = self.alloc(desired)?;
                    let mut p = Self::params(7, transform.legacy()?);
                    p.settings = [node.opacity, 0.0, f32::from(node.features.clip), 0.0];
                    let mut entries = vec![];
                    if let Some(mask) = &node.features.mask {
                        let (e, t) = self.mask_entries(
                            mask,
                            transform,
                            desired,
                            if self.mask_preview {
                                Some(node.key)
                            } else {
                                None
                            },
                        )?;
                        entries = e;
                        p = Self::params(7, t.legacy()?);
                        p.settings = [node.opacity, 1.0, f32::from(node.features.clip), 0.0];
                    }
                    p.settings[2] = 0.0;
                    p.settings[3] = mode(node.blend);
                    self.dispatch(&out, &input, &filtered, &input, p, &entries)?;
                    mixed = out;
                }
                if let Some(mask) = self.vector_coverage(&node, transform, desired)? {
                    let out = self.alloc(desired)?;
                    let p = if self.resources.exact {
                        Self::q14_params(
                            29,
                            desired,
                            [Some(input.roi), Some(mixed.roi)],
                            [0; 4],
                            None,
                        )?
                    } else {
                        Self::params(12, transform.legacy()?)
                    };
                    self.dispatch(&out, &input, &mixed, &mask, p, &[])?;
                    mixed = out;
                }
                if node.features.clip {
                    let out = self.alloc(desired)?;
                    let p = if self.resources.exact {
                        Self::q14_params(
                            28,
                            desired,
                            [Some(input.roi), Some(mixed.roi)],
                            [0; 4],
                            None,
                        )?
                    } else {
                        Self::params(10, transform.legacy()?)
                    };
                    self.dispatch(&out, &input, &mixed, &input, p, &[])?;
                    base = Some(out.clone());
                    accum = self.blend(
                        base_background
                            .clone()
                            .ok_or(RenderError::Internal("clipping backdrop".into()))?,
                        out,
                        base_node
                            .as_ref()
                            .ok_or(RenderError::Internal("clipping base".into()))?,
                        None,
                    )?;
                } else {
                    accum = mixed;
                    base = None;
                    base_node = None;
                    base_background = None;
                }
                continue;
            }
            let pass = matches!(
                &node.content,
                RContent::Group {
                    isolated: false,
                    ..
                }
            ) && node.blend == RenderBlend::Normal
                && node.opacity == 1.0
                && node.features.mask.is_none()
                && node.features.vector_mask.is_none()
                && node.features.clip_path.is_none()
                && node.features.effects.is_empty()
                && !node.features.clip
                && !children
                    .get(position + 1)
                    .is_some_and(|i| self.scene.nodes[*i].features.clip);
            if pass
                && let RContent::Group { children, .. } = &node.content
                && !children
                    .iter()
                    .any(|i| matches!(self.scene.nodes[*i].content, RContent::Adjustment(_)))
            {
                accum = self.children(children, accum, transform, depth + 1)?;
                base = None;
                continue;
            }
            let src = self.node(index, accum.roi, world, depth + 1)?;
            if node.features.clip {
                if let Some(current) = base {
                    let clipped = self.clip_blend(current, src, &node)?;
                    base = Some(clipped.clone());
                    accum = self.blend(
                        base_background
                            .clone()
                            .ok_or(RenderError::Internal("clipping backdrop".into()))?,
                        clipped,
                        base_node
                            .as_ref()
                            .ok_or(RenderError::Internal("clipping base".into()))?,
                        None,
                    )?;
                }
            } else {
                base_position = position;
                base_background = Some(accum.clone());
                base_node = Some(node.clone());
                base = Some(src.clone());
                accum = self.blend(accum, src, &node, None)?;
            }
        }
        Ok(accum)
    }
}
