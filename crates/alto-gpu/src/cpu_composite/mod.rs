//! CPU reference for `shaders/composite.wgsl`, and a headless run of that shader.
//!
//! The reference is `#[cfg(test)]`, so the shipped WASM does not contain it. It follows
//! the shader: f64 world matrices, straight source texels, premultiplied accumulation,
//! and `rgba8unorm` ties-to-even quantization. Render version 2 (the default here)
//! filters bilinear and rotated samples with the integer 4-tap in `sample.rs`.
//! Render version 1 keeps the f32 `mix` path. It does not cull opaque layers; a
//! correct cull leaves the same pixels, and a wrong one fails the browser suite,
//! which reads production tiles.
//!
//! Masks, clips, blur, vectors and text are not on the composite path. `composite.wgsl`
//! never sees them (`plain_scene` is false, and the effect pass uses rgba16float).
//! Color adjust is the exception: for versions 1 and 2 the reference repeats
//! `effects.wgsl` mode 0, the normal blend onto clear, mode 3, and
//! `effects-resolve.wgsl`, with an f16 round trip after every store. Render version 3
//! runs the integer M5 stages of `effects-q14.wgsl` (Q14 in `rgba16uint`, resolved to
//! `rgba8uint`), and its reference is integer throughout. That includes the v3
//! pixel-mask stages (modes 21 to 23, `mask_programs`), whose Q8 mask lookup is
//! the compositor's sampler.
//! The parity image is a 256² gradient (every R, G, B and alpha, including alpha
//! 0, 1, 128 and 254) run through the contrast × saturation × brightness grid.
//! The other four stay explicit xfails in the browser suite.

mod q14;

use crate::blend_ref::{blend, effects_blend};
use crate::damage::plain_scene;
use crate::sample::{
    IntegerInverse, axis_unit, bilinear_byte, floor_frac_q8, integer_inverse, pixel_q8, source_q8,
};
use crate::shader_with_blend;
use alto_core::{Affine2, ContentHash, IRect, NodeId, RENDER_VERSION};
use alto_scene::{
    RContent, REffect, REffectKind, RFeatures, RMask, RNode, RenderBlend, RenderScene,
};
use alto_vector::{ClipPath, FillRule, PathData, VectorMask};
use bytemuck::{Pod, Zeroable};
use std::path::PathBuf;
mod cases;
mod color_adjust;
mod composite;
mod gpu;
mod gpu_parity_tests;
mod known_value_tests;
mod mask_programs;
mod measure;
mod numeric_tests;
mod q14_stages;
mod reference;
mod reference_tests;
mod renderer_cases;
mod renderer_runs;
mod shader_tests;
mod text_tests;
mod v3_tests;
use cases::*;
use color_adjust::*;
use composite::*;
use gpu::*;
use known_value_tests::*;
use mask_programs::*;
use measure::*;
use numeric_tests::*;
use q14_stages::*;
use reference::*;
use renderer_cases::*;
use renderer_runs::*;
use text_tests::*;
use v3_tests::*;

const BLENDS: [&str; 17] = [
    "normal",
    "multiply",
    "screen",
    "overlay",
    "darken",
    "lighten",
    "color_dodge",
    "color_burn",
    "hard_light",
    "soft_light",
    "difference",
    "exclusion",
    "hue",
    "saturation",
    "color",
    "luminosity",
    "add",
];

#[derive(Clone)]
struct Image {
    w: u32,
    h: u32,
    pixels: Vec<u8>,
}

#[derive(Clone)]
struct Tile {
    tx: i32,
    ty: i32,
    pixels: Vec<u8>,
}

enum Op {
    Push,
    Pop {
        opacity: f32,
        mode: u32,
    },
    Draw {
        inv: [f64; 6],
        q: Option<IntegerInverse>,
        opacity: f32,
        mode: u32,
        tiles: Vec<Tile>,
    },
}

#[derive(Clone)]
enum Node {
    Raster {
        image: Image,
        opacity: f32,
        mode: u32,
        visible: bool,
        transform: [f64; 6],
        /// Tiles merged after the image. The out-of-range case plants one at the
        /// coordinate a truncated i32 would wrap to, which an imported image
        /// cannot reach (width is at most 32768).
        extra_tiles: Vec<Tile>,
    },
    Group {
        isolated: bool,
        opacity: f32,
        mode: u32,
        visible: bool,
        transform: [f64; 6],
        children: Vec<Node>,
    },
}

#[derive(Clone)]
struct Case {
    name: String,
    w: u32,
    h: u32,
    level: i32,
    tx: i32,
    ty: i32,
    /// Written to `tests/parity/cpu-reference.txt` (render version 2).
    lock: bool,
    /// Semi-transparent coverage. Printed and saved, and a mismatch does not fail.
    measure: bool,
    nodes: Vec<Node>,
}

fn ident() -> [f64; 6] {
    [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
}

fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Image {
    Image {
        w,
        h,
        pixels: rgba.repeat((w * h) as usize),
    }
}

fn ramp(w: u32, h: u32) -> Image {
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            pixels[i] = (x & 255) as u8;
            pixels[i + 1] = (y & 255) as u8;
            pixels[i + 2] = ((x * 3 + y * 5) & 255) as u8;
            pixels[i + 3] = 255;
        }
    }
    Image { w, h, pixels }
}

fn checker(w: u32, h: u32, a: [u8; 4], b: [u8; 4]) -> Image {
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            pixels[i..i + 4].copy_from_slice(if (x + y) % 2 == 0 { &a } else { &b });
        }
    }
    Image { w, h, pixels }
}

fn raster(image: Image, mode: u32, opacity: f32, visible: bool, transform: [f64; 6]) -> Node {
    Node::Raster {
        image,
        opacity,
        mode,
        visible,
        transform,
        extra_tiles: Vec::new(),
    }
}

fn group(isolated: bool, opacity: f32, children: Vec<Node>) -> Node {
    Node::Group {
        isolated,
        opacity,
        mode: 0,
        visible: true,
        transform: ident(),
        children,
    }
}

pub(crate) static GPU_RENDERER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
