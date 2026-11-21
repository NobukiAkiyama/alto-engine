use alto_core::FixedAffine;
use alto_engine::selection::PathCoverageError;
use alto_gpu::raster_q8::{self, CURVE_LIMIT, Prepared, RasterError};
use alto_vector::{FillRule, PathData};

pub(super) fn rasterize_path(
    path: &PathData,
    rule: FillRule,
    windows: &[[i64; 4]],
) -> Result<Vec<Vec<u8>>, PathCoverageError> {
    let map_error = |error| match error {
        RasterError::CurveRange => PathCoverageError::TooComplex,
        RasterError::Range => PathCoverageError::OutOfRange,
    };
    let commands =
        raster_q8::map_path(path, FixedAffine::IDENTITY, 0).ok_or(PathCoverageError::OutOfRange)?;
    let prepared = Prepared::new(&commands, CURVE_LIMIT).map_err(map_error)?;
    let rule = match rule {
        FillRule::NonZero => raster_q8::FillRule::NonZero,
        FillRule::EvenOdd => raster_q8::FillRule::EvenOdd,
    };
    windows
        .iter()
        .map(|&window| {
            let width = window[2] as usize;
            let mut pixels = vec![0; width * window[3] as usize];
            if let Some(([x, y], w, _, coverage)) =
                raster_q8::rasterize_crop_prepared(&prepared, rule, window).map_err(map_error)?
            {
                let first = y as usize * width + x as usize;
                let w = w as usize;
                for (row, source) in coverage.chunks_exact(w).enumerate() {
                    let offset = first + row * width;
                    pixels[offset..offset + w].copy_from_slice(source);
                }
            }
            Ok(pixels)
        })
        .collect()
}
