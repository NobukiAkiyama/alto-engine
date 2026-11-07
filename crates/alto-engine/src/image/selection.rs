//! Selection masks for image edits, and moving them with the image geometry.
use super::*;

pub(super) fn selection(
    engine: &Engine,
    map: Option<&TileMap>,
    m: alto_core::FixedAffine,
    x: i32,
    y: i32,
) -> Result<u8, EngineError> {
    let Some(map) = map else { return Ok(255) };
    // Pixel centers mapped with v3's composed Q32/Q16 transform, then quantized to Q8.
    let cx = 2 * i128::from(x) + 1;
    let cy = 2 * i128::from(y) + 1;
    let qx = rounded(
        i128::from(m.m[0]) * cx + i128::from(m.m[2]) * cy + i128::from(m.t[0]) * (1 << 17)
            - (1 << 32),
        1 << 25,
    );
    let qy = rounded(
        i128::from(m.m[1]) * cx + i128::from(m.m[3]) * cy + i128::from(m.t[1]) * (1 << 17)
            - (1 << 32),
        1 << 25,
    );
    if qx.div_euclid(256) < i64::from(i32::MIN)
        || qx.div_euclid(256) >= i64::from(i32::MAX)
        || qy.div_euclid(256) < i64::from(i32::MIN)
        || qy.div_euclid(256) >= i64::from(i32::MAX)
    {
        return Err(err("INVALID_ARGUMENT", "selection mapping out of range"));
    }
    let x = qx.div_euclid(256) as i32;
    let y = qy.div_euclid(256) as i32;
    let fx = qx.rem_euclid(256) as u64;
    let fy = qy.rem_euclid(256) as u64;
    let mut n = 0u64;
    for (dx, dy, w) in [
        (0, 0, (256 - fx) * (256 - fy)),
        (1, 0, fx * (256 - fy)),
        (0, 1, (256 - fx) * fy),
        (1, 1, fx * fy),
    ] {
        n += u64::from(read(engine, map, x + dx, y + dy)?[3]) * w;
    }
    Ok(((n + 32768) / 65536) as u8)
}

/// How a geometry edit moves node-local pixels.
#[derive(Clone, Copy)]
pub(super) enum Remap {
    Rotate(u8),
    Flip(FlipAxis),
    Scale,
}
/// Selection tiles a geometry edit may produce for one effect.
pub(super) const MAX_REMAPPED_SELECTION_TILES: usize = 4096;
/// An effect's R8 selection carried through a geometry edit from `old` to `new` bounds.
/// Flips and quarter turns move each pixel exactly as `image.flip`/`image.rotate` move
/// image pixels. A resize samples the selection bilinearly at the new pixel centers,
/// with the same Q8 source coordinates as `image.resize`, and zero outside its tiles.
pub(super) fn remap_selection(
    engine: &Engine,
    map: &TileMap,
    remap: Remap,
    old: ImageBounds,
    new: ImageBounds,
) -> Result<BTreeMap<TileKey, Vec<u8>>, EngineError> {
    let limit = || {
        err(
            "RESOURCE_LIMIT",
            "the moved effect selection is out of range",
        )
    };
    let mut out: BTreeMap<TileKey, Vec<u8>> = BTreeMap::new();
    let (ox, oy) = (i64::from(old.x), i64::from(old.y));
    let (w, h) = (i64::from(old.width), i64::from(old.height));
    if let Remap::Scale = remap {
        let Some(tx0) = map.tiles.keys().map(|k| k.tx).min() else {
            return Ok(out);
        };
        let ty0 = map.tiles.keys().map(|k| k.ty).min().unwrap_or(0);
        let tx1 = map.tiles.keys().map(|k| k.tx).max().unwrap_or(0);
        let ty1 = map.tiles.keys().map(|k| k.ty).max().unwrap_or(0);
        let (nw, nh) = (i64::from(new.width), i64::from(new.height));
        // Destination pixels whose bilinear footprint can reach a source tile.
        let lo = |t: i32, o: i64, from: i64, to: i64| {
            o + ((i64::from(t) * 256 - 1 - o) * to).div_euclid(from) - 1
        };
        let hi = |t: i32, o: i64, from: i64, to: i64| {
            o + ((i64::from(t) * 256 + 256 + 1 - o) * to + from - 1).div_euclid(from) + 1
        };
        let (dx0, dx1) = (lo(tx0, ox, w, nw), hi(tx1, ox, w, nw));
        let (dy0, dy1) = (lo(ty0, oy, h, nh), hi(ty1, oy, h, nh));
        let tiles = ((dx1 - 1).div_euclid(256) - dx0.div_euclid(256) + 1)
            * ((dy1 - 1).div_euclid(256) - dy0.div_euclid(256) + 1);
        if tiles > MAX_REMAPPED_SELECTION_TILES as i64 {
            return Err(err(
                "RESOURCE_LIMIT",
                "the resized effect selection exceeds 4096 tiles",
            ));
        }
        if dx0 < i64::from(i32::MIN)
            || dy0 < i64::from(i32::MIN)
            || dx1 > i64::from(i32::MAX)
            || dy1 > i64::from(i32::MAX)
        {
            return Err(limit());
        }
        let source = |origin: i64, d: i64, from: i64, to: i64| {
            rounded(
                i128::from(origin) * 256 * 2 * i128::from(to)
                    + (2 * i128::from(d - origin) + 1) * i128::from(from) * 256
                    - 256 * i128::from(to),
                2 * i128::from(to),
            )
        };
        let mut reader = Reader::new(engine, map);
        for v in dy0..dy1 {
            let qy = source(oy, v, h, nh);
            let (iy, fy) = (qy.div_euclid(256), qy.rem_euclid(256) as u64);
            for u in dx0..dx1 {
                let qx = source(ox, u, w, nw);
                let (ix, fx) = (qx.div_euclid(256), qx.rem_euclid(256) as u64);
                let mut acc = 0u64;
                for (dx, dy, weight) in [
                    (0, 0, (256 - fx) * (256 - fy)),
                    (1, 0, fx * (256 - fy)),
                    (0, 1, (256 - fx) * fy),
                    (1, 1, fx * fy),
                ] {
                    if weight == 0 {
                        continue;
                    }
                    let (Ok(x), Ok(y)) = (i32::try_from(ix + dx), i32::try_from(iy + dy)) else {
                        continue;
                    };
                    acc += u64::from(reader.get(x, y)?[3]) * weight;
                }
                let value = ((acc + 32768) / 65536) as u8;
                if value != 0 {
                    let key = TileKey {
                        tx: u.div_euclid(256) as i32,
                        ty: v.div_euclid(256) as i32,
                    };
                    out.entry(key).or_insert_with(|| vec![0; 65536])
                        [v.rem_euclid(256) as usize * 256 + u.rem_euclid(256) as usize] = value;
                }
            }
        }
        return Ok(out);
    }
    for (key, hash) in &map.tiles {
        let tile = engine.require_tile(hash)?;
        if tile.format != PixelFormat::R8 {
            return Err(err("CORRUPT_DATA", "effect selection requires R8"));
        }
        for (i, value) in tile.pixels.iter().enumerate() {
            if *value == 0 {
                continue;
            }
            let x = i64::from(key.tx) * 256 + (i % 256) as i64;
            let y = i64::from(key.ty) * 256 + (i / 256) as i64;
            let (nx, ny) = match remap {
                Remap::Flip(FlipAxis::Horizontal) => (2 * ox + w - 1 - x, y),
                Remap::Flip(FlipAxis::Vertical) => (x, 2 * oy + h - 1 - y),
                Remap::Rotate(1) => (ox + h - 1 - (y - oy), oy + (x - ox)),
                Remap::Rotate(2) => (2 * ox + w - 1 - x, 2 * oy + h - 1 - y),
                Remap::Rotate(_) => (ox + (y - oy), oy + w - 1 - (x - ox)),
                Remap::Scale => unreachable!("handled above"),
            };
            let (Ok(nx), Ok(ny)) = (i32::try_from(nx), i32::try_from(ny)) else {
                return Err(limit());
            };
            out.entry(TileKey {
                tx: nx.div_euclid(256),
                ty: ny.div_euclid(256),
            })
            .or_insert_with(|| vec![0; 65536])
                [ny.rem_euclid(256) as usize * 256 + nx.rem_euclid(256) as usize] = *value;
        }
        if out.len() > MAX_REMAPPED_SELECTION_TILES {
            return Err(err(
                "RESOURCE_LIMIT",
                "the moved effect selection exceeds 4096 tiles",
            ));
        }
    }
    Ok(out)
}
