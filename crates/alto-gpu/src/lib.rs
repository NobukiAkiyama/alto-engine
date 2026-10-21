use alto_core::{AssetId, ContentHash, IRect, NodeId, TileKey};
use std::collections::HashMap;

pub mod glyph_atlas;
pub mod renderer;

#[cfg(test)]
mod blend_ref;

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

/// Blend math lives in one shader. Composite and the effect pass both prepend it.
pub fn shader_with_blend(body: &str) -> String {
    format!("{}{body}", include_str!("../shaders/blend.wgsl"))
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureHandle {
    pub index: u32,
    pub generation: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GpuSource {
    Tile(ContentHash),
    Asset(AssetId),
    Derived(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acquire {
    Ready(TextureHandle),
    Pending(TextureHandle),
}

#[derive(Clone, Debug)]
struct Slot {
    generation: u32,
    source: Option<GpuSource>,
    bytes: u64,
    last_used: u64,
    pins: u32,
    resident: bool,
}

pub struct ResourceManager {
    slots: Vec<Slot>,
    free: Vec<u32>,
    by_source: HashMap<GpuSource, TextureHandle>,
    budget_bytes: u64,
    used_bytes: u64,
    frame: u64,
}

impl ResourceManager {
    pub fn new(budget_bytes: u64) -> Self {
        Self {
            slots: vec![],
            free: vec![],
            by_source: HashMap::new(),
            budget_bytes,
            used_bytes: 0,
            frame: 0,
        }
    }
    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    pub fn acquire(&mut self, source: GpuSource) -> Acquire {
        if let Some(handle) = self.by_source.get(&source).copied() {
            let slot = &mut self.slots[handle.index as usize];
            slot.last_used = self.frame;
            return if slot.resident {
                Acquire::Ready(handle)
            } else {
                Acquire::Pending(handle)
            };
        }
        let index = if let Some(index) = self.free.pop() {
            index
        } else {
            self.slots.push(Slot {
                generation: 0,
                source: None,
                bytes: 0,
                last_used: 0,
                pins: 0,
                resident: false,
            });
            (self.slots.len() - 1) as u32
        };
        let slot = &mut self.slots[index as usize];
        slot.source = Some(source.clone());
        slot.last_used = self.frame;
        let handle = TextureHandle {
            index,
            generation: slot.generation,
        };
        self.by_source.insert(source, handle);
        Acquire::Pending(handle)
    }

    pub fn mark_ready(&mut self, handle: TextureHandle, bytes: u64) -> bool {
        let Some(slot) = self.slots.get_mut(handle.index as usize) else {
            return false;
        };
        if slot.generation != handle.generation || slot.source.is_none() {
            return false;
        }
        if slot.resident {
            self.used_bytes = self.used_bytes.saturating_sub(slot.bytes);
        }
        slot.resident = true;
        slot.bytes = bytes;
        slot.last_used = self.frame;
        self.used_bytes = self.used_bytes.saturating_add(bytes);
        true
    }

    pub fn resolve(&self, handle: TextureHandle) -> Option<&GpuSource> {
        let slot = self.slots.get(handle.index as usize)?;
        if slot.generation == handle.generation && slot.resident {
            slot.source.as_ref()
        } else {
            None
        }
    }

    pub fn pin(&mut self, handle: TextureHandle) -> bool {
        let Some(slot) = self.slots.get_mut(handle.index as usize) else {
            return false;
        };
        if slot.generation != handle.generation || !slot.resident {
            return false;
        }
        slot.pins = slot.pins.saturating_add(1);
        true
    }

    pub fn end_frame(&mut self) {
        self.frame = self.frame.saturating_add(1);
        for slot in &mut self.slots {
            slot.pins = 0;
        }
        while self.used_bytes > self.budget_bytes {
            let candidate = self
                .slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| slot.resident && slot.pins == 0)
                .min_by_key(|(_, slot)| {
                    (
                        slot.last_used,
                        !matches!(slot.source, Some(GpuSource::Derived(_))),
                    )
                })
                .map(|(index, _)| index);
            let Some(index) = candidate else {
                break;
            };
            let slot = &mut self.slots[index];
            self.used_bytes = self.used_bytes.saturating_sub(slot.bytes);
            slot.bytes = 0;
            slot.resident = false;
            slot.generation = slot.generation.wrapping_add(1);
            if let Some(source) = slot.source.take() {
                self.by_source.remove(&source);
            }
            self.free.push(index as u32);
        }
    }

    pub fn on_device_lost(&mut self) {
        self.used_bytes = 0;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(source) = &slot.source {
                slot.resident = false;
                slot.bytes = 0;
                slot.pins = 0;
                slot.generation = slot.generation.wrapping_add(1);
                self.by_source.insert(
                    source.clone(),
                    TextureHandle {
                        index: index as u32,
                        generation: slot.generation,
                    },
                );
            }
        }
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

    #[test]
    fn resource_budget_evicts_derived_first_and_invalidates_handle() {
        let mut resources = ResourceManager::new(100);
        let tile_source = GpuSource::Tile(ContentHash::of(b"tile"));
        let tile = match resources.acquire(tile_source.clone()) {
            Acquire::Pending(handle) => handle,
            _ => panic!("new tile must be pending"),
        };
        let derived = match resources.acquire(GpuSource::Derived(7)) {
            Acquire::Pending(handle) => handle,
            _ => panic!("new cache must be pending"),
        };
        assert!(resources.mark_ready(tile, 80));
        assert!(resources.mark_ready(derived, 80));
        resources.end_frame();
        assert_eq!(resources.used_bytes(), 80);
        assert_eq!(resources.resolve(tile), Some(&tile_source));
        assert_eq!(resources.resolve(derived), None);
    }

    #[test]
    fn device_loss_rebuilds_from_source_with_new_generation() {
        let mut resources = ResourceManager::new(1024);
        let source = GpuSource::Tile(ContentHash::of(b"rebuild"));
        let old = match resources.acquire(source.clone()) {
            Acquire::Pending(handle) => handle,
            _ => panic!("new tile must be pending"),
        };
        assert!(resources.mark_ready(old, 256));
        resources.on_device_lost();
        assert_eq!(resources.used_bytes(), 0);
        assert_eq!(resources.resolve(old), None);
        let new = match resources.acquire(source.clone()) {
            Acquire::Pending(handle) => handle,
            _ => panic!("lost tile must need rebuild"),
        };
        assert_ne!(old, new);
        assert!(resources.mark_ready(new, 256));
        assert_eq!(resources.resolve(new), Some(&source));
    }
}
