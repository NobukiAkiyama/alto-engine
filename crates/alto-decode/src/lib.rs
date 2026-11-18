//! The decode Worker's module: LZ4 and the content-address check, nothing else, so
//! each Worker loads a small module instead of the whole engine.
use alto_core::{ContentHash, PixelFormat, TILE};
use alto_raster::TileData;
use wasm_bindgen::prelude::*;

/// Decodes requests framed as `Runtime.takeDecodeRequests` frames them and returns
/// results framed as `Runtime.provideDecoded` reads them. An empty result means
/// the request frame was truncated; the worker must fall back to inline decoding.
#[wasm_bindgen(js_name = decodeBatch)]
pub fn decode_batch(framed: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let Some(count) = framed.first_chunk::<4>().map(|c| u32::from_le_bytes(*c)) else {
        return Vec::new();
    };
    out.extend(count.to_le_bytes());
    let mut p = 4usize;
    for _ in 0..count {
        // Slice from p first: on wasm32 a usize sum near u32::MAX would wrap.
        let Some(header) = framed.get(p..).and_then(|rest| rest.get(..38)) else {
            return Vec::new();
        };
        let hash = ContentHash(header[..32].try_into().expect("32 bytes"));
        let format = if header[32] == 1 {
            PixelFormat::R8
        } else {
            PixelFormat::Rgba8
        };
        let check = header[33] != 0;
        let len = u32::from_le_bytes(header[34..38].try_into().expect("4 bytes")) as usize;
        let Some(compressed) = framed[p + 38..].get(..len) else {
            return Vec::new();
        };
        p += 38 + len;
        out.extend(hash.0);
        match decode(compressed, format, check.then_some(hash)) {
            Some(pixels) => {
                out.push(u8::from(check));
                out.extend((pixels.len() as u32).to_le_bytes());
                out.extend(pixels);
            }
            None => {
                out.push(2);
                out.extend(0u32.to_le_bytes());
            }
        }
    }
    out
}

fn decode(
    compressed: &[u8],
    format: PixelFormat,
    expected: Option<ContentHash>,
) -> Option<Box<[u8]>> {
    // Check the size prefix before LZ4 allocates. Tile sizes are fixed; an
    // untrusted prefix must never request an arbitrary allocation.
    let size = u32::from_le_bytes(*compressed.first_chunk::<4>()?) as usize;
    let expected_size = (TILE * TILE) as usize * format.bytes_per_pixel();
    if size != expected_size {
        return None;
    }
    let pixels = lz4_flex::decompress_size_prepended(compressed).ok()?;
    let tile = TileData::new(format, pixels).ok()?;
    if expected.is_some_and(|h| tile.hash() != h) {
        return None;
    }
    Some(tile.pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_lz4_prefix_is_reported_as_damage() {
        let framed = request(&[(
            ContentHash([0; 32]),
            PixelFormat::R8,
            false,
            vec![255, 255, 255, 255, 0],
        )]);
        let result = decode_batch(&framed);
        assert_eq!(result.len(), 41);
        assert_eq!(result[36], 2);
        assert_eq!(&result[37..41], &[0; 4]);
    }
    #[test]
    fn length_past_the_frame_stops_the_batch() {
        assert!(decode_batch(&[1, 0, 0]).is_empty());
        let mut framed = request(&[(ContentHash([0; 32]), PixelFormat::R8, false, vec![])]);
        framed[0..4].copy_from_slice(&2u32.to_le_bytes());
        framed[38..42].copy_from_slice(&u32::MAX.to_le_bytes());
        framed.extend([0; 38]);
        let result = decode_batch(&framed);
        // Reject the entire batch so the worker can retry all inputs inline.
        assert!(result.is_empty());
    }
    #[test]
    fn empty_and_frame_ending_items_keep_the_batch_aligned() {
        // Item 1 has length 0; item 2's body ends exactly at the frame's end;
        // the count claims a third item that is not there.
        let mut framed = request(&[
            (ContentHash([1; 32]), PixelFormat::R8, false, vec![]),
            (ContentHash([2; 32]), PixelFormat::R8, false, vec![1, 2, 3]),
        ]);
        framed[0..4].copy_from_slice(&3u32.to_le_bytes());
        let result = decode_batch(&framed);
        assert!(result.is_empty());
        framed[0..4].copy_from_slice(&2u32.to_le_bytes());
        let result = decode_batch(&framed);
        assert_eq!(result.len(), 4 + 37 * 2);
        assert_eq!(u32::from_le_bytes(result[..4].try_into().unwrap()), 2);
        assert_eq!(result[36], 2);
        assert_eq!(result[73], 2);
    }
    fn request(items: &[(ContentHash, PixelFormat, bool, Vec<u8>)]) -> Vec<u8> {
        let mut out = (items.len() as u32).to_le_bytes().to_vec();
        for (hash, format, check, compressed) in items {
            out.extend(hash.0);
            out.push(if *format == PixelFormat::R8 { 1 } else { 4 });
            out.push(u8::from(*check));
            out.extend((compressed.len() as u32).to_le_bytes());
            out.extend(compressed);
        }
        out
    }
    #[test]
    fn decodes_checks_and_reports_damage() {
        let tile = TileData::new(PixelFormat::Rgba8, vec![9; 262144]).unwrap();
        let good = lz4_flex::compress_prepend_size(&tile.pixels);
        let wrong = ContentHash::of(b"another tile");
        let out = decode_batch(&request(&[
            (tile.hash(), PixelFormat::Rgba8, true, good.clone()),
            (wrong, PixelFormat::Rgba8, true, good.clone()),
            (wrong, PixelFormat::Rgba8, false, good),
            (tile.hash(), PixelFormat::Rgba8, false, vec![1, 2, 3]),
        ]));
        assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()), 4);
        let mut p = 4;
        let mut statuses = vec![];
        for _ in 0..4 {
            let status = out[p + 32];
            let len = u32::from_le_bytes(out[p + 33..p + 37].try_into().unwrap()) as usize;
            if status != 2 {
                assert_eq!(&out[p + 37..p + 37 + len], &tile.pixels[..]);
            }
            statuses.push(status);
            p += 37 + len;
        }
        // Checked and right; checked and wrong; not asked to check; not LZ4.
        assert_eq!(statuses, vec![1, 2, 0, 2]);
    }
}
