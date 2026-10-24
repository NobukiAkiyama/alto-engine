//! What the compositor draws, independent of any GPU API. The engine builds a
//! `RenderScene` from the document; renderers and damage tracking read it.
use alto_core::{ContentHash, IRect, NodeId, TileKey};

pub mod damage;

pub type RIdx = usize;
pub type TargetId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderBlend {
    Normal = 0,
    Multiply = 1,
    Screen = 2,
    Overlay = 3,
    Darken = 4,
    Lighten = 5,
    ColorDodge = 6,
    ColorBurn = 7,
    HardLight = 8,
    SoftLight = 9,
    Difference = 10,
    Exclusion = 11,
    Hue = 12,
    Saturation = 13,
    Color = 14,
    Luminosity = 15,
    Add = 16,
}

/// Bytes left for evictable caches after the frame and the fixed textures are reserved.
/// `frame` is the stroke and effect targets. `fixed` is memory that cannot be freed this frame.
pub fn cache_allowance(budget: u64, frame: u64, fixed: u64) -> Option<u64> {
    budget.checked_sub(frame.saturating_add(fixed))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderTile {
    pub key: TileKey,
    pub hash: ContentHash,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RContent {
    Group {
        children: Vec<RIdx>,
        isolated: bool,
    },
    Tiles(Vec<RenderTile>),
    Mesh(std::sync::Arc<Vec<alto_vector::StyledPath>>),
    /// Text as size runs: vector paths for large em sizes, the glyph atlas for small ones.
    Text(std::sync::Arc<Vec<alto_vector::TextRun>>),
    Adjustment(REffect),
    Unsupported,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RMask {
    pub tiles: Vec<RenderTile>,
    pub linked: bool,
    pub transform: [f64; 6],
}
#[derive(Clone, Debug, PartialEq)]
pub enum REffectKind {
    GaussianBlur {
        radius: f32,
    },
    ColorAdjust {
        brightness: f32,
        contrast: f32,
        saturation: f32,
    },
    Levels {
        in_black: f32,
        in_white: f32,
        gamma: f32,
        out_black: f32,
        out_white: f32,
    },
    Curves {
        points: Vec<(f32, f32)>,
    },
    HueSaturation {
        hue: f32,
        saturation: f32,
        lightness: f32,
    },
    Invert,
}
#[derive(Clone, Debug, PartialEq)]
pub struct REffect {
    pub kind: REffectKind,
    pub selection: Option<RMask>,
}
#[derive(Clone, Debug, PartialEq, Default)]
pub struct RFeatures {
    pub mask: Option<RMask>,
    pub vector_mask: Option<alto_vector::VectorMask>,
    pub clip_path: Option<alto_vector::ClipPath>,
    pub clip: bool,
    pub effects: Vec<REffect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RNode {
    pub key: NodeId,
    pub version: u64,
    pub visible: bool,
    /// Document matrix. The compositor casts to f32 only when it writes a GPU command.
    pub transform: [f64; 6],
    pub opacity: f32,
    pub blend: RenderBlend,
    pub effects: usize,
    pub features: RFeatures,
    pub content: RContent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderScene {
    pub canvas: IRect,
    pub root: RIdx,
    pub nodes: Vec<RNode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pass {
    Draw {
        node: RIdx,
        region: IRect,
        into: TargetId,
    },
    BlendIsolated {
        src: TargetId,
        dst: TargetId,
        out: TargetId,
        mode: RenderBlend,
        opacity_bits: u32,
    },
    Effect {
        node: RIdx,
        effect_index: usize,
        input: TargetId,
        out: TargetId,
        roi: IRect,
    },
    Present {
        from: TargetId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetDesc {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderPlan {
    pub passes: Vec<Pass>,
    pub temps: Vec<TargetDesc>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlanError {
    MissingNode(RIdx),
    Cycle(RIdx),
    InvalidCanvas,
}

impl RenderScene {
    pub fn compile(&self, region: IRect) -> Result<RenderPlan, PlanError> {
        if region.x1 <= region.x0 || region.y1 <= region.y0 {
            return Err(PlanError::InvalidCanvas);
        }
        let mut plan = RenderPlan {
            passes: Vec::new(),
            temps: vec![TargetDesc {
                width: (region.x1 - region.x0) as u32,
                height: (region.y1 - region.y0) as u32,
            }],
        };
        let mut visiting = vec![false; self.nodes.len()];
        let final_target = self.compile_node(self.root, region, 0, &mut plan, &mut visiting)?;
        plan.passes.push(Pass::Present { from: final_target });
        Ok(plan)
    }

    fn compile_node(
        &self,
        index: RIdx,
        region: IRect,
        parent: TargetId,
        plan: &mut RenderPlan,
        visiting: &mut [bool],
    ) -> Result<TargetId, PlanError> {
        let node = self.nodes.get(index).ok_or(PlanError::MissingNode(index))?;
        if !node.visible {
            return Ok(parent);
        }
        if visiting[index] {
            return Err(PlanError::Cycle(index));
        }
        visiting[index] = true;
        let separate = node.opacity < 1.0
            || node.blend != RenderBlend::Normal
            || node.effects > 0
            || matches!(&node.content, RContent::Group { isolated: true, .. });
        let target = if separate {
            plan.temps.push(TargetDesc {
                width: (region.x1 - region.x0) as u32,
                height: (region.y1 - region.y0) as u32,
            });
            plan.temps.len() - 1
        } else {
            parent
        };
        let mut source = match &node.content {
            RContent::Group { children, .. } => {
                let mut current = target;
                for child in children {
                    current = self.compile_node(*child, region, current, plan, visiting)?;
                }
                current
            }
            RContent::Tiles(_) | RContent::Mesh(_) | RContent::Text(_) => {
                plan.passes.push(Pass::Draw {
                    node: index,
                    region,
                    into: target,
                });
                target
            }
            RContent::Unsupported | RContent::Adjustment(_) => target,
        };
        for effect_index in 0..node.effects {
            plan.temps.push(TargetDesc {
                width: (region.x1 - region.x0) as u32,
                height: (region.y1 - region.y0) as u32,
            });
            let out = plan.temps.len() - 1;
            plan.passes.push(Pass::Effect {
                node: index,
                effect_index,
                input: source,
                out,
                roi: region,
            });
            source = out;
        }
        let result = if separate {
            plan.temps.push(TargetDesc {
                width: (region.x1 - region.x0) as u32,
                height: (region.y1 - region.y0) as u32,
            });
            let out = plan.temps.len() - 1;
            plan.passes.push(Pass::BlendIsolated {
                src: source,
                dst: parent,
                out,
                mode: node.blend,
                opacity_bits: node.opacity.to_bits(),
            });
            out
        } else {
            source
        };
        visiting[index] = false;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn node(content: RContent) -> RNode {
        RNode {
            key: NodeId::new(),
            version: 0,
            visible: true,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            opacity: 1.0,
            blend: RenderBlend::Normal,
            effects: 0,
            features: RFeatures::default(),
            content,
        }
    }
    #[test]
    fn scene_compiles_in_tree_order_and_isolates_blends() {
        let mut upper = node(RContent::Tiles(vec![]));
        upper.blend = RenderBlend::Multiply;
        let scene = RenderScene {
            canvas: IRect {
                x0: 0,
                y0: 0,
                x1: 512,
                y1: 512,
            },
            root: 0,
            nodes: vec![
                node(RContent::Group {
                    children: vec![1, 2],
                    isolated: false,
                }),
                node(RContent::Tiles(vec![])),
                upper,
            ],
        };
        let plan = scene.compile(scene.canvas).unwrap();
        assert_eq!(
            plan.passes[0],
            Pass::Draw {
                node: 1,
                region: scene.canvas,
                into: 0
            }
        );
        assert_eq!(
            plan.passes[1],
            Pass::Draw {
                node: 2,
                region: scene.canvas,
                into: 1
            }
        );
        assert!(matches!(
            plan.passes[2],
            Pass::BlendIsolated {
                mode: RenderBlend::Multiply,
                ..
            }
        ));
        assert_eq!(plan.passes[3], Pass::Present { from: 2 });
    }

    #[test]
    fn cache_allowance_reserves_the_frame_before_the_cache() {
        assert_eq!(cache_allowance(1000, 400, 300), Some(300));
        assert_eq!(cache_allowance(1000, 700, 400), None);
        assert_eq!(cache_allowance(1000, 1000, 0), Some(0));
    }
}
