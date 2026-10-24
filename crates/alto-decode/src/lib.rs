//! The decode Worker's module: LZ4 and the content-address check, nothing else, so
//! each Worker loads a small module instead of the whole engine.
use alto_core::{ContentHash, PixelFormat};
use alto_raster::TileData;
use wasm_bindgen::prelude::*;

/// Decodes requests framed as `Runtime.takeDecodeRequests` frames them and returns
/// results framed as `Runtime.provideDecoded` reads them.
#[wasm_bindgen(js_name = decodeBatch)]
pub fn decode_batch(framed: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let Some(count) = framed.first_chunk::<4>().map(|c| u32::from_le_bytes(*c)) else {
        return 0u32.to_le_bytes().to_vec();
    };
    out.extend(count.to_le_bytes());
    let mut p = 4usize;
    for _ in 0..count {
        let Some(header) = framed.get(p..p + 38) else {
            break;
        };
        let hash = ContentHash(header[..32].try_into().expect("32 bytes"));
        let format = if header[32] == 1 {
            PixelFormat::R8
        } else {
            PixelFormat::Rgba8
        };
        let check = header[33] != 0;
        let len = u32::from_le_bytes(header[34..38].try_into().expect("4 bytes")) as usize;
        let compressed = framed.get(p + 38..p + 38 + len).unwrap_or_default();
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
