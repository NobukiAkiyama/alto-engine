//! The render version 3 text bake (`ALTB`): a text node's glyphs as integers, the
//! only input of a v3 text render (doc/design/2026-10-28-v3-integer-text.md
//! "焼き込み"). The engine writes it at edit time; the renderer reads it with
//! [`TextBake::decode`], which checks every bound before any value is used.

use crate::ContentHash;

const MAGIC: &[u8; 4] = b"ALTB";
const VERSION: u16 = 1;
/// The most commands one outline holds (the vector schema's `CHUNK`).
pub const MAX_OUTLINE_COMMANDS: u32 = 4000;
/// Outline coordinates are font units in Q6, at most this in magnitude.
pub const MAX_OUTLINE_COORD: i32 = 1 << 22;
/// Placement translations are node pixels in Q30, at most this in magnitude.
pub const MAX_TRANSLATION: i64 = 1 << 60;
/// The outline font index of the missing-glyph placeholder.
pub const PLACEHOLDER_FONT: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BakeCommand {
    Move([i32; 2]),
    Line([i32; 2]),
    Quad([i32; 2], [i32; 2]),
    Cubic([i32; 2], [i32; 2], [i32; 2]),
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BakeFont {
    /// The font asset's blake3.
    pub hash: ContentHash,
    pub face_index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BakeOutline {
    /// Index into `fonts`, or [`PLACEHOLDER_FONT`].
    pub font: u32,
    pub glyph: u16,
    /// 1..=16384.
    pub upem: u16,
    /// Font units (y up) in Q6.
    pub commands: Vec<BakeCommand>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BakeGlyph {
    /// Index into `outlines`.
    pub outline: u32,
    /// Font units to node pixels `[a, b, c, d]` (column major, as `Affine2`), Q24.
    pub m: [i32; 4],
    /// Node pixels, Q30.
    pub t: [i64; 2],
    /// Straight RGBA, Q14.
    pub color: [u16; 4],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextBake {
    /// The baking build's `BAKER_VERSION`; diagnostic only, never read by a render.
    pub baker: u16,
    pub fonts: Vec<BakeFont>,
    pub outlines: Vec<BakeOutline>,
    /// In drawing order.
    pub glyphs: Vec<BakeGlyph>,
}

/// Why a bake did not decode (`TEXT_BAKE_INVALID`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BakeError(pub &'static str);

impl TextBake {
    /// The little-endian `ALTB` bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&self.baker.to_le_bytes());
        out.extend_from_slice(&(self.fonts.len() as u32).to_le_bytes());
        for font in &self.fonts {
            out.extend_from_slice(&font.hash.0);
            out.extend_from_slice(&font.face_index.to_le_bytes());
        }
        out.extend_from_slice(&(self.outlines.len() as u32).to_le_bytes());
        for outline in &self.outlines {
            out.extend_from_slice(&outline.font.to_le_bytes());
            out.extend_from_slice(&outline.glyph.to_le_bytes());
            out.extend(outline_record(outline));
        }
        out.extend_from_slice(&(self.glyphs.len() as u32).to_le_bytes());
        for glyph in &self.glyphs {
            out.extend_from_slice(&glyph.outline.to_le_bytes());
            for v in glyph.m {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for v in glyph.t {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for v in glyph.color {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }

    /// Decodes and checks every bound (magic, version, `upem`, command counts,
    /// coordinate and translation ranges, colours, indices, exact end).
    pub fn decode(bytes: &[u8]) -> Result<Self, BakeError> {
        let mut r = Reader { bytes, at: 0 };
        if r.take(4)? != MAGIC {
            return Err(BakeError("magic"));
        }
        if r.u16()? != VERSION {
            return Err(BakeError("version"));
        }
        let baker = r.u16()?;
        let font_count = r.count(36)?;
        let mut fonts = Vec::with_capacity(font_count);
        for _ in 0..font_count {
            let hash = ContentHash(r.take(32)?.try_into().map_err(|_| BakeError("hash"))?);
            fonts.push(BakeFont {
                hash,
                face_index: r.u32()?,
            });
        }
        let outline_count = r.count(12)?;
        let mut outlines = Vec::with_capacity(outline_count);
        for _ in 0..outline_count {
            let font = r.u32()?;
            if font != PLACEHOLDER_FONT && font as usize >= fonts.len() {
                return Err(BakeError("outline font index"));
            }
            let glyph = r.u16()?;
            let upem = r.u16()?;
            if !(1..=16384).contains(&upem) {
                return Err(BakeError("upem"));
            }
            let n = r.u32()?;
            if n > MAX_OUTLINE_COMMANDS {
                return Err(BakeError("command count"));
            }
            let mut commands = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let tag = r.take(1)?[0];
                commands.push(match tag {
                    0 => BakeCommand::Move(r.point()?),
                    1 => BakeCommand::Line(r.point()?),
                    2 => BakeCommand::Quad(r.point()?, r.point()?),
                    3 => BakeCommand::Cubic(r.point()?, r.point()?, r.point()?),
                    4 => BakeCommand::Close,
                    _ => return Err(BakeError("command tag")),
                });
            }
            outlines.push(BakeOutline {
                font,
                glyph,
                upem,
                commands,
            });
        }
        let glyph_count = r.count(44)?;
        let mut glyphs = Vec::with_capacity(glyph_count);
        for _ in 0..glyph_count {
            let outline = r.u32()?;
            if outline as usize >= outlines.len() {
                return Err(BakeError("glyph outline index"));
            }
            let m = [r.i32()?, r.i32()?, r.i32()?, r.i32()?];
            let t = [r.i64()?, r.i64()?];
            if t.iter().any(|v| v.unsigned_abs() > MAX_TRANSLATION as u64) {
                return Err(BakeError("translation"));
            }
            let color = [r.u16()?, r.u16()?, r.u16()?, r.u16()?];
            if color.iter().any(|&c| c > 16384) {
                return Err(BakeError("colour"));
            }
            glyphs.push(BakeGlyph {
                outline,
                m,
                t,
                color,
            });
        }
        if r.at != bytes.len() {
            return Err(BakeError("trailing bytes"));
        }
        Ok(Self {
            baker,
            fonts,
            outlines,
            glyphs,
        })
    }
}

/// An outline's identity bytes for the v3 atlas key: `upem` through the last
/// command, without `font` and `glyph` (memo "輪郭と鍵").
pub fn outline_record(outline: &BakeOutline) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&outline.upem.to_le_bytes());
    out.extend_from_slice(&(outline.commands.len() as u32).to_le_bytes());
    for command in &outline.commands {
        let (tag, points) = match *command {
            BakeCommand::Move(p) => (0u8, vec![p]),
            BakeCommand::Line(p) => (1, vec![p]),
            BakeCommand::Quad(c, p) => (2, vec![c, p]),
            BakeCommand::Cubic(a, b, p) => (3, vec![a, b, p]),
            BakeCommand::Close => (4, vec![]),
        };
        out.push(tag);
        for p in points {
            out.extend_from_slice(&p[0].to_le_bytes());
            out.extend_from_slice(&p[1].to_le_bytes());
        }
    }
    out
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], BakeError> {
        let end = self.at.checked_add(n).ok_or(BakeError("length"))?;
        let slice = self.bytes.get(self.at..end).ok_or(BakeError("truncated"))?;
        self.at = end;
        Ok(slice)
    }
    fn u16(&mut self) -> Result<u16, BakeError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, BakeError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, BakeError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn point(&mut self) -> Result<[i32; 2], BakeError> {
        let p = [self.i32()?, self.i32()?];
        if p.iter()
            .any(|v| v.unsigned_abs() > MAX_OUTLINE_COORD as u32)
        {
            return Err(BakeError("outline coordinate"));
        }
        Ok(p)
    }
    fn i64(&mut self) -> Result<i64, BakeError> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// A count whose records take at least `min_record` bytes each: refused
    /// before any allocation when the rest of the blob cannot hold them.
    fn count(&mut self, min_record: usize) -> Result<usize, BakeError> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_record) > self.bytes.len() - self.at {
            return Err(BakeError("count"));
        }
        Ok(n)
    }
}
