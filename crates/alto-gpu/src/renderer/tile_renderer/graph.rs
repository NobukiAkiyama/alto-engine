//! Walking the render tree: children, nodes and their stacks.
use super::*;

/// Planned ROIs of one sibling position, keyed by sampling scale.
pub(super) type Requests = BTreeMap<u32, Roi>;

pub(super) fn scale_key(roi: Roi) -> u32 {
    roi.scale.to_bits()
}

/// A backdrop adjustment that filters a halo of its sibling prefix.
struct HaloStep {
    stack: Vec<REffect>,
    /// The result mixes with the adjustment's input at its output levels.
    reads_input: bool,
}

/// The running state of one planned sampling level.
struct Lane {
    accum: Target,
    base: Option<Target>,
    background: Option<Target>,
}

impl Context<'_> {
    /// Composites `children` over `accum`. A backdrop adjustment reads its
    /// sibling prefix from the same walk instead of rendering it again.
    pub(super) fn children(
        &mut self,
        children: &[usize],
        accum: Target,
        world: WorldTransform,
        depth: usize,
    ) -> Result<Target> {
        let desired = accum.roi;
        self.siblings(children, Some(accum), desired, world, depth)
    }

    /// The effect stack of a visible adjustment.
    fn adjustment_stack(node: &RNode) -> Option<Vec<REffect>> {
        let RContent::Adjustment(effect) = &node.content else {
            return None;
        };
        let mut stack = vec![effect.clone()];
        stack.extend(node.features.effects.iter().cloned());
        Some(stack)
    }

    fn reads_backdrop_halo(&self, stack: &[REffect]) -> bool {
        stack
            .iter()
            .any(|effect| super::stages::needs_backdrop_halo(effect, self.resources.exact))
    }

    /// A non-isolated group composited straight into the backdrop.
    fn passes_through(&self, children: &[usize], position: usize, node: &RNode) -> bool {
        let RContent::Group {
            isolated: false,
            children: inner,
        } = &node.content
        else {
            return false;
        };
        node.blend == RenderBlend::Normal
            && node.opacity == 1.0
            && node.features.mask.is_none()
            && node.features.vector_mask.is_none()
            && node.features.clip_path.is_none()
            && node.features.effects.is_empty()
            && !node.features.clip
            && !children
                .get(position + 1)
                .is_some_and(|i| self.scene.nodes[*i].features.clip)
            && !inner
                .iter()
                .any(|i| matches!(self.scene.nodes[*i].content, RContent::Adjustment(_)))
    }

    /// The backdrop adjustments that read a halo of their sibling prefix, by
    /// position. A clipped one runs only over a base.
    fn halo_steps(&self, children: &[usize]) -> Vec<Option<HaloStep>> {
        let mut has_base = false;
        children
            .iter()
            .enumerate()
            .map(|(position, &index)| {
                let node = &self.scene.nodes[index];
                if !node.visible {
                    if !node.features.clip {
                        has_base = false;
                    }
                    return None;
                }
                if let Some(stack) = Self::adjustment_stack(node) {
                    let runs = !node.features.clip || has_base;
                    if !node.features.clip {
                        has_base = false;
                    }
                    return (runs && self.reads_backdrop_halo(&stack)).then(|| HaloStep {
                        stack,
                        reads_input: Self::mixes_input(node),
                    });
                }
                if self.passes_through(children, position, node) {
                    has_base = false;
                } else if !node.features.clip {
                    has_base = true;
                }
                None
            })
            .collect()
    }

    /// Whether `mix_adjustment` reads the adjustment's input. Otherwise the
    /// filtered backdrop replaces it.
    fn mixes_input(node: &RNode) -> bool {
        node.features.clip
            || node.features.mask.is_some()
            || node.features.vector_mask.is_some()
            || node.features.clip_path.is_some()
            || node.blend != RenderBlend::Normal
            || node.opacity != 1.0
    }

    pub(super) fn request(&self, requests: &mut Requests, roi: Roi) -> Result<()> {
        use std::collections::btree_map::Entry;
        match requests.entry(scale_key(roi)) {
            Entry::Vacant(entry) => {
                entry.insert(roi);
            }
            Entry::Occupied(mut entry) => {
                let planned = *entry.get();
                *entry.get_mut() = if self.resources.exact {
                    RoiQ::from_roi(planned)?
                        .union(RoiQ::from_roi(roi)?)?
                        .to_roi()?
                } else {
                    legacy_union(planned, roi)?
                };
            }
        }
        Ok(())
    }

    /// Plans, from the last sibling back, the region and sampling levels each
    /// prefix must cover. A backdrop adjustment needs its stack's inputs, halos
    /// and coarse levels included, and its own output levels too when it mixes
    /// the result with its input.
    fn plan_siblings(&self, steps: &[Option<HaloStep>], desired: Roi) -> Result<Vec<Requests>> {
        let mut plan = vec![Requests::new(); steps.len() + 1];
        plan[steps.len()].insert(scale_key(desired), desired);
        for (position, step) in steps.iter().enumerate().rev() {
            let mut requests = match step {
                Some(step) if !step.reads_input => Requests::new(),
                _ => plan[position + 1].clone(),
            };
            if let Some(step) = step {
                for &output in plan[position + 1].values() {
                    for input in self.stack_inputs(&step.stack, output)? {
                        self.request(&mut requests, input)?;
                    }
                }
            }
            plan[position] = requests;
        }
        Ok(plan)
    }

    /// Composites `children` once per planned sampling level. `backdrop` is
    /// `None` for an isolated, transparent start.
    pub(super) fn siblings(
        &mut self,
        children: &[usize],
        backdrop: Option<Target>,
        desired: Roi,
        world: WorldTransform,
        depth: usize,
    ) -> Result<Target> {
        #[cfg(test)]
        if REFERENCE_SIBLINGS.get() {
            let accum = match backdrop {
                Some(accum) => accum,
                None => self.zero(desired)?,
            };
            return self.children_reference(children, accum, world, depth);
        }
        let steps = self.halo_steps(children);
        let plan = self.plan_siblings(&steps, desired)?;
        let mut lanes = BTreeMap::new();
        match backdrop {
            Some(accum) => {
                // A backdrop that is not transparent reaches only a walk with
                // no backdrop adjustment (`passes_through`).
                if plan[0].len() != 1 || !plan[0].values().all(|roi| same_roi(*roi, accum.roi)) {
                    return Err(RenderError::Internal(
                        "a backdrop adjustment over an inherited backdrop".into(),
                    ));
                }
                lanes.insert(
                    scale_key(accum.roi),
                    Lane {
                        accum,
                        base: None,
                        background: None,
                    },
                );
            }
            None => {
                for (&key, &roi) in &plan[0] {
                    lanes.insert(
                        key,
                        Lane {
                            accum: self.zero(roi)?,
                            base: None,
                            background: None,
                        },
                    );
                }
            }
        }
        let mut base_node: Option<RNode> = None;
        for (position, &index) in children.iter().enumerate() {
            let node = self.scene.nodes[index].clone();
            // The sibling prefix that a backdrop adjustment filters, read before
            // the lanes shrink to this position's output.
            let prefix = match &steps[position] {
                Some(_) if node.features.clip => {
                    // The recursive walk rendered the clipping run from the base
                    // onto a transparent backdrop.
                    let base_node = base_node
                        .as_ref()
                        .ok_or(RenderError::Internal("clipping base".into()))?;
                    let mut prefix = BTreeMap::new();
                    for (&key, lane) in &lanes {
                        let base = lane
                            .base
                            .clone()
                            .ok_or(RenderError::Internal("clipping base".into()))?;
                        let blank = self.zero(base.roi)?;
                        prefix.insert(key, self.blend(blank, base, base_node, None)?);
                    }
                    Some(prefix)
                }
                Some(_) => Some(
                    lanes
                        .iter()
                        .map(|(&key, lane)| (key, lane.accum.clone()))
                        .collect::<BTreeMap<_, _>>(),
                ),
                None => None,
            };
            if let (Some(step), Some(prefix)) = (&steps[position], &prefix)
                && !step.reads_input
            {
                // The filtered prefix replaces the backdrop at every level.
                let transform = world.concat(node.transform)?;
                let mut next = BTreeMap::new();
                for (&key, &roi) in &plan[position + 1] {
                    let accum =
                        self.stack(StackInput::Planes(prefix), &step.stack, roi, transform)?;
                    next.insert(
                        key,
                        Lane {
                            accum,
                            base: None,
                            background: None,
                        },
                    );
                }
                lanes = next;
                base_node = None;
                continue;
            }
            lanes = self.shrink_lanes(lanes, &plan[position + 1])?;
            if !node.visible {
                if !node.features.clip {
                    for lane in lanes.values_mut() {
                        lane.base = None;
                    }
                }
                continue;
            }
            let transform = world.concat(node.transform)?;
            if let Some(stack) = Self::adjustment_stack(&node) {
                if node.features.clip && lanes.values().any(|lane| lane.base.is_none()) {
                    continue;
                }
                for lane in lanes.values_mut() {
                    let input = if node.features.clip {
                        lane.base.clone().expect("checked clipping base")
                    } else {
                        lane.accum.clone()
                    };
                    let desired = input.roi;
                    let filtered = if let Some(prefix) = &prefix {
                        self.stack(StackInput::Planes(prefix), &stack, desired, transform)?
                    } else {
                        self.effects(input.clone(), &stack, desired, transform)?
                    };
                    let mixed = self.mix_adjustment(&node, &input, filtered, transform)?;
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
                        lane.base = Some(out.clone());
                        lane.accum = self.blend(
                            lane.background
                                .clone()
                                .ok_or(RenderError::Internal("clipping backdrop".into()))?,
                            out,
                            base_node
                                .as_ref()
                                .ok_or(RenderError::Internal("clipping base".into()))?,
                            None,
                        )?;
                    } else {
                        lane.accum = mixed;
                        lane.base = None;
                        lane.background = None;
                    }
                }
                if !node.features.clip {
                    base_node = None;
                }
                continue;
            }
            if let RContent::Group {
                children: inner, ..
            } = &node.content
                && self.passes_through(children, position, &node)
            {
                for lane in lanes.values_mut() {
                    let accum = lane.accum.clone();
                    lane.accum = self.children(inner, accum, transform, depth + 1)?;
                    lane.base = None;
                }
                continue;
            }
            if node.features.clip {
                for lane in lanes.values_mut() {
                    let Some(current) = lane.base.take() else {
                        continue;
                    };
                    let src = self.node(index, lane.accum.roi, world, depth + 1)?;
                    let clipped = self.clip_blend(current, src, &node)?;
                    lane.base = Some(clipped.clone());
                    lane.accum = self.blend(
                        lane.background
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
                for lane in lanes.values_mut() {
                    let src = self.node(index, lane.accum.roi, world, depth + 1)?;
                    let accum = lane.accum.clone();
                    lane.background = Some(accum.clone());
                    lane.base = Some(src.clone());
                    lane.accum = self.blend(accum, src, &node, None)?;
                }
                base_node = Some(node);
            }
        }
        let mut lanes = self.shrink_lanes(lanes, &plan[children.len()])?;
        lanes
            .remove(&scale_key(desired))
            .map(|lane| lane.accum)
            .ok_or(RenderError::Internal("planned sibling output".into()))
    }

    /// Drops the levels no later sibling reads and crops the rest to the
    /// region still needed.
    fn shrink_lanes(
        &mut self,
        lanes: BTreeMap<u32, Lane>,
        requests: &Requests,
    ) -> Result<BTreeMap<u32, Lane>> {
        let mut kept = BTreeMap::new();
        for (key, lane) in lanes {
            let Some(&roi) = requests.get(&key) else {
                continue;
            };
            let base = lane.base.map(|t| self.crop(t, roi)).transpose()?;
            let background = lane.background.map(|t| self.crop(t, roi)).transpose()?;
            kept.insert(
                key,
                Lane {
                    accum: self.crop(lane.accum, roi)?,
                    base,
                    background,
                },
            );
        }
        if kept.len() != requests.len() {
            return Err(RenderError::Internal("unplanned sibling level".into()));
        }
        Ok(kept)
    }

    /// Mixes a filtered backdrop with its input by the adjustment's opacity,
    /// blend mode, pixel mask and vector coverage.
    fn mix_adjustment(
        &mut self,
        node: &RNode,
        input: &Target,
        filtered: Target,
        transform: WorldTransform,
    ) -> Result<Target> {
        let desired = input.roi;
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
                self.dispatch(&out, input, &filtered, input, p, &entries)?;
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
                self.dispatch(&out, input, &filtered, input, p, &[])?;
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
            self.dispatch(&out, input, &filtered, input, p, &entries)?;
            mixed = out;
        }
        if let Some(mask) = self.vector_coverage(node, transform, desired)? {
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
            self.dispatch(&out, input, &mixed, &mask, p, &[])?;
            mixed = out;
        }
        Ok(mixed)
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
        #[cfg(test)]
        RAW_NODE_RENDERS.set(RAW_NODE_RENDERS.get() + 1);
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
                    self.vector_q14(paths, world.fixed()?, roi, true)?
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
            RContent::Group { children, .. } => self.siblings(children, None, roi, world, depth)?,
            _ => self.zero(roi)?,
        };
        Ok(source)
    }
}

fn same_roi(a: Roi, b: Roi) -> bool {
    a.x == b.x && a.y == b.y && a.scale == b.scale && a.w == b.w && a.h == b.h
}

/// Whether `outer` covers `inner` at the same sampling scale. Version 1 and 2
/// origins are f32, so they may miss the sampling grid by a rounding step.
pub(super) fn covers(outer: Roi, inner: Roi) -> bool {
    let slack = f64::from(outer.scale) / 64.0;
    let axis = |o: f32, ow: u32, i: f32, iw: u32| {
        let scale = f64::from(outer.scale);
        f64::from(o) <= f64::from(i) + slack
            && f64::from(o) + f64::from(ow) * scale + slack >= f64::from(i) + f64::from(iw) * scale
    };
    outer.scale == inner.scale
        && axis(outer.x, outer.w, inner.x, inner.w)
        && axis(outer.y, outer.h, inner.y, inner.h)
}

/// The union of two version 1 or 2 ROIs at one scale. Their origins lie on
/// that scale's document grid (`Roi::coarsen`, whole-pixel `Roi::expand`) up
/// to f32 rounding, which the span absorbs.
fn legacy_union(a: Roi, b: Roi) -> Result<Roi> {
    debug_assert_eq!(a.scale, b.scale);
    let scale = f64::from(a.scale);
    let axis = |a0: f32, aw: u32, b0: f32, bw: u32| -> Result<(f32, u32)> {
        let low = a0.min(b0);
        let high =
            (f64::from(a0) + f64::from(aw) * scale).max(f64::from(b0) + f64::from(bw) * scale);
        let span = ((high - f64::from(low)) / scale - 1.0 / 64.0).ceil();
        if !(0.0..=f64::from(u32::MAX)).contains(&span) {
            return Err(RenderError::InvalidArgument("legacy ROI span".into()));
        }
        Ok((low, span as u32))
    };
    let (x, w) = axis(a.x, a.w, b.x, b.w)?;
    let (y, h) = axis(a.y, a.h, b.y, b.h)?;
    Ok(Roi {
        x,
        y,
        w,
        h,
        scale: a.scale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_union_covers_both_rois_on_the_grid() {
        let a = Roi {
            x: -3.0,
            y: 4.0,
            scale: 0.25,
            w: 10,
            h: 2,
        };
        let b = Roi {
            x: -1.0,
            y: 3.0,
            scale: 0.25,
            w: 20,
            h: 2,
        };
        let union = legacy_union(a, b).unwrap();
        assert_eq!((union.x, union.y, union.w, union.h), (-3.0, 3.0, 28, 6));
        assert!(covers(union, a) && covers(union, b));
        assert!(!covers(a, b) && !covers(b, a));
        // An origin one f32 step off the grid still joins without a gap.
        let off = Roi {
            x: f32::from_bits((-1.0f32).to_bits() + 1),
            ..b
        };
        let union = legacy_union(a, off).unwrap();
        assert_eq!(union.w, 28);
        assert!(covers(union, off));
    }
}
