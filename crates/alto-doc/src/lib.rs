use alto_core::{
    Affine2, AssetId, BitDepth, ColorSpace, ContentHash, MAX_ISOLATED_GROUPS, MAX_TREE_DEPTH,
    NodeId, PixelFormat, Rect, TILE, TileKey,
};
use alto_raster::TileMap;
use alto_text::TextContent;
use alto_vector::{ClipPath, StyledPath, VectorMask, validate_paths};

mod image_bounds;
pub use image_bounds::{ImageBounds, MAX_IMAGE_PIXELS};
pub mod digest;
pub mod version;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub use version::{Commit, DiffItem, IntentSummary, diff, restore_ops};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentMeta {
    pub width: u32,
    pub height: u32,
    pub color: ColorSpace,
    pub depth: BitDepth,
    /// File format 5. Earlier formats held render versions 1 and 2, which are
    /// not supported.
    pub format_version: u32,
    /// Which compositor samples this document: always
    /// [`alto_core::RENDER_VERSION_EXACT`]. Kept so a later version can be told apart.
    pub render_version: u32,
}

impl DocumentMeta {
    pub fn srgb_u8(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            color: ColorSpace::Srgb,
            depth: BitDepth::U8,
            // The live value matches the snapshot so a journal digest survives
            // save and replay.
            format_version: 5,
            render_version: alto_core::RENDER_VERSION_EXACT,
        }
    }
    fn validate(&self) -> Result<(), DocError> {
        if self.width == 0 || self.height == 0 {
            return Err(DocError::Invalid(
                "Canvas dimensions must be positive".into(),
            ));
        }
        if self.format_version != 5 {
            return Err(DocError::Unsupported("Unsupported document format".into()));
        }
        if self.render_version != alto_core::RENDER_VERSION_EXACT {
            return Err(DocError::Unsupported("Unsupported render version".into()));
        }
        if self.color != ColorSpace::Srgb || self.depth != BitDepth::U8 {
            return Err(DocError::Invalid(
                "Only sRGB 8-bit documents are supported".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
/// Messages start with the code the SDK reports (`CODE: detail`).
pub enum DocError {
    #[error("UNSUPPORTED: {0}")]
    Unsupported(String),
    #[error("INVALID_ARGUMENT: invalid operation: {0}")]
    Invalid(String),
    #[error("NOT_FOUND: node not found: {0:?}")]
    NotFound(NodeId),
    #[error("CONFLICT: revision conflict: expected {expected}, actual {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error("INVALID_ARGUMENT: serialization error: {0}")]
    Serialization(String),
}

mod document;
mod editor;
mod effect;
mod node;
mod op;
pub use document::*;
pub use editor::*;
pub use effect::*;
pub use node::*;
pub use op::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_rejects_unimplemented_color_and_depth() {
        let mut meta = DocumentMeta::srgb_u8(8, 8);
        meta.color = ColorSpace::DisplayP3;
        assert!(Document::new(meta).is_err());
        let mut meta = DocumentMeta::srgb_u8(8, 8);
        meta.depth = BitDepth::F16;
        assert!(Document::new(meta).is_err());
    }
}
