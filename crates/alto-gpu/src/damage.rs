//! Which document pixels can differ between two render scenes.
//!
//! Display tiles are composites of the whole scene, so an edit only needs to discard
//! the tiles it can change. The comparison runs on render scenes, not on document
//! operations: the scene is exactly what the compositor reads.
use crate::renderer::{inverse, multiply, point};
use crate::{RContent, RNode, RenderScene, RenderTile};
use alto_core::{NodeId, Rect, TILE};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Damage {
    None,
    /// Document coordinates.
    Rects(Vec<Rect>),
    All,
}

/// More rectangles than this are merged into their bounding box.
const MAX_RECTS: usize = 64;

/// A scene the compositor draws with per-pixel operations only. Effects, masks, clips,
/// vectors and text read neighbouring pixels or use other render paths.
pub fn plain_scene(scene: &RenderScene) -> bool {
    scene.nodes.iter().all(|n| {
        n.features.vector_mask.is_none()
            && n.features.clip_path.is_none()
            && n.features.mask.is_none()
            && !n.features.clip
            && n.features.effects.is_empty()
            && matches!(
                n.content,
                RContent::Group { .. } | RContent::Tiles(_) | RContent::Unsupported
            )
    })
}

/// A node as the compositor sees it: accumulated transform and effective visibility.
struct Placed<'a> {
    node: &'a RNode,
    world: [f64; 6],
    shown: bool,
    children: Vec<NodeId>,
}

fn place(scene: &RenderScene) -> Option<HashMap<NodeId, Placed<'_>>> {
    let mut out = HashMap::new();
    let mut stack = vec![(scene.root, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], true)];
    while let Some((idx, parent, parent_shown)) = stack.pop() {
        let node = scene.nodes.get(idx)?;
        let world = multiply(parent, node.transform);
        let shown = parent_shown && node.visible && node.opacity > 0.0;
        let mut children = Vec::new();
        if let RContent::Group { children: c, .. } = &node.content {
            for &child in c {
                children.push(scene.nodes.get(child)?.key);
                stack.push((child, world, shown));
            }
        }
        let placed = Placed {
            node,
            world,
            shown,
            children,
        };
        // A node reached twice is not a tree; let the caller fall back.
        if out.insert(node.key, placed).is_some() {
            return None;
        }
    }
    Some(out)
}

/// Document pixels a layer-space rectangle can affect once drawn through `world`.
pub fn layer_rect_damage(world: [f64; 6], r: Rect) -> Rect {
    let corners = [[r.x0, r.y0], [r.x1, r.y0], [r.x0, r.y1], [r.x1, r.y1]].map(|p| point(world, p));
    // Filtering reads up to one source pixel beyond the rectangle: `scale` document pixels.
    let scale = world[0]
        .hypot(world[1])
        .max(world[2].hypot(world[3]))
        .max(1.0);
    let margin = scale.ceil() + 1.0;
    Rect {
        x0: corners.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min) - margin,
        y0: corners.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min) - margin,
        x1: corners
            .iter()
            .map(|p| p[0])
            .fold(f64::NEG_INFINITY, f64::max)
            + margin,
        y1: corners
            .iter()
            .map(|p| p[1])
            .fold(f64::NEG_INFINITY, f64::max)
            + margin,
    }
}

pub fn union(a: Rect, b: Rect) -> Rect {
    Rect {
        x0: a.x0.min(b.x0),
        y0: a.y0.min(b.y0),
        x1: a.x1.max(b.x1),
        y1: a.y1.max(b.y1),
    }
}

fn tile_rect(world: [f64; 6], tile: &RenderTile) -> Rect {
    let s = TILE as f64;
    let (x0, y0) = (tile.key.tx as f64 * s, tile.key.ty as f64 * s);
    layer_rect_damage(
        world,
        Rect {
            x0,
            y0,
            x1: x0 + s,
            y1: y0 + s,
        },
    )
}

fn add_node(placed: &HashMap<NodeId, Placed<'_>>, id: NodeId, out: &mut Vec<Rect>) {
    let mut stack = vec![id];
    while let Some(id) = stack.pop() {
        let Some(p) = placed.get(&id) else { continue };
        if !p.shown {
            continue;
        }
        if let RContent::Tiles(tiles) = &p.node.content
            && inverse(p.world).is_some()
        {
            out.extend(tiles.iter().map(|t| tile_rect(p.world, t)));
        }
        stack.extend(p.children.iter().copied());
    }
}

/// Tiles present in only one list, or with a different hash. Both lists are sorted by key.
fn changed_tiles<'a>(old: &'a [RenderTile], new: &'a [RenderTile]) -> Vec<&'a RenderTile> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() || j < new.len() {
        match (old.get(i), new.get(j)) {
            (Some(a), Some(b)) if a.key == b.key => {
                if a.hash != b.hash {
                    out.push(b);
                }
                i += 1;
                j += 1;
            }
            (Some(a), Some(b)) if a.key < b.key => {
                out.push(a);
                i += 1;
            }
            (Some(_), Some(b)) => {
                out.push(b);
                j += 1;
            }
            (Some(a), None) => {
                out.push(a);
                i += 1;
            }
            (None, Some(b)) => {
                out.push(b);
                j += 1;
            }
            (None, None) => break,
        }
    }
    out
}

fn same_composite(a: &Placed<'_>, b: &Placed<'_>) -> bool {
    a.world == b.world
        && a.node.opacity == b.node.opacity
        && a.node.blend == b.node.blend
        && a.children == b.children
        && match (&a.node.content, &b.node.content) {
            (RContent::Group { isolated: x, .. }, RContent::Group { isolated: y, .. }) => x == y,
            (RContent::Tiles(_), RContent::Tiles(_)) => true,
            (RContent::Unsupported, RContent::Unsupported) => true,
            _ => false,
        }
}

pub fn scene_damage(old: &RenderScene, new: &RenderScene) -> Damage {
    if old.canvas != new.canvas || !plain_scene(old) || !plain_scene(new) {
        return Damage::All;
    }
    let (Some(before), Some(after)) = (place(old), place(new)) else {
        return Damage::All;
    };
    let mut rects = Vec::new();
    for (id, a) in &before {
        if !after.contains_key(id) && a.shown {
            add_node(&before, *id, &mut rects);
        }
    }
    for (id, b) in &after {
        let Some(a) = before.get(id) else {
            add_node(&after, *id, &mut rects);
            continue;
        };
        if !a.shown && !b.shown {
            continue;
        }
        if a.shown != b.shown || !same_composite(a, b) {
            add_node(&before, *id, &mut rects);
            add_node(&after, *id, &mut rects);
            continue;
        }
        if let (RContent::Tiles(x), RContent::Tiles(y)) = (&a.node.content, &b.node.content)
            && inverse(b.world).is_some()
        {
            rects.extend(
                changed_tiles(x, y)
                    .into_iter()
                    .map(|t| tile_rect(b.world, t)),
            );
        }
    }
    if rects.is_empty() {
        return Damage::None;
    }
    if rects.len() > MAX_RECTS {
        rects = vec![rects.iter().copied().fold(rects[0], union)];
    }
    Damage::Rects(rects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RFeatures, RenderBlend};
    use alto_core::{ContentHash, IRect, TileKey};

    const ID: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

    fn node(key: NodeId, content: RContent) -> RNode {
        RNode {
            key,
            version: 0,
            visible: true,
            transform: ID,
            opacity: 1.0,
            blend: RenderBlend::Normal,
            effects: 0,
            features: RFeatures::default(),
            content,
        }
    }
    fn tile(tx: i32, ty: i32, seed: &[u8]) -> RenderTile {
        RenderTile {
            key: TileKey { tx, ty },
            hash: ContentHash::of(seed),
        }
    }
    struct Fixture {
        scene: RenderScene,
    }
    /// Root group with two raster layers: A covers tiles (0,0),(1,0); B covers (0,1).
    fn fixture() -> Fixture {
        let (root, a, b) = (NodeId::new(), NodeId::new(), NodeId::new());
        let scene = RenderScene {
            canvas: IRect {
                x0: 0,
                y0: 0,
                x1: 1024,
                y1: 1024,
            },
            root: 0,
            nodes: vec![
                node(
                    root,
                    RContent::Group {
                        children: vec![1, 2],
                        isolated: false,
                    },
                ),
                node(
                    a,
                    RContent::Tiles(vec![tile(0, 0, b"a0"), tile(1, 0, b"a1")]),
                ),
                node(b, RContent::Tiles(vec![tile(0, 1, b"b0")])),
            ],
        };
        Fixture { scene }
    }
    fn covers(d: &Damage, x: f64, y: f64) -> bool {
        match d {
            Damage::All => true,
            Damage::None => false,
            Damage::Rects(rs) => rs
                .iter()
                .any(|r| r.x0 <= x && x < r.x1 && r.y0 <= y && y < r.y1),
        }
    }

    #[test]
    fn identical_scenes_have_no_damage() {
        let f = fixture();
        assert_eq!(scene_damage(&f.scene, &f.scene.clone()), Damage::None);
    }

    #[test]
    fn a_changed_tile_damages_only_that_tile() {
        let f = fixture();
        let mut next = f.scene.clone();
        next.nodes[1].content = RContent::Tiles(vec![tile(0, 0, b"a0"), tile(1, 0, b"painted")]);
        let d = scene_damage(&f.scene, &next);
        assert!(covers(&d, 300.0, 100.0));
        assert!(!covers(&d, 100.0, 100.0));
        assert!(!covers(&d, 100.0, 300.0));
        assert!(!covers(&d, 700.0, 700.0));
    }

    #[test]
    fn added_and_removed_tiles_are_damaged() {
        let f = fixture();
        let mut next = f.scene.clone();
        next.nodes[1].content = RContent::Tiles(vec![tile(1, 0, b"a1"), tile(2, 2, b"new")]);
        let d = scene_damage(&f.scene, &next);
        assert!(covers(&d, 100.0, 100.0), "removed (0,0)");
        assert!(covers(&d, 600.0, 600.0), "added (2,2)");
        assert!(!covers(&d, 300.0, 100.0));
    }

    #[test]
    fn moving_a_layer_damages_its_old_and_new_place() {
        let f = fixture();
        let mut next = f.scene.clone();
        next.nodes[2].transform = [1.0, 0.0, 0.0, 1.0, 512.0, 0.0];
        let d = scene_damage(&f.scene, &next);
        assert!(covers(&d, 100.0, 300.0), "old place");
        assert!(covers(&d, 600.0, 300.0), "new place");
        assert!(!covers(&d, 300.0, 100.0), "layer A is untouched");
    }

    #[test]
    fn hiding_a_layer_and_changing_opacity_damage_the_layer() {
        let f = fixture();
        let mut hidden = f.scene.clone();
        hidden.nodes[2].visible = false;
        assert!(covers(&scene_damage(&f.scene, &hidden), 100.0, 300.0));
        let mut faded = f.scene.clone();
        faded.nodes[1].opacity = 0.5;
        let d = scene_damage(&f.scene, &faded);
        assert!(covers(&d, 100.0, 100.0) && covers(&d, 300.0, 100.0));
        assert!(!covers(&d, 100.0, 300.0));
    }

    #[test]
    fn reordering_children_damages_the_group_contents() {
        let f = fixture();
        let mut next = f.scene.clone();
        next.nodes[0].content = RContent::Group {
            children: vec![2, 1],
            isolated: false,
        };
        let d = scene_damage(&f.scene, &next);
        assert!(covers(&d, 100.0, 100.0) && covers(&d, 100.0, 300.0));
    }

    #[test]
    fn group_transform_damages_descendants_in_both_places() {
        let f = fixture();
        let mut next = f.scene.clone();
        next.nodes[0].transform = [1.0, 0.0, 0.0, 1.0, 0.0, 512.0];
        let d = scene_damage(&f.scene, &next);
        assert!(covers(&d, 100.0, 100.0), "old");
        assert!(covers(&d, 100.0, 600.0), "new");
    }

    #[test]
    fn inserted_and_removed_layers_are_damaged() {
        let f = fixture();
        let mut next = f.scene.clone();
        let c = NodeId::new();
        next.nodes
            .push(node(c, RContent::Tiles(vec![tile(3, 3, b"c")])));
        next.nodes[0].content = RContent::Group {
            children: vec![1, 3],
            isolated: false,
        };
        let d = scene_damage(&f.scene, &next);
        assert!(covers(&d, 100.0, 300.0), "B removed");
        assert!(covers(&d, 800.0, 800.0), "C inserted");
    }

    #[test]
    fn invisible_edits_have_no_damage() {
        let f = fixture();
        let mut old = f.scene.clone();
        old.nodes[2].visible = false;
        let mut next = old.clone();
        next.nodes[2].content = RContent::Tiles(vec![tile(0, 1, b"other")]);
        assert_eq!(scene_damage(&old, &next), Damage::None);
    }

    #[test]
    fn effects_and_canvas_changes_fall_back_to_everything() {
        let f = fixture();
        let mut masked = f.scene.clone();
        masked.nodes[1].features.clip = true;
        assert_eq!(scene_damage(&f.scene, &masked), Damage::All);
        let mut grown = f.scene.clone();
        grown.canvas.x1 = 2048;
        assert_eq!(scene_damage(&f.scene, &grown), Damage::All);
    }

    #[test]
    fn many_rects_collapse_to_their_bounds() {
        let f = fixture();
        let mut old = f.scene.clone();
        let mut next = f.scene.clone();
        old.nodes[1].content = RContent::Tiles((0..80).map(|i| tile(i, 0, b"x")).collect());
        next.nodes[1].content = RContent::Tiles((0..80).map(|i| tile(i, 0, b"y")).collect());
        old.canvas.x1 = 80 * 256;
        next.canvas.x1 = 80 * 256;
        let Damage::Rects(rs) = scene_damage(&old, &next) else {
            panic!()
        };
        assert_eq!(rs.len(), 1);
        assert!(rs[0].x0 <= 0.0 && rs[0].x1 >= 80.0 * 256.0);
    }
}
