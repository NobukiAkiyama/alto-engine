//! Text for Alto: font assets, shaping (rustybuzz), horizontal and Japanese vertical
//! layout, glyph outlines for the vector renderer, and selection geometry.
pub mod font;
pub mod layout;
pub mod model;
pub mod orient;

pub use font::{FontData, FontEmbedding, FontError, FontInfo, FontSet, inspect};
pub use layout::{ClusterBox, GlyphRef, LineBox, PlacedGlyph, TextLayout, layout};
pub use model::*;
pub mod outline;
pub mod select;
pub use outline::{OutlineCache, RawCommand, glyph_paths, glyph_runs, raw_outline};
pub use select::{Caret, Motion, caret, hit_test, navigate, selection_rects};
