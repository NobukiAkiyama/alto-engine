use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

pub const TILE: u32 = 256;
/// Isolated groups the compositor can have open at once. The composite shader
/// keeps 32 slots and rejects another push once this many isolates are already open.
pub const MAX_ISOLATED_GROUPS: u32 = 31;
/// Structural walk limit. This stops a cycle; it is not the compositor depth.
/// Drawn isolated groups stop at `MAX_ISOLATED_GROUPS`.
pub const MAX_TREE_DEPTH: u32 = 128;
/// Float bilinear (`mix`) and f32 source coordinates. Documents saved before the
/// integer filter omit `render_version` and load as this version, so their pixels
/// stay on that path.
pub const RENDER_VERSION_F32: u32 = 1;
/// Integer 4-tap bilinear. Inverse coefficients are Q16, source positions are Q8,
/// and the filter accumulates bytes with round-half-up. New documents use this.
pub const RENDER_VERSION: u32 = 2;
/// Integer color-adjust (Q14). Not the default: new documents stay on
/// [`RENDER_VERSION`]. A document selects this explicitly. Version 1 and 2
/// keep the float mode-3 formula.
pub const RENDER_VERSION_EXACT: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub uuid::Uuid);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetId(pub uuid::Uuid);

impl NodeId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}
impl AssetId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}
impl Default for NodeId {
    fn default() -> Self {
        Self::new()
    }
}
impl Default for AssetId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
    pub fn of_tagged(tag: u8, bytes: &[u8]) -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(&[tag]);
        hash.update(bytes);
        Self(*hash.finalize().as_bytes())
    }
    pub fn hex(self) -> String {
        blake3::Hash::from(self.0).to_hex().to_string()
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}
impl FromStr for ContentHash {
    type Err = blake3::HexError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(*blake3::Hash::from_hex(s)?.as_bytes()))
    }
}
impl Serialize for ContentHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}
impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Affine2(pub [f64; 6]);
impl Default for Affine2 {
    fn default() -> Self {
        Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
    }
}
impl Affine2 {
    pub fn concat(self, b: Self) -> Self {
        let a = self.0;
        let b = b.0;
        Self([
            a[0] * b[0] + a[2] * b[1],
            a[1] * b[0] + a[3] * b[1],
            a[0] * b[2] + a[2] * b[3],
            a[1] * b[2] + a[3] * b[3],
            a[0] * b[4] + a[2] * b[5] + a[4],
            a[1] * b[4] + a[3] * b[5] + a[5],
        ])
    }
    pub fn inverse(self) -> Option<Self> {
        let m = self.0;
        let det = m[0] * m[3] - m[1] * m[2];
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        Some(Self([
            m[3] / det,
            -m[1] / det,
            -m[2] / det,
            m[0] / det,
            (m[2] * m[5] - m[3] * m[4]) / det,
            (m[1] * m[4] - m[0] * m[5]) / det,
        ]))
    }
    pub fn is_finite(&self) -> bool {
        self.0.iter().all(|v| v.is_finite())
    }
    pub fn apply(self, p: [f64; 2]) -> [f64; 2] {
        let m = self.0;
        [
            m[0] * p[0] + m[2] * p[1] + m[4],
            m[1] * p[0] + m[3] * p[1] + m[5],
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    pub tx: i32,
    pub ty: i32,
}

impl fmt::Display for TileKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{},{}", self.tx, self.ty)
    }
}
impl FromStr for TileKey {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (x, y) = s
            .split_once(',')
            .ok_or_else(|| "Invalid tile key".to_string())?;
        Ok(Self {
            tx: x.parse().map_err(|_| "Invalid tile X".to_string())?,
            ty: y.parse().map_err(|_| "Invalid tile Y".to_string())?,
        })
    }
}
impl Serialize for TileKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for TileKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    Rgba8,
    R8,
}
impl PixelFormat {
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgba8 => 4,
            Self::R8 => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn generated_node_ids_do_not_collide_in_sample() {
        let ids: BTreeSet<_> = (0..10_000).map(|_| NodeId::new()).collect();
        assert_eq!(ids.len(), 10_000);
    }

    #[test]
    fn tile_key_json_is_stable_and_supports_negative_coordinates() {
        let key = TileKey { tx: -4, ty: 9 };
        assert_eq!(serde_json::to_string(&key).unwrap(), "\"-4,9\"");
        assert_eq!(serde_json::from_str::<TileKey>("\"-4,9\"").unwrap(), key);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorSpace {
    Srgb,
    DisplayP3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BitDepth {
    U8,
    U16,
    F16,
}

#[cfg(test)]
mod tagged_hash_tests {
    #[test]
    fn streamed_tile_hash_matches_canonical_bytes() {
        for size in [0, 1, 63, 64, 1023, 1024, 65536, 262144] {
            for tag in [1, 4] {
                let bytes: Vec<_> = (0..size).map(|i| (i % 251) as u8).collect();
                let mut canonical = vec![tag];
                canonical.extend_from_slice(&bytes);
                assert_eq!(
                    super::ContentHash::of(&canonical),
                    super::ContentHash::of_tagged(tag, &bytes)
                );
            }
        }
    }
}
