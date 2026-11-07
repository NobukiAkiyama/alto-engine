//! Walking the render tree: children, nodes and their stacks.
use super::*;

impl Context<'_> {
    pub(super) fn children(
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
                let filtered = if stack.iter().any(|e| {
                    if self.resources.exact {
                        matches!(e.kind,REffectKind::GaussianBlur {radius_q8, ..} if radius_q8>0)
                    } else {
                        matches!(e.kind,REffectKind::GaussianBlur {radius, ..} if radius>0.0)
                    }
                }) {
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
    pub(super) fn node(
        &mut self,
        index: usize,
        desired: Roi,
        parent: WorldTransform,
        depth: usize,
    ) -> Result<Target> {
        if depth > alto_core::MAX_TREE_DEPTH as usize {
            return Err(RenderError::InvalidArgument("effect graph too deep".into()));
        }
        let node = self.scene.nodes[index].clone();
        let world = parent.concat(node.transform)?;
        let source = self.stack(
            StackInput::Node {
                index,
                world,
                depth,
            },
            &node.features.effects,
            desired,
            world,
        )?;
        self.apply_props(source, &node, world)
    }
    pub(super) fn raw_node(
        &mut self,
        index: usize,
        roi: Roi,
        world: WorldTransform,
        depth: usize,
    ) -> Result<Target> {
        let node = self.scene.nodes[index].clone();
        let source = match &node.content {
            RContent::Tiles(tiles) => {
                let layer = if self.mask_preview {
                    None
                } else {
                    Some(node.key)
                };
                // Version 3 mode 18 reads the nearest texel through the Q16 lookup
                // (`mask_sampler`, as the masks) and premultiplies its bytes into
                // Q14. A singular transform draws nothing, as the f64 selection
                // below. Version 1 and 2 keep the f32 inverse and mode 0.
                if self.resources.exact {
                    if v3_inverse(world.fixed()?)?.is_none() {
                        return self.zero(roi);
                    }
                    let (entries, sampler) =
                        self.lookup_entries(tiles, world, roi, layer, "raster")?;
                    let out = self.alloc(roi)?;
                    let p = Self::q14_params(18, roi, [None, None], [0; 4], Some(&sampler))?;
                    let dummy = self.dummy();
                    self.dispatch(&out, &dummy, &dummy, &dummy, p, &entries)?;
                    return Ok(out);
                }
                let out = self.alloc(roi)?;
                let entries = self.entries(tiles, world.legacy()?, roi, layer)?;
                let p = Self::params(0, world.legacy()?);
                let dummy = self.dummy();
                self.dispatch(&out, &dummy, &dummy, &dummy, p, &entries)?;
                out
            }
            RContent::Mesh(paths) => {
                if self.resources.exact {
                    self.vector_q14(paths, world.fixed()?, roi)?
                } else {
                    self.vector(VectorContent::Paths(paths), world.legacy()?, roi, true)?
                }
            }
            RContent::Text(runs) => {
                self.vector(VectorContent::Text(runs), world.legacy()?, roi, false)?
            }
            RContent::TextBake(bake) => {
                if !self.resources.exact {
                    return Err(RenderError::Internal(
                        "a text bake reached a v1 / v2 renderer".into(),
                    ));
                }
                self.text_q14(bake.as_ref(), world.fixed()?, roi)?
            }
            RContent::Group { children, .. } => {
                let blank = self.zero(roi)?;
                self.children(children, blank, world, depth)?
            }
            _ => self.zero(roi)?,
        };
        Ok(source)
    }
}
