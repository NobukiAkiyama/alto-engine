use crate::DocError;
use serde::{Deserialize, Serialize};

pub const MAX_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
impl Default for ImageBounds {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 256,
            height: 256,
        }
    }
}
impl ImageBounds {
    pub fn validate(&self) -> Result<(), DocError> {
        if self.width == 0
            || self.height == 0
            || self.width > 65536
            || self.height > 65536
            || u64::from(self.width) * u64::from(self.height) > MAX_IMAGE_PIXELS
            || i64::from(self.x) + i64::from(self.width) > i64::from(i32::MAX / 256 * 256)
            || i64::from(self.y) + i64::from(self.height) > i64::from(i32::MAX / 256 * 256)
        {
            return Err(DocError::Invalid(
                "image bounds exceed dimension or area limits".into(),
            ));
        }
        Ok(())
    }
    pub fn right(self) -> i32 {
        (i64::from(self.x) + i64::from(self.width)) as i32
    }
    pub fn bottom(self) -> i32 {
        (i64::from(self.y) + i64::from(self.height)) as i32
    }
    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
    pub fn union(self, b: Self) -> Result<Self, DocError> {
        let x = self.x.min(b.x);
        let y = self.y.min(b.y);
        let width = u32::try_from(i64::from(self.right().max(b.right())) - i64::from(x))
            .map_err(|_| DocError::Invalid("bounds overflow".into()))?;
        let height = u32::try_from(i64::from(self.bottom().max(b.bottom())) - i64::from(y))
            .map_err(|_| DocError::Invalid("bounds overflow".into()))?;
        let out = Self {
            x,
            y,
            width,
            height,
        };
        out.validate()?;
        Ok(out)
    }
    pub fn tile(key: alto_core::TileKey) -> Result<Self, DocError> {
        let x = key
            .tx
            .checked_mul(256)
            .ok_or_else(|| DocError::Invalid("tile coordinate overflow".into()))?;
        let y = key
            .ty
            .checked_mul(256)
            .ok_or_else(|| DocError::Invalid("tile coordinate overflow".into()))?;
        let out = Self {
            x,
            y,
            width: 256,
            height: 256,
        };
        out.validate()?;
        Ok(out)
    }
}
