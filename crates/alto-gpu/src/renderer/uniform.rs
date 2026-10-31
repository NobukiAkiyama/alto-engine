//! Render version 3 builds a level-n tile only from its 4^n level-0 tiles
//! (DQ3-1). Away from every path edge those leaves are all the same bytes, so a
//! subtree there is built from one leaf: each level the integer downsample of
//! four copies of the level below, which is what the full chain computes.
//!
//! Why the leaves are equal: in the scenes accepted here every pixel is a
//! pointwise function of each node's coverage byte at that pixel. A nonzero or
//! even-odd coverage cell crossed by no segment is the winding number's byte
//! (`raster_q8`'s cell integration; a curve left of the window becomes its
//! chord, with the same winding to its right), and no segment crosses a
//! footprint that no segment box touches, so the winding is one number over it.
use super::*;
use std::sync::Arc;

/// Q8 units added around each segment box, a margin beyond the exact segments.
const MARGIN: i64 = 512;
/// Past this many segments in a scene the shortcut is not taken.
const SEGMENT_LIMIT: usize = 1 << 18;

/// What [`Renderer::uniform_subtree`] read from one node. Holding the paths keeps
/// them unchanged for as long as the memo is kept.
#[derive(Clone)]
enum Input {
    Group(Vec<usize>),
    Mesh(Arc<Vec<alto_vector::StyledPath>>),
    /// Anything else: the scene is not accepted.
    Other,
}

#[derive(Clone)]
struct NodeInput {
    visible: bool,
    transform: [u64; 6],
    plain: bool,
    input: Input,
}

pub(super) struct Memo {
    canvas: alto_core::IRect,
    root: usize,
    nodes: Vec<NodeInput>,
    /// Level-0 Q8 boxes around every fill segment, or `None` when the scene is not one
    /// whose leaves follow the winding alone.
    edges: Option<Vec<[i64; 4]>>,
}

fn read(scene: &RenderScene) -> Vec<NodeInput> {
    scene
        .nodes
        .iter()
        .map(|n| NodeInput {
            visible: n.visible,
            transform: n.transform.map(f64::to_bits),
            plain: n.features == Default::default(),
            input: match &n.content {
                RContent::Group { children, .. } => Input::Group(children.clone()),
                RContent::Mesh(paths) => Input::Mesh(paths.clone()),
                _ => Input::Other,
            },
        })
        .collect()
}

fn same(a: &[NodeInput], b: &[NodeInput]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.visible == b.visible
                && a.transform == b.transform
                && a.plain == b.plain
                && match (&a.input, &b.input) {
                    (Input::Group(a), Input::Group(b)) => a == b,
                    (Input::Mesh(a), Input::Mesh(b)) => Arc::ptr_eq(a, b),
                    (Input::Other, Input::Other) => true,
                    _ => false,
                }
        })
}

/// Segment boxes of the visible tree under `index`, or `None` for a node this
/// argument does not cover: effects, masks, clipping, rasters, text,
/// adjustments, or a visible stroke.
fn edges(
    scene: &RenderScene,
    index: usize,
    parent: WorldTransform,
    depth: usize,
    out: &mut Vec<[i64; 4]>,
) -> Option<()> {
    if depth > alto_core::MAX_TREE_DEPTH as usize {
        return None;
    }
    let node = scene.nodes.get(index)?;
    if node.features != Default::default() {
        return None;
    }
    let world = parent.concat(node.transform).ok()?;
    match &node.content {
        RContent::Group { children, .. } => {
            for &child in children {
                if scene.nodes.get(child)?.visible {
                    edges(scene, child, world, depth + 1, out)?;
                }
            }
        }
        RContent::Mesh(paths) => {
            let world = world.fixed().ok()?;
            for path in paths.iter() {
                if path
                    .stroke
                    .as_ref()
                    .is_some_and(crate::raster_q8::stroke_visible)
                {
                    return None;
                }
                let commands = crate::raster_q8::map_path(&path.data, world, 0)?;
                let room = SEGMENT_LIMIT.checked_sub(out.len())?;
                for (a, b) in crate::raster_q8::fill_segments(&commands, room)? {
                    out.push([
                        a[0].min(b[0]) - MARGIN,
                        a[1].min(b[1]) - MARGIN,
                        a[0].max(b[0]) + MARGIN,
                        a[1].max(b[1]) + MARGIN,
                    ]);
                }
            }
        }
        _ => return None,
    }
    Some(())
}

impl Renderer {
    /// Every level-0 tile under `key` is the same bytes (see the module comment).
    /// Only for a level >= 1 key whose whole footprint lies inside the canvas, so
    /// no descendant is missing at the canvas edge.
    pub(super) fn uniform_subtree(&mut self, scene: &RenderScene, key: CacheKey) -> bool {
        if !self.uniform_pyramid
            || !self.integer_cache
            || scene.render_version != alto_core::RENDER_VERSION_EXACT
            || key.0 <= 0
            || key.0 > 12
        {
            return false;
        }
        let span = 256i64 << key.0;
        let (x0, y0) = (key.1 as i64 * span, key.2 as i64 * span);
        let c = scene.canvas;
        if c.x0 != 0 || c.y0 != 0 || x0 < 0 || y0 < 0 || x0 + span > c.x1 as i64 {
            return false;
        }
        if y0 + span > c.y1 as i64 {
            return false;
        }
        let nodes = read(scene);
        let fresh = self.uniform.as_ref().is_some_and(|m| {
            m.canvas == scene.canvas && m.root == scene.root && same(&m.nodes, &nodes)
        });
        if !fresh {
            let mut out = Vec::new();
            let accepted = scene.nodes.get(scene.root).is_some_and(|n| n.visible)
                && edges(
                    scene,
                    scene.root,
                    WorldTransform::root(scene.render_version),
                    0,
                    &mut out,
                )
                .is_some();
            self.uniform = Some(Memo {
                canvas: scene.canvas,
                root: scene.root,
                nodes,
                edges: accepted.then_some(out),
            });
        }
        let Some(edges) = self.uniform.as_ref().and_then(|m| m.edges.as_ref()) else {
            return false;
        };
        let (fx0, fy0, fx1, fy1) = (x0 * 256, y0 * 256, (x0 + span) * 256, (y0 + span) * 256);
        !edges
            .iter()
            .any(|b| b[0] <= fx1 && b[2] >= fx0 && b[1] <= fy1 && b[3] >= fy0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alto_vector::PathCommand as P;

    fn shape(transform: f64, paths: Vec<alto_vector::StyledPath>) -> RenderScene {
        alto_scene::RenderScene {
            canvas: alto_core::IRect {
                x0: 0,
                y0: 0,
                x1: 65536,
                y1: 65536,
            },
            root: 0,
            render_version: alto_core::RENDER_VERSION_EXACT,
            nodes: vec![alto_scene::RNode {
                key: alto_core::NodeId::new(),
                version: 0,
                visible: true,
                transform: [transform, 0.0, 0.0, transform, 0.0, 0.0],
                opacity: 1.0,
                blend: RenderBlend::Normal,
                effects: 0,
                features: Default::default(),
                content: RContent::Mesh(Arc::new(paths)),
            }],
        }
    }

    fn curve(fill: [f32; 4], rule: alto_vector::FillRule) -> alto_vector::StyledPath {
        alto_vector::StyledPath {
            data: alto_vector::PathData {
                commands: vec![
                    P::Move { x: 128.0, y: 192.0 },
                    P::Quad {
                        cx: 256.0,
                        cy: 64.0,
                        x: 384.0,
                        y: 192.0,
                    },
                    P::Line { x: 384.0, y: 400.0 },
                    P::Line { x: 128.0, y: 400.0 },
                    P::Close,
                ],
            },
            fill: Some(fill),
            stroke: None,
            fill_rule: rule,
        }
    }

    type Poller = (
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::thread::JoinHandle<()>,
    );
    fn headless() -> Option<(Renderer, Poller)> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::METAL | wgpu::Backends::VULKAN | wgpu::Backends::DX12;
        let instance = wgpu::Instance::new(descriptor);
        let renderer = pollster::block_on(Renderer::headless(&instance, 64)).ok()?;
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (device, stop) = (renderer.device.clone(), done.clone());
        let poller = std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                let _ = device.poll(wgpu::PollType::Poll);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        Some((renderer, (done, poller)))
    }

    /// The shortcut leaves every byte as the full chain: tiles on the curve, inside
    /// and outside it, and across the canvas edge, through level 6.
    #[test]
    fn uniform_subtrees_match_the_full_pyramid() {
        let _gpu = crate::cpu_composite::GPU_RENDERER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some((mut r, (done, poller))) = headless() else {
            return;
        };
        let store = alto_raster::ObjectStore::default();
        let mut overlap = shape(
            128.0,
            vec![
                curve([1.0, 0.5, 0.25, 1.0], alto_vector::FillRule::NonZero),
                curve([0.2, 0.4, 0.9, 0.5], alto_vector::FillRule::EvenOdd),
            ],
        );
        let RContent::Mesh(paths) = &mut overlap.nodes[0].content else {
            unreachable!()
        };
        Arc::make_mut(paths)[1]
            .data
            .commands
            .push(P::Move { x: 0.0, y: 0.0 });
        overlap.nodes[0].opacity = 0.75;
        overlap.nodes[0].blend = RenderBlend::Multiply;
        let scenes = [
            shape(128.0, vec![curve([1.0; 4], alto_vector::FillRule::NonZero)]),
            overlap,
            shape(
                200.0,
                vec![curve([0.3, 0.6, 0.1, 0.8], alto_vector::FillRule::EvenOdd)],
            ),
        ];
        // Levels up to 4 everywhere; the deep reads once, on the curve.
        let mut keys = vec![(5, 3, 2), (6, 1, 1)];
        for level in 1..=4 {
            let n = 65536 >> (8 + level);
            for (x, y) in [
                ((192 * 128) >> (8 + level), (144 * 128) >> (8 + level)),
                (0, 0),
                (n / 2, n / 2),
                (n / 2, (300 * 128) >> (8 + level)),
                (n - 1, n - 1),
            ] {
                keys.push((level, x, y));
            }
        }
        let mut shortcuts = 0;
        for (i, scene) in scenes.iter().enumerate() {
            for &key in &keys[if i == 0 { 0 } else { 2 }..] {
                shortcuts += r.uniform_subtree(scene, key) as u32;
                r.uniform_pyramid = false;
                r.reset_render_cache();
                let full = pollster::block_on(r.read_tile(scene, &store, key)).unwrap();
                r.uniform_pyramid = true;
                r.reset_render_cache();
                let fast = pollster::block_on(r.read_tile(scene, &store, key)).unwrap();
                assert!(full == fast, "scene {i} {key:?}");
            }
        }
        assert!(shortcuts > 0, "no subtree took the shortcut");
        done.store(true, std::sync::atomic::Ordering::Release);
        poller.join().unwrap();
    }

    #[test]
    fn edges_refuse_what_the_winding_does_not_decide() {
        let fill = curve([1.0; 4], alto_vector::FillRule::NonZero);
        let mut stroked = fill.clone();
        stroked.stroke = Some(alto_vector::Stroke {
            color: [1.0; 4],
            width: 2.0,
            cap: Default::default(),
            join: Default::default(),
            miter_limit: 4.0,
        });
        let mut blurred = shape(128.0, vec![fill.clone()]);
        blurred.nodes[0].features.effects.push(alto_scene::REffect {
            kind: alto_scene::REffectKind::GaussianBlur {
                radius: 2.0,
                radius_q8: 512,
            },
            selection: None,
        });
        let root = WorldTransform::root(alto_core::RENDER_VERSION_EXACT);
        let mut out = Vec::new();
        assert!(edges(&shape(128.0, vec![fill]), 0, root, 0, &mut out).is_some());
        assert!(edges(&shape(128.0, vec![stroked]), 0, root, 0, &mut out).is_none());
        assert!(edges(&blurred, 0, root, 0, &mut out).is_none());
    }
}
