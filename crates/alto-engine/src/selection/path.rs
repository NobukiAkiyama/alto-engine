use super::*;
use alto_vector::{FillRule, PathCommand, PathData};
use serde::Deserialize;

/// Rasterizes document-pixel [x, y, width, height] windows to row-major R8.
pub type PathCoverage<'a> =
    &'a dyn Fn(&PathData, FillRule, &[[i64; 4]]) -> Result<Vec<Vec<u8>>, PathCoverageError>;

#[derive(Clone, Copy, Debug)]
pub enum PathCoverageError {
    TooComplex,
    OutOfRange,
}

// Bound intermediate coverage as well as repeated raster and feather work.
const TILE: usize = alto_core::TILE as usize;
const MAX_PATH_SELECTION_WORK: u64 = 1 << 27;
const MAX_PATH_WINDOW_EDGE_WORK: usize = 1 << 22;

impl Engine {
    pub fn selection_ops_with_path(
        &mut self,
        handle: DocHandle,
        spec: &Value,
        rasterize: PathCoverage<'_>,
    ) -> Result<Vec<Op>, EngineError> {
        if spec["kind"] != "path" {
            return self.selection_ops(handle, spec);
        }
        let data = PathData::deserialize(&spec["path"])
            .map_err(|_| invalid("Path selection requires valid path data"))?;
        data.validate()
            .map_err(|_| invalid("Path selection requires valid path data"))?;
        let rule = match spec.get("fill_rule") {
            None => FillRule::NonZero,
            Some(value) if value == "non_zero" => FillRule::NonZero,
            Some(value) if value == "even_odd" => FillRule::EvenOdd,
            _ => return Err(invalid("Unknown path selection fill rule")),
        };
        let feather = spec
            .get("feather")
            .map_or(Some(0.0), Value::as_f64)
            .filter(|n| (0.0..=64.0).contains(n))
            .ok_or_else(|| invalid("Selection feather must be 0..64"))?;
        let radius = if feather > 0.0 {
            ((feather / 2.0).round() as usize).max(1)
        } else {
            0
        };
        let mode = selection_mode(spec)?;
        let doc = self.document(handle)?;
        let width = i64::from(doc.meta.width);
        let height = i64::from(doc.meta.height);
        let r = radius as f64;
        let [x0, y0, x1, y1] = control_bounds(&data).unwrap_or([0.0; 4]);
        let keys = if x0 < x1
            && y0 < y1
            && x0 < width as f64
            && y0 < height as f64
            && x1 > 0.0
            && y1 > 0.0
        {
            let [x0, y0, x1, y1] = checked_bounds([
                (x0 - r).max(0.0),
                (y0 - r).max(0.0),
                (x1 + r).min(width as f64),
                (y1 + r).min(height as f64),
            ])?;
            (y0..y1)
                .flat_map(|ty| (x0..x1).map(move |tx| TileKey { tx, ty }))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        let previous = doc.selection.as_ref().filter(|_| mode != "replace");
        let mut candidates: BTreeSet<_> = keys.iter().copied().collect();
        if let Some(mask) = previous {
            candidates.extend(mask.tiles.keys().copied().filter(|key| {
                let [x, y] = tile_origin(*key);
                x >= 0 && y >= 0 && x < width && y < height
            }));
        }
        if candidates.len() > 4096 {
            return Err(invalid("Selection/mask exceeds 4096 tiles"));
        }

        let side = TILE + 2 * radius;
        let curves = data
            .commands
            .iter()
            .filter(|c| matches!(c, PathCommand::Quad { .. } | PathCommand::Cubic { .. }))
            .count();
        // Even-odd records crossings on 16 sub-rows per pixel. Bound the
        // crossing buffers for each individual window as well as total work.
        let rows_per_pixel = if rule == FillRule::EvenOdd { 16 } else { 2 };
        let edge_work = side * rows_per_pixel * (data.commands.len() + 2 * curves);
        let raster_work = side * side * if radius == 0 { 1 } else { 4 };
        let work = candidates.len() as u64 * (TILE * TILE) as u64
            + keys.len() as u64 * (raster_work + edge_work + curves * 4096) as u64;
        if !keys.is_empty() && edge_work > MAX_PATH_WINDOW_EDGE_WORK {
            return Err(invalid("Path is too complex; simplify the path"));
        }
        if work > MAX_PATH_SELECTION_WORK {
            return Err(invalid(
                "Path selection is too large; reduce the path or the area",
            ));
        }
        let windows: Vec<_> = keys
            .iter()
            .map(|key| {
                let [x, y] = tile_origin(*key);
                [
                    x - radius as i64,
                    y - radius as i64,
                    side as i64,
                    side as i64,
                ]
            })
            .collect();
        let coverages = if windows.is_empty() {
            Vec::new()
        } else {
            rasterize(&data, rule, &windows).map_err(|error| match error {
                PathCoverageError::TooComplex => invalid("Path is too complex"),
                PathCoverageError::OutOfRange => EngineError::Operation {
                    code: "UNSUPPORTED",
                    message: "Path is outside the renderer's coordinate range".into(),
                },
            })?
        };
        if coverages.len() != windows.len() || coverages.iter().any(|v| v.len() != side * side) {
            return Err(EngineError::Operation {
                code: "INTERNAL",
                message: "Invalid rasterized path coverage".into(),
            });
        }

        // Load prior tiles before writing so a read error leaves the tile store alone.
        let old = previous
            .map(|mask| decode(mask, self))
            .transpose()?
            .unwrap_or_default();
        let mut coverage = BTreeMap::new();
        for ((key, [x, y, ..]), mut pixels) in keys.into_iter().zip(windows).zip(coverages) {
            if radius > 0 {
                clip(&mut pixels, side, x, y, width, height);
                pixels = modify::box_average(&pixels, side, radius);
            }
            coverage.insert(key, pixels);
        }
        let mut map = empty();
        for key in candidates {
            let mut pixels = coverage
                .remove(&key)
                .unwrap_or_else(|| vec![0; TILE * TILE]);
            if mode != "replace" {
                let prior = old.get(&key).map(|tile| &tile.pixels[..]);
                for (i, value) in pixels.iter_mut().enumerate() {
                    let prior = f64::from(prior.map_or(0, |p| p[i])) / 255.0;
                    let amount = f64::from(*value) / 255.0;
                    *value = (combine_selection(mode, prior, amount) * 255.0).round() as u8;
                }
            }
            let [x, y] = tile_origin(key);
            clip(&mut pixels, TILE, x, y, width, height);
            if let Some(hash) =
                self.put_tile(TileData::new(PixelFormat::R8, pixels).expect("fixed R8 size"))
            {
                map.tiles.insert(key, hash);
            }
        }
        Ok(vec![Op::SetSelection {
            selection: Some(map),
        }])
    }
}

fn tile_origin(key: TileKey) -> [i64; 2] {
    [
        i64::from(key.tx) * TILE as i64,
        i64::from(key.ty) * TILE as i64,
    ]
}

fn clip(pixels: &mut [u8], side: usize, x: i64, y: i64, width: i64, height: i64) {
    let left = (-x).clamp(0, side as i64) as usize;
    let right = (width - x).clamp(0, side as i64) as usize;
    for (dy, row) in pixels.chunks_exact_mut(side).enumerate() {
        if (0..height).contains(&(y + dy as i64)) {
            row[..left].fill(0);
            row[right..].fill(0);
        } else {
            row.fill(0);
        }
    }
}

// Control points also bound the piecewise linear approximation used for fill.
// Move-only subpaths contribute nothing.
fn control_bounds(data: &PathData) -> Option<[f64; 4]> {
    let mut current = [0.0; 2];
    let mut points = Vec::new();
    for command in &data.commands {
        let (end, controls): ([f64; 2], &[[f64; 2]]) = match *command {
            PathCommand::Move { x, y } => {
                current = [x, y];
                continue;
            }
            PathCommand::Line { x, y } => ([x, y], &[]),
            PathCommand::Quad { cx, cy, x, y } => ([x, y], &[[cx, cy]]),
            PathCommand::Cubic {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => ([x, y], &[[c1x, c1y], [c2x, c2y]]),
            PathCommand::Close => continue,
        };
        points.extend_from_slice(&[current, end]);
        points.extend_from_slice(controls);
        current = end;
    }
    (!points.is_empty()).then(|| bounds_points(&points))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rectangle([left, top, right, bottom]: [i32; 4]) -> PathData {
        PathData::from_svg(&format!("M{left} {top} H{right} V{bottom} H{left} Z")).unwrap()
    }

    fn rasterize_rectangle(
        data: &PathData,
        _: FillRule,
        windows: &[[i64; 4]],
    ) -> Result<Vec<Vec<u8>>, PathCoverageError> {
        let [left, top, right, bottom] = data.bounds().map(|value| value as i64);
        Ok(windows
            .iter()
            .map(|&[x, y, w, h]| {
                (0..h)
                    .flat_map(|dy| {
                        (0..w).map(move |dx| {
                            u8::MAX
                                * u8::from(
                                    (left..right).contains(&(x + dx))
                                        && (top..bottom).contains(&(y + dy)),
                                )
                        })
                    })
                    .collect()
            })
            .collect())
    }

    fn result(ops: Vec<Op>) -> TileMap {
        let [
            Op::SetSelection {
                selection: Some(map),
            },
        ]: [Op; 1] = ops.try_into().expect("one selection op")
        else {
            panic!("missing mask");
        };
        map
    }

    #[test]
    fn rectangle_path_matches_existing_combinations_and_is_one_undo_step() {
        let mut engine = Engine::new();
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(512, 256))
            .unwrap();
        let ops = engine
            .selection_ops(
                doc,
                &json!({
                    "kind": "rectangle", "x": 235, "y": 20, "width": 29, "height": 50, "feather": 6
                }),
            )
            .unwrap();
        let before = result(ops.clone());
        engine.transact(doc, "prior", ops, 0).unwrap();
        let path = rectangle([252, 24, 270, 80]);
        for mode in ["replace", "add", "subtract", "intersect"] {
            let ops = engine
                .selection_ops_with_path(
                    doc,
                    &json!({
                        "kind": "path", "path": path, "mode": mode
                    }),
                    &rasterize_rectangle,
                )
                .unwrap();
            let rect = engine.selection_ops(doc, &json!({
                "kind": "rectangle", "x": 252, "y": 24, "width": 18, "height": 56, "mode": mode
            })).unwrap();
            assert_eq!(result(ops.clone()), result(rect), "{mode}");
            engine
                .transact(doc, "path", ops, engine.document(doc).unwrap().revision)
                .unwrap();
            engine.undo(doc).unwrap();
            assert_eq!(
                engine.document(doc).unwrap().selection.as_ref(),
                Some(&before)
            );
        }
    }

    #[test]
    fn feather_crosses_tile_edges_and_clips_before_and_after_averaging() {
        let mut engine = Engine::new();
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(258, 20))
            .unwrap();
        let ops = engine
            .selection_ops_with_path(
                doc,
                &json!({
                    "kind": "path", "path": rectangle([243, -10, 255, 15]), "feather": 3
                }),
                &|path, rule, windows| {
                    assert_eq!(windows, &[[-2, -2, 260, 260], [254, -2, 260, 260]]);
                    rasterize_rectangle(path, rule, windows)
                },
            )
            .unwrap();
        let coverage = decode(&result(ops), &engine).unwrap();
        assert_eq!(sample(&coverage, 250, 6), 255);
        assert_eq!(
            sample(&coverage, 250, 0),
            153,
            "outside is zero before feather"
        );
        assert_eq!(
            sample(&coverage, 256, 6),
            51,
            "rounded radius crosses the tile boundary"
        );
        assert_eq!(sample(&coverage, 257, 6), 0);
        let ops = engine
            .selection_ops_with_path(
                doc,
                &json!({
                    "kind": "path", "path": rectangle([245, 3, 400, 40]), "feather": 4
                }),
                &rasterize_rectangle,
            )
            .unwrap();
        let coverage = decode(&result(ops), &engine).unwrap();
        assert_eq!(
            sample(&coverage, 257, 19),
            92,
            "both document edges clip the input"
        );
        assert_eq!(
            sample(&coverage, 258, 19),
            0,
            "partial right tile clips output"
        );
        assert_eq!(
            sample(&coverage, 257, 20),
            0,
            "partial bottom tile clips output"
        );
    }

    #[test]
    fn only_path_tiles_are_rasterized_for_sparse_combinations() {
        let mut engine = Engine::new();
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(32000, 32000))
            .unwrap();
        let prior_key = TileKey { tx: 120, ty: 120 };
        let hash = engine
            .put_tile(TileData::new(PixelFormat::R8, vec![255; TILE * TILE]).unwrap())
            .unwrap();
        let mut prior = empty();
        prior.tiles.insert(prior_key, hash);
        engine
            .transact(
                doc,
                "prior",
                vec![Op::SetSelection {
                    selection: Some(prior),
                }],
                0,
            )
            .unwrap();
        let ops = engine
            .selection_ops_with_path(
                doc,
                &json!({
                    "kind": "path", "path": rectangle([5, 5, 25, 25]), "mode": "add"
                }),
                &|path, rule, windows| {
                    assert_eq!(windows, &[[0, 0, 256, 256]]);
                    rasterize_rectangle(path, rule, windows)
                },
            )
            .unwrap();
        let map = result(ops);
        assert_eq!(map.tiles.len(), 2);
        assert_eq!(map.tiles[&prior_key], hash);
    }

    #[test]
    fn empty_paths_produce_an_empty_mask_without_rasterizing() {
        let mut engine = Engine::new();
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(512, 256))
            .unwrap();
        for path in [
            PathData::default(),
            PathData::from_svg("M1 1 M500 200").unwrap(),
            rectangle([-15, 2, -1, 20]),
        ] {
            let ops = engine
                .selection_ops_with_path(
                    doc,
                    &json!({
                        "kind": "path", "path": path, "feather": 64
                    }),
                    &|_, _, _| panic!("empty coverage need not rasterize"),
                )
                .unwrap();
            assert!(result(ops).tiles.is_empty());
        }
    }

    #[test]
    fn invalid_paths_and_work_limits_fail_before_rasterizing() {
        let mut engine = Engine::new();
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(32000, 32000))
            .unwrap();
        let before = engine.snapshot(doc).unwrap();
        let stored = engine.tiles.len();
        let mut complex = rectangle([0, 0, 32 * 256, 12 * 256]);
        complex
            .commands
            .resize(4096, PathCommand::Move { x: 0.0, y: 0.0 });
        let mut too_many = complex.clone();
        too_many.commands.push(PathCommand::Move { x: 0.0, y: 0.0 });
        for (path, message) in [
            (complex, "Path selection is too large"),
            (rectangle([0, 0, 32000, 32000]), "exceeds 4096 tiles"),
            (too_many, "requires valid path data"),
            (
                PathData {
                    commands: vec![PathCommand::Line { x: 0.0, y: 0.0 }],
                },
                "requires valid path data",
            ),
            (
                PathData {
                    commands: vec![PathCommand::Move {
                        x: 1048577.0,
                        y: 0.0,
                    }],
                },
                "requires valid path data",
            ),
        ] {
            let error = engine
                .selection_ops_with_path(
                    doc,
                    &json!({
                        "kind": "path", "path": path
                    }),
                    &|_, _, _| panic!("limits must precede rasterization"),
                )
                .unwrap_err();
            assert!(error.to_string().starts_with("INVALID_ARGUMENT:"));
            assert!(error.to_string().contains(message), "{error}");
        }
        let mut dense = rectangle([1, 1, 5, 5]);
        dense.commands = (0..220)
            .flat_map(|_| dense.commands.iter().cloned())
            .collect();
        let error = engine
            .selection_ops_with_path(
                doc,
                &json!({"kind": "path", "path": dense, "fill_rule": "even_odd"}),
                &|_, _, _| panic!("window complexity must be checked before rasterization"),
            )
            .unwrap_err();
        assert!(error.to_string().contains("Path is too complex"));

        let spec = json!({"kind": "path", "path": rectangle([1, 1, 5, 5])});
        for (failure, code) in [
            (PathCoverageError::TooComplex, "INVALID_ARGUMENT:"),
            (PathCoverageError::OutOfRange, "UNSUPPORTED:"),
        ] {
            let error = engine
                .selection_ops_with_path(doc, &spec, &|_, _, _| Err(failure))
                .unwrap_err();
            assert!(error.to_string().starts_with(code));
        }
        assert_eq!(engine.snapshot(doc).unwrap(), before);
        assert_eq!(engine.tiles.len(), stored);
    }
}
