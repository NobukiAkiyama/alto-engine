pub use alto_scene::*;
pub use error::RenderError;

mod error;
pub mod glyph_atlas;
pub mod renderer;

#[cfg(test)]
mod blend_ref;
#[cfg(test)]
mod cpu_composite;

/// Blend math lives in one shader. Composite and the effect pass both prepend it.
pub fn shader_with_blend(body: &str) -> String {
    format!("{}{body}", include_str!("../shaders/blend.wgsl"))
}
