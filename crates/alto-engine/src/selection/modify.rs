//! Bounded, tile-local morphology for the document's soft R8 selection.
use super::{MAX_SELECTION_TILES, empty, invalid};
use crate::{DocHandle, Engine, EngineError};
use alto_core::{PixelFormat, TileKey};
use alto_doc::Op;
use alto_raster::{TileData, TileMap};
use serde_json::Value;
use std::collections::BTreeSet;

const TILE: usize = 256;
const MAX_RADIUS: usize = 64;
// Counts vertical reductions or moving-sum passes, before any tile is decoded.
const MAX_MODIFY_WORK: u64 = 1 << 28;

#[derive(Clone, Copy, Debug)]
enum Modify {
    Grow,
    Shrink,
    Border,
    Smooth,
}

impl Modify {
    fn work_per_pixel(self, radius: usize) -> u64 {
        let diameter = (2 * radius + 1) as u64;
        match self {
            Self::Grow | Self::Shrink => diameter,
            Self::Border => 2 * diameter,
            Self::Smooth => 4,
        }
    }
}

pub(super) fn modify_selection(
    engine: &mut Engine,
    handle: DocHandle,
    spec: &Value,
) -> Result<Vec<Op>, EngineError> {
    let radius = spec["radius"]
        .as_u64()
        .filter(|r| (1..=MAX_RADIUS as u64).contains(r))
        .ok_or_else(|| invalid("Selection radius must be an integer 1..64"))?
        as usize;
    let operation = match spec["operation"].as_str() {
        Some("grow") => Modify::Grow,
        Some("shrink") => Modify::Shrink,
        Some("border") => Modify::Border,
        Some("smooth") => Modify::Smooth,
        _ => return Err(invalid("Unknown selection modification")),
    };
    let doc = engine.document(handle)?;
    let width = doc.meta.width as usize;
    let height = doc.meta.height as usize;
    let source = doc
        .selection
        .as_ref()
        .ok_or_else(|| invalid("There is no selection to modify"))?;
    let keys = candidate_tiles(source, width, height, operation)?;

    // A fully selected canvas is unchanged by dilation. Check tile content IDs
    // against the canonical masks (at most four sizes, including clipped edges).
    if matches!(operation, Modify::Grow) && is_full_canvas(source, &keys, width, height) {
        // The canonical check proved every in-document key is present.
        let selection = TileMap {
            format: PixelFormat::R8,
            tiles: keys
                .into_iter()
                .map(|key| (key, source.tiles[&key]))
                .collect(),
        };
        for hash in selection.tiles.values().copied().collect::<BTreeSet<_>>() {
            engine.require_tile(&hash)?;
        }
        return Ok(vec![Op::SetSelection {
            selection: Some(selection),
        }]);
    }
    let work = keys.len() as u64 * (TILE * TILE) as u64 * operation.work_per_pixel(radius);
    if work > MAX_MODIFY_WORK {
        return Err(invalid(
            "Selection modification is too large; reduce the radius or the area",
        ));
    }

    let side = TILE + 2 * radius;
    let mut window = vec![0; side * side];
    let mut output = Vec::new();
    for key in keys {
        copy_window(engine, source, key, width, height, radius, &mut window)?;
        let mut pixels = modify_window(&window, side, operation, radius);
        let w = (width - key.tx as usize * TILE).min(TILE);
        let h = (height - key.ty as usize * TILE).min(TILE);
        for row in pixels[..h * TILE].as_chunks_mut::<TILE>().0 {
            row[w..].fill(0);
        }
        pixels[h * TILE..].fill(0);
        let tile = TileData::new(PixelFormat::R8, pixels)?;
        if !tile.is_empty() {
            output.push((key, tile));
        }
    }
    let mut selection = empty();
    for (key, tile) in output {
        // put_tile is infallible and omits all-zero tiles.
        if let Some(hash) = engine.put_tile(tile) {
            selection.tiles.insert(key, hash);
        }
    }
    Ok(vec![Op::SetSelection {
        selection: Some(selection),
    }])
}

fn candidate_tiles(
    source: &TileMap,
    width: usize,
    height: usize,
    operation: Modify,
) -> Result<BTreeSet<TileKey>, EngineError> {
    let spread = i64::from(!matches!(operation, Modify::Shrink));
    let cols = width.div_ceil(TILE) as i64;
    let rows = height.div_ceil(TILE) as i64;
    let mut keys = BTreeSet::new();
    for key in source.tiles.keys() {
        let x = i64::from(key.tx);
        let y = i64::from(key.ty);
        // Input outside the document is zero, including whole tiles outside.
        if !(0..cols).contains(&x) || !(0..rows).contains(&y) {
            continue;
        }
        for ty in (y - spread).max(0)..=(y + spread).min(rows - 1) {
            for tx in (x - spread).max(0)..=(x + spread).min(cols - 1) {
                keys.insert(TileKey {
                    tx: tx as i32,
                    ty: ty as i32,
                });
                if keys.len() > MAX_SELECTION_TILES {
                    return Err(invalid("Selection/mask exceeds 4096 tiles"));
                }
            }
        }
    }
    Ok(keys)
}

fn is_full_canvas(source: &TileMap, keys: &BTreeSet<TileKey>, width: usize, height: usize) -> bool {
    let cols = width.div_ceil(TILE);
    let rows = height.div_ceil(TILE);
    if cols * rows != keys.len() {
        return false;
    }
    let mut solid = std::collections::BTreeMap::new();
    keys.iter().all(|key| {
        let w = (width - key.tx as usize * TILE).min(TILE);
        let h = (height - key.ty as usize * TILE).min(TILE);
        let expected = solid.entry((w, h)).or_insert_with(|| {
            let mut tile = vec![0; TILE * TILE];
            for row in tile[..h * TILE].as_chunks_mut::<TILE>().0 {
                row[..w].fill(255);
            }
            TileData::new(PixelFormat::R8, tile)
                .expect("R8 tile dimensions")
                .hash()
        });
        source.tiles.get(key) == Some(expected)
    })
}

fn copy_window(
    engine: &Engine,
    source: &TileMap,
    key: TileKey,
    width: usize,
    height: usize,
    radius: usize,
    window: &mut [u8],
) -> Result<(), EngineError> {
    window.fill(0);
    let side = TILE + 2 * radius;
    let x0 = key.tx * TILE as i32 - radius as i32;
    let y0 = key.ty * TILE as i32 - radius as i32;
    let x1 = (x0 + side as i32).min(width as i32);
    let y1 = (y0 + side as i32).min(height as i32);
    for ty in y0.max(0) / TILE as i32..=(y1 - 1) / TILE as i32 {
        for tx in x0.max(0) / TILE as i32..=(x1 - 1) / TILE as i32 {
            let Some(hash) = source.tiles.get(&TileKey { tx, ty }) else {
                continue;
            };
            let tile = engine.require_tile(hash)?;
            let tile_x = tx * TILE as i32;
            let tile_y = ty * TILE as i32;
            let start_x = x0.max(tile_x);
            let end_x = x1.min(tile_x + TILE as i32);
            let len = (end_x - start_x) as usize;
            for y in y0.max(tile_y)..y1.min(tile_y + TILE as i32) {
                let from = (y - tile_y) as usize * TILE + (start_x - tile_x) as usize;
                let to = (y - y0) as usize * side + (start_x - x0) as usize;
                window[to..to + len].copy_from_slice(&tile.pixels[from..from + len]);
            }
        }
    }
    Ok(())
}

fn modify_window(window: &[u8], side: usize, operation: Modify, radius: usize) -> Vec<u8> {
    let width = side - 2 * radius;
    if window.iter().all(|&v| v == window[0]) {
        let value = if matches!(operation, Modify::Border) {
            0
        } else {
            window[0]
        };
        return vec![value; width * width];
    }
    match operation {
        Modify::Grow => disk_extreme::<true>(window, side, radius),
        Modify::Shrink => disk_extreme::<false>(window, side, radius),
        Modify::Border => disk_extreme::<true>(window, side, radius)
            .into_iter()
            .zip(disk_extreme::<false>(window, side, radius))
            .map(|(outer, inner)| outer.saturating_sub(inner))
            .collect(),
        Modify::Smooth => box_average(window, side, radius),
    }
}

fn extreme<const MAX: bool>(a: u8, b: u8) -> u8 {
    if MAX { a.max(b) } else { a.min(b) }
}

fn disk_extreme<const MAX: bool>(window: &[u8], side: usize, radius: usize) -> Vec<u8> {
    let width = side - 2 * radius;
    let mut rows_by_half_width = vec![Vec::new(); radius + 1];
    for row in 0..=2 * radius {
        let dy = row.abs_diff(radius);
        let half_width = (radius * radius - dy * dy).isqrt();
        rows_by_half_width[half_width].push(row);
    }
    let mut output = vec![if MAX { 0 } else { 255 }; width * width];
    let mut horizontal = vec![0; side * width];
    let mut prefix = vec![0; side];
    let mut suffix = vec![0; side];
    for (half, rows) in rows_by_half_width
        .iter()
        .enumerate()
        .filter(|(_, rows)| !rows.is_empty())
    {
        let block = 2 * half + 1;
        for (row, out) in window
            .chunks_exact(side)
            .zip(horizontal.chunks_exact_mut(width))
        {
            // van Herk/Gil-Werman. Any interval of length `block` is a
            // suffix and a prefix of adjacent blocks, or one complete block.
            for ((input, prefix), suffix) in row
                .chunks(block)
                .zip(prefix.chunks_mut(block))
                .zip(suffix.chunks_mut(block))
            {
                let mut p = input[0];
                let mut s = input[input.len() - 1];
                for (i, &value) in input.iter().enumerate() {
                    p = extreme::<MAX>(p, value);
                    prefix[i] = p;
                }
                for (i, &value) in input.iter().enumerate().rev() {
                    s = extreme::<MAX>(s, value);
                    suffix[i] = s;
                }
            }
            for (x, value) in out.iter_mut().enumerate() {
                let start = x + radius - half;
                *value = extreme::<MAX>(suffix[start], prefix[start + 2 * half]);
            }
        }
        for &row in rows {
            let offset = row * width;
            for (value, &input) in output
                .iter_mut()
                .zip(&horizontal[offset..offset + width * width])
            {
                *value = extreme::<MAX>(*value, input);
            }
        }
    }
    output
}

fn box_average(window: &[u8], side: usize, radius: usize) -> Vec<u8> {
    let width = side - 2 * radius;
    let count = 2 * radius + 1;
    let area = (count * count) as u32;
    let mut rows = vec![0u32; side * width];
    for (row, out) in window.chunks_exact(side).zip(rows.chunks_exact_mut(width)) {
        let mut sum = row[..count].iter().map(|&v| u32::from(v)).sum::<u32>();
        out[0] = sum;
        for x in 1..width {
            sum = sum + u32::from(row[x + count - 1]) - u32::from(row[x - 1]);
            out[x] = sum;
        }
    }
    let mut output = vec![0; width * width];
    for x in 0..width {
        let mut sum = rows[..count * width]
            .iter()
            .skip(x)
            .step_by(width)
            .sum::<u32>();
        output[x] = ((sum + area / 2) / area) as u8;
        for y in 1..width {
            sum = sum + rows[(y + count - 1) * width + x] - rows[(y - 1) * width + x];
            output[y * width + x] = ((sum + area / 2) / area) as u8;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::{decode, sample};
    use alto_doc::DocumentMeta;
    use serde_json::json;

    fn apply(engine: &mut Engine, doc: DocHandle, spec: Value) {
        let ops = engine.selection_ops(doc, &spec).unwrap();
        let rev = engine.document(doc).unwrap().revision;
        engine.transact(doc, "selection", ops, rev).unwrap();
    }

    #[test]
    fn windows_match_a_direct_reference() {
        for radius in [1, 2, 3, 5, 8] {
            let side = 11 + 2 * radius;
            let input: Vec<u8> = (0..side * side)
                .map(|i| ((i * 37 + i / 5 * 71) % 256) as u8)
                .collect();
            for operation in [Modify::Grow, Modify::Shrink, Modify::Border, Modify::Smooth] {
                let output = modify_window(&input, side, operation, radius);
                for y in 0..11 {
                    for x in 0..11 {
                        let mut stats = (255u8, 0u8, 0u32, 0u32);
                        for dy in 0..=2 * radius {
                            for dx in 0..=2 * radius {
                                if matches!(operation, Modify::Smooth)
                                    || dx.abs_diff(radius).pow(2) + dy.abs_diff(radius).pow(2)
                                        <= radius * radius
                                {
                                    let value = input[(y + dy) * side + x + dx];
                                    stats.0 = stats.0.min(value);
                                    stats.1 = stats.1.max(value);
                                    stats.2 += u32::from(value);
                                    stats.3 += 1;
                                }
                            }
                        }
                        let expected = match operation {
                            Modify::Grow => stats.1,
                            Modify::Shrink => stats.0,
                            Modify::Border => stats.1 - stats.0,
                            Modify::Smooth => ((stats.2 + stats.3 / 2) / stats.3) as u8,
                        };
                        assert_eq!(
                            output[y * 11 + x],
                            expected,
                            "{operation:?} r={radius} x={x} y={y}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn maximum_radius_reaches_axis_endpoints_but_excludes_the_corners() {
        let r = MAX_RADIUS;
        let side = 2 * r + 1;
        let mut input = vec![0; side * side];
        input[0] = 255;
        input[r] = 123;
        input[(side - 1) * side + r] = 201;
        assert_eq!(modify_window(&input, side, Modify::Grow, r), [201]);
        input.fill(255);
        input[0] = 0;
        input[r] = 123;
        assert_eq!(modify_window(&input, side, Modify::Shrink, r), [123]);
    }

    #[test]
    fn grow_crosses_tiles_and_clips_partial_edges() {
        let mut engine = Engine::new();
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(257, 16))
            .unwrap();
        apply(
            &mut engine,
            doc,
            json!({"kind":"rectangle","x":255,"y":0,"width":1,"height":1}),
        );
        let before = engine.document(doc).unwrap().selection.clone();
        let base = engine.save_package(doc).unwrap();
        let ops = engine
            .selection_ops(
                doc,
                &json!({"kind":"modify","operation":"grow","radius":1,"mode":"ignored"}),
            )
            .unwrap();
        let rev = engine.document(doc).unwrap().revision;
        engine.transact(doc, "selection", ops.clone(), rev).unwrap();
        let journal = engine
            .journal_record(doc, crate::JournalKind::Transact, "selection", &ops)
            .unwrap();
        let after = engine.document(doc).unwrap().selection.clone();
        let tiles = decode(after.as_ref().unwrap(), &engine).unwrap();
        assert_eq!(tiles.len(), 2);
        assert_eq!(sample(&tiles, 255, 0), 255);
        assert_eq!(sample(&tiles, 254, 0), 255);
        assert_eq!(sample(&tiles, 256, 0), 255);
        assert_eq!(sample(&tiles, 255, 1), 255);
        assert_eq!(sample(&tiles, 256, 1), 0);
        assert_eq!(sample(&tiles, 255, -1), 0);
        assert_eq!(sample(&tiles, 257, 0), 0);
        engine.undo(doc).unwrap();
        assert_eq!(engine.document(doc).unwrap().selection, before);
        engine.redo(doc).unwrap();
        assert_eq!(engine.document(doc).unwrap().selection, after);
        let mut recovered = Engine::new();
        let (reopened, warnings) = recovered.open_package(&base).unwrap();
        assert!(warnings.is_empty());
        recovered.replay_record(reopened, &journal).unwrap();
        assert_eq!(recovered.document(reopened).unwrap().selection, after);
    }

    #[test]
    fn grow_ignores_input_beyond_the_last_partial_tile() {
        let mut engine = Engine::new();
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(257, 16))
            .unwrap();
        let mut pixels = vec![0; TILE * TILE];
        pixels[1] = 255; // x=257 is outside the document, on the same tile as x=256.
        let hash = engine
            .put_tile(TileData::new(PixelFormat::R8, pixels).unwrap())
            .unwrap();
        let mut selection = empty();
        selection.tiles.insert(TileKey { tx: 1, ty: 0 }, hash);
        engine
            .transact(
                doc,
                "selection",
                vec![Op::SetSelection {
                    selection: Some(selection),
                }],
                0,
            )
            .unwrap();
        apply(
            &mut engine,
            doc,
            json!({"kind":"modify","operation":"grow","radius":1}),
        );
        assert!(
            engine
                .document(doc)
                .unwrap()
                .selection
                .as_ref()
                .unwrap()
                .tiles
                .is_empty()
        );
    }

    #[test]
    fn invalid_requests_and_empty_results_keep_the_selection_contract() {
        let mut engine = Engine::new();
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(16, 16))
            .unwrap();
        for spec in [
            json!({"kind":"modify","operation":"grow","radius":1}),
            json!({"kind":"modify","operation":"grow","radius":0}),
            json!({"kind":"modify","operation":"grow","radius":65}),
            json!({"kind":"modify","operation":"grow","radius":1.5}),
            json!({"kind":"modify","operation":"grow"}),
            json!({"kind":"modify","operation":"unknown","radius":1}),
        ] {
            let error = engine.selection_ops(doc, &spec).unwrap_err();
            assert!(error.to_string().starts_with("INVALID_ARGUMENT:"));
            assert!(engine.document(doc).unwrap().selection.is_none());
        }
        apply(
            &mut engine,
            doc,
            json!({"kind":"rectangle","x":8,"y":8,"width":1,"height":1}),
        );
        apply(
            &mut engine,
            doc,
            json!({"kind":"modify","operation":"shrink","radius":1}),
        );
        for operation in ["grow", "shrink", "border", "smooth"] {
            assert!(
                engine
                    .document(doc)
                    .unwrap()
                    .selection
                    .as_ref()
                    .unwrap()
                    .tiles
                    .is_empty()
            );
            apply(
                &mut engine,
                doc,
                json!({"kind":"modify","operation":operation,"radius":2}),
            );
        }
    }

    #[test]
    fn sparse_candidates_use_neighbours_and_reject_the_tile_limit() {
        let mut map = empty();
        let h = alto_core::ContentHash::of(b"not read during candidate enumeration");
        map.tiles.insert(TileKey { tx: 0, ty: 0 }, h);
        map.tiles.insert(TileKey { tx: 199, ty: 199 }, h);
        map.tiles.insert(TileKey { tx: -1, ty: 100 }, h);
        assert_eq!(
            candidate_tiles(&map, 200 * TILE, 200 * TILE, Modify::Grow)
                .unwrap()
                .len(),
            8
        );
        assert_eq!(
            candidate_tiles(&map, 200 * TILE, 200 * TILE, Modify::Shrink)
                .unwrap()
                .len(),
            2
        );
        for i in 0..500 {
            map.tiles.insert(
                TileKey {
                    tx: 3 * (i % 25) + 1,
                    ty: 3 * (i / 25) + 1,
                },
                h,
            );
        }
        assert!(candidate_tiles(&map, 200 * TILE, 200 * TILE, Modify::Grow).is_err());
        let mut engine = Engine::new();
        let side = 200 * TILE as u32;
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(side, side))
            .unwrap();
        let hash = engine
            .put_tile(TileData::new(PixelFormat::R8, vec![255; TILE * TILE]).unwrap())
            .unwrap();
        for value in map.tiles.values_mut() {
            *value = hash;
        }
        engine
            .transact(
                doc,
                "selection",
                vec![Op::SetSelection {
                    selection: Some(map),
                }],
                0,
            )
            .unwrap();
        let before = engine.snapshot(doc).unwrap();
        let stored = engine.tiles.len();
        let error = engine
            .selection_ops(doc, &json!({"kind":"modify","operation":"grow","radius":1}))
            .unwrap_err();
        assert!(error.to_string().contains("exceeds 4096 tiles"));
        assert_eq!(engine.snapshot(doc).unwrap(), before);
        assert_eq!(engine.tiles.len(), stored);
    }

    #[test]
    fn full_canvas_grow_reuses_the_complete_mask() {
        let mut engine = Engine::new();
        let side = 64 * TILE;
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(side as u32, side as u32))
            .unwrap();
        let hash = engine
            .put_tile(TileData::new(PixelFormat::R8, vec![255; TILE * TILE]).unwrap())
            .unwrap();
        let mut mask = empty();
        for ty in 0..64 {
            for tx in 0..64 {
                mask.tiles.insert(TileKey { tx, ty }, hash);
            }
        }
        let mut with_outside = mask.clone();
        with_outside.tiles.insert(TileKey { tx: -1, ty: 0 }, hash);
        engine
            .transact(
                doc,
                "full selection",
                vec![Op::SetSelection {
                    selection: Some(with_outside),
                }],
                0,
            )
            .unwrap();
        let result = engine
            .selection_ops(
                doc,
                &json!({"kind":"modify","operation":"grow","radius":64}),
            )
            .unwrap();
        assert!(
            matches!(&result[..], [Op::SetSelection { selection: Some(selection) }] if selection == &mask)
        );
    }

    #[test]
    fn work_limit_counts_both_passes_of_border() {
        let mut engine = Engine::new();
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(32 * 256, 256))
            .unwrap();
        apply(
            &mut engine,
            doc,
            json!({"kind":"rectangle","x":0,"y":80,"width":32*256,"height":16}),
        );
        let before = engine.snapshot(doc).unwrap();
        for operation in ["grow", "border"] {
            let error = engine
                .selection_ops(
                    doc,
                    &json!({"kind":"modify","operation":operation,"radius":64}),
                )
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Selection modification is too large")
            );
            assert_eq!(before, engine.snapshot(doc).unwrap());
        }
        assert!(16 * 65536 * Modify::Grow.work_per_pixel(64) <= MAX_MODIFY_WORK);
        assert!(16 * 65536 * Modify::Border.work_per_pixel(64) > MAX_MODIFY_WORK);
        assert_eq!(
            Modify::Smooth.work_per_pixel(1),
            Modify::Smooth.work_per_pixel(64)
        );
    }
}
