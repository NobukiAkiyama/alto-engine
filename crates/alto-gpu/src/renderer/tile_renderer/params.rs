//! Stage uniforms, integer ROIs and bind-group entries.
use super::*;

impl Context<'_> {
    pub(super) fn params(mode: u32, world: [f64; 6]) -> Params {
        let inv = inverse(world).unwrap_or([0.0; 6]);
        Params {
            inv0: [inv[0] as f32, inv[1] as f32, inv[2] as f32, inv[3] as f32],
            inv1: [inv[4] as f32, inv[5] as f32, 0.0, 0.0],
            info: [mode, 0, 0, 0],
            ..Params::zeroed()
        }
    }
    /// The uniform of a version 3 stage (`effects-q14.wgsl`) writing `out`.
    /// `inputs` are the ROIs of the inputs the mode reads (`a`, `b`); each texel
    /// is placed from the integer pixel index (`input_map`). `lookup` is the atlas
    /// read of mode 18 (the raster) or modes 21 to 23 (the mask), the Q16
    /// `mask_sampler`. `settings` are Q14 integers. All of it is in `Config.grid`
    /// (`mask_grid`); the float slots stay zero. An input region without an
    /// integer definition is refused (DQ3-4), never placed in f32.
    pub(super) fn q14_params(
        mode: u32,
        out: Roi,
        inputs: [Option<Roi>; 2],
        settings: [i32; 4],
        lookup: Option<&super::MaskSampler>,
    ) -> Result<Params> {
        let map = |input: Option<Roi>| match input {
            None => Ok([0; 4]),
            Some(input) => super::input_map(out.region(), [out.w, out.h], input.region())
                .ok_or_else(|| {
                    RenderError::GpuUnsupported(
                        "v3 stage input region has no integer texel map".into(),
                    )
                }),
        };
        Ok(Params {
            curve: super::mask_grid(settings, map(inputs[0])?, map(inputs[1])?, lookup),
            info: [mode, 0, 0, crate::sample::SAMPLE_INTEGER],
            ..Params::zeroed()
        })
    }
    /// A version 3 mask stage (modes 21 to 23) writing `out` from inputs `a` and
    /// `b`, with the mask, if any, read at `out` through `mask_sampler`: the Q16
    /// lookup only. A mask without an integer definition is refused (DQ3-4),
    /// never sampled in f32. See `q14_params`.
    pub(super) fn mask_params(
        mode: u32,
        transform: Option<WorldTransform>,
        out: Roi,
        a: Roi,
        b: Roi,
        settings: [i32; 4],
    ) -> Result<Params> {
        let sampler = transform
            .map(|t| {
                let fixed = t.fixed()?;
                super::fixed_mask_sampler(&fixed, Self::integer_roi(out)?).ok_or_else(|| {
                    RenderError::GpuUnsupported(
                        "v3 mask transform or level has no integer lookup".into(),
                    )
                })
            })
            .transpose()?;
        Self::q14_params(mode, out, [Some(a), Some(b)], settings, sampler.as_ref())
    }
    pub(super) fn blur_params(
        out: Roi,
        input: Roi,
        radius: u32,
        weights: &[u32],
        axis: u32,
    ) -> Result<Params> {
        let output = RoiQ::from_roi(out)?;
        let source = RoiQ::from_roi(input)?;
        if output.level != source.level {
            return Err(RenderError::InvalidArgument(
                "v3 blur input level mismatch".into(),
            ));
        }
        let dx = i32::try_from(output.ox - source.ox)
            .map_err(|_| RenderError::InvalidArgument("v3 blur x offset".into()))?;
        let dy = i32::try_from(output.oy - source.oy)
            .map_err(|_| RenderError::InvalidArgument("v3 blur y offset".into()))?;
        let mut params = Params {
            curve: super::mask_grid(
                [i32::try_from(radius).unwrap(), dx, dy, 0],
                [0; 4],
                [0; 4],
                None,
            ),
            info: [24, 0, 0, axis],
            ..Params::zeroed()
        };
        let words: &mut [u32] = bytemuck::cast_slice_mut(&mut params.curve);
        words[4..37].fill(0);
        for (index, &weight) in weights.iter().enumerate() {
            let word = 4 + index / 2;
            words[word] |= weight << ((index % 2) * 16);
        }
        Ok(params)
    }
    pub(super) fn integer_roi(roi: Roi) -> Result<[i32; 3]> {
        let invalid = || {
            RenderError::GpuUnsupported(format!(
                "v3 ROI has no integer origin or scale: ({}, {}, {})",
                roi.x, roi.y, roi.scale
            ))
        };
        let grid = RoiQ::from_roi(roi)?;
        if grid.level >= 0 {
            Ok([
                super::exact_i32(roi.x).ok_or_else(invalid)?,
                super::exact_i32(roi.y).ok_or_else(invalid)?,
                super::exact_i32(roi.scale).ok_or_else(invalid)?,
            ])
        } else {
            let denominator = 1_i32
                .checked_shl(u32::try_from(-grid.level).map_err(|_| invalid())?)
                .ok_or_else(invalid)?;
            Ok([
                i32::try_from(grid.ox).map_err(|_| invalid())?,
                i32::try_from(grid.oy).map_err(|_| invalid())?,
                -denominator,
            ])
        }
    }
    /// Version 3: the sampler of `tiles` drawn with document-to-pixel `transform`
    /// at `roi`, and the tiles it can read, uploaded. The tiles are selected with
    /// the sampler's own Q16 inverse and Q8 pixel centers (`v3_reach`), not with
    /// the f64 inverse: the two differ by the quantization error, which grows
    /// with the distance from the origin. A source that no pixel reaches uploads
    /// nothing. Refused (`GpuUnsupported`, naming `what`) when the lookup has no
    /// Q16 value or pixels that reach a tile have no exact shader value (DQ3-4).
    pub(super) fn lookup_entries(
        &mut self,
        tiles: &[crate::RenderTile],
        transform: WorldTransform,
        roi: Roi,
        layer: Option<NodeId>,
        what: &str,
    ) -> Result<(Vec<Entry>, super::MaskSampler)> {
        let unsupported =
            || RenderError::GpuUnsupported(format!("v3 {what} ROI has no integer tile range"));
        let sampler = super::fixed_mask_sampler(&transform.fixed()?, Self::integer_roi(roi)?)
            .ok_or_else(unsupported)?;
        // The source's tiles, the stroke overrides of `layer` included.
        let keys = tiles.iter().map(|t| t.key).chain(
            self.overrides
                .keys()
                .filter(|(l, _)| Some(*l) == layer)
                .map(|(_, k)| *k),
        );
        let [x, y, scale] = sampler.origin.map(i64::from);
        let span = [0, 0, i64::from(roi.w), i64::from(roi.h)];
        let inverse = super::sampler_inverse(&sampler);
        let reach = if scale > 0 {
            super::v3_reach(&inverse, [x, y], scale, span, 0, keys)
        } else {
            let step = i128::from(256 / -scale);
            super::v3_reach_q8(
                &inverse,
                [i128::from(x) * step, i128::from(y) * step],
                step,
                span,
                0,
                keys,
            )
        }
        .map_err(|()| unsupported())?;
        // Nothing reached: no tile is read (every pixel is transparent or byte 0).
        let Some(range) = reach else {
            return Ok((vec![], sampler));
        };
        let entries = self.upload_entries(tiles, layer, |key| {
            (range[0]..=range[2]).contains(&key.tx) && (range[1]..=range[3]).contains(&key.ty)
        })?;
        Ok((entries, sampler))
    }
    pub(super) fn mask_entries(
        &mut self,
        mask: &RMask,
        world: WorldTransform,
        roi: Roi,
        layer: Option<NodeId>,
    ) -> Result<(Vec<Entry>, WorldTransform)> {
        let transform = if mask.linked {
            world.concat(mask.transform)?
        } else {
            WorldTransform::root(self.scene.render_version).concat(mask.transform)?
        };
        if !self.resources.exact {
            return Ok((
                self.entries(&mask.tiles, transform.legacy()?, roi, layer)?,
                transform,
            ));
        }
        // Version 3 reads the mask through `mask_sampler` (modes 21 to 23).
        let (entries, _) = self.lookup_entries(&mask.tiles, transform, roi, layer, "mask")?;
        Ok((entries, transform))
    }
}
