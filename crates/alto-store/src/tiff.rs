//! Classic little-endian TIFF headers for straight-alpha RGBA8 strips.
use serde::Deserialize;

pub const MAX_TIFF_BYTES: u64 = u32::MAX as u64;
pub const ROWS_PER_STRIP: u32 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TiffCompression {
    None,
    Deflate,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TiffStrip {
    pub rows: u32,
    pub bytes: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum TiffError {
    #[error("INVALID_ARGUMENT: {0}")]
    InvalidArgument(&'static str),
    #[error("RESOURCE_LIMIT: classic TIFF exceeds the 32-bit file size limit")]
    ResourceLimit,
}

/// Returns the metadata and strip tables. Append the strip payloads in order.
pub fn rgba8_header(
    width: u32,
    height: u32,
    compression: TiffCompression,
    strips: &[TiffStrip],
) -> Result<Vec<u8>, TiffError> {
    use TiffError::{InvalidArgument, ResourceLimit};
    if width == 0 || height == 0 || strips.len() != height.div_ceil(ROWS_PER_STRIP) as usize {
        return Err(InvalidArgument("invalid TIFF dimensions or strip count"));
    }

    const TAGS: u16 = 12;
    const SHORT: u16 = 3;
    const LONG: u16 = 4;
    const BITS_OFFSET: u32 = 8 + 2 + TAGS as u32 * 12 + 4;
    const FORMAT_OFFSET: u32 = BITS_OFFSET + 8;
    const TABLE_OFFSET: u32 = FORMAT_OFFSET + 8;
    let count = strips.len() as u32;
    let multiple = count > 1;
    let header_len = TABLE_OFFSET + if multiple { count * 8 } else { 0 };
    let mut file_len = u64::from(header_len);
    let mut remaining = height;
    for strip in strips {
        let rows = remaining.min(ROWS_PER_STRIP);
        if strip.rows != rows || strip.bytes == 0 {
            return Err(InvalidArgument("invalid TIFF strip rows or byte count"));
        }
        if compression == TiffCompression::None
            && u64::from(strip.bytes) != u64::from(width) * u64::from(rows) * 4
        {
            return Err(InvalidArgument(
                "uncompressed TIFF strip has the wrong byte count",
            ));
        }
        remaining -= rows;
        file_len += u64::from(strip.bytes);
        if file_len > MAX_TIFF_BYTES {
            return Err(ResourceLimit);
        }
    }

    let mut header = Vec::with_capacity(header_len as usize);
    header.extend_from_slice(b"II\x2a\x00\x08\x00\x00\x00");
    header.extend_from_slice(&TAGS.to_le_bytes());
    let mut tag = |id: u16, kind: u16, length: u32, value: u32| {
        header.extend_from_slice(&id.to_le_bytes());
        header.extend_from_slice(&kind.to_le_bytes());
        header.extend_from_slice(&length.to_le_bytes());
        header.extend_from_slice(&value.to_le_bytes());
    };
    tag(256, LONG, 1, width);
    tag(257, LONG, 1, height);
    tag(258, SHORT, 4, BITS_OFFSET);
    tag(
        259,
        SHORT,
        1,
        if compression == TiffCompression::None {
            1
        } else {
            8
        },
    );
    tag(262, SHORT, 1, 2);
    tag(
        273,
        LONG,
        count,
        if multiple { TABLE_OFFSET } else { header_len },
    );
    tag(277, SHORT, 1, 4);
    tag(278, LONG, 1, ROWS_PER_STRIP);
    tag(
        279,
        LONG,
        count,
        if multiple {
            TABLE_OFFSET + count * 4
        } else {
            strips[0].bytes
        },
    );
    tag(284, SHORT, 1, 1);
    tag(338, SHORT, 1, 2);
    tag(339, SHORT, 4, FORMAT_OFFSET);
    header.extend_from_slice(&0u32.to_le_bytes());
    for value in [8u16; 4].into_iter().chain([1; 4]) {
        header.extend_from_slice(&value.to_le_bytes());
    }
    if multiple {
        let mut offset = header_len;
        for strip in strips {
            header.extend_from_slice(&offset.to_le_bytes());
            // The complete file length was checked above.
            offset += strip.bytes;
        }
        for strip in strips {
            header.extend_from_slice(&strip.bytes.to_le_bytes());
        }
    }
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiff_header_fields_are_valid() {
        let file = rgba8_header(
            2,
            257,
            TiffCompression::None,
            &[
                TiffStrip {
                    rows: 256,
                    bytes: 2048,
                },
                TiffStrip { rows: 1, bytes: 8 },
            ],
        )
        .unwrap();
        assert_eq!(&file[..8], b"II\x2a\x00\x08\x00\x00\x00");
        let short = |at: usize| u16::from_le_bytes(file[at..at + 2].try_into().unwrap());
        let long = |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().unwrap());
        let tags: Vec<_> = (0..usize::from(short(8)))
            .map(|n| {
                let at = 10 + n * 12;
                (short(at), short(at + 2), long(at + 4), long(at + 8))
            })
            .collect();
        assert_eq!(
            tags.iter().map(|tag| tag.0).collect::<Vec<_>>(),
            [256, 257, 258, 259, 262, 273, 277, 278, 279, 284, 338, 339]
        );
        assert_eq!(tags[2].1, 3);
        assert_eq!(tags[2].2, 4);
        assert_eq!(short(tags[2].3 as usize), 8);
        assert_eq!(tags[10], (338, 3, 1, 2));
        let offsets = tags[5].3 as usize;
        let counts = tags[8].3 as usize;
        assert_eq!(
            [long(offsets), long(offsets + 4)],
            [file.len() as u32, file.len() as u32 + 2048]
        );
        assert_eq!([long(counts), long(counts + 4)], [2048, 8]);
    }

    #[test]
    fn tiff_strip_rows_must_cover_the_height() {
        assert!(matches!(
            rgba8_header(
                2,
                257,
                TiffCompression::None,
                &[TiffStrip {
                    rows: 256,
                    bytes: 2048
                }]
            ),
            Err(TiffError::InvalidArgument(_))
        ));
        assert!(matches!(
            rgba8_header(
                2,
                1,
                TiffCompression::None,
                &[TiffStrip { rows: 1, bytes: 7 }]
            ),
            Err(TiffError::InvalidArgument(_))
        ));
    }

    #[test]
    fn tiff_over_4_gib_is_refused() {
        assert!(matches!(
            rgba8_header(
                1,
                1,
                TiffCompression::Deflate,
                &[TiffStrip {
                    rows: 1,
                    bytes: u32::MAX
                }]
            ),
            Err(TiffError::ResourceLimit)
        ));
    }
}
