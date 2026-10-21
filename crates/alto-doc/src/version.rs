//! Versions: commits over canonical snapshots, the semantic diff between two documents,
//! and the reversible operations that restore an earlier version.
use crate::{AssetKind, Document, IntentLogEntry, LayerProps, Node, NodeKind, Op, PropPatch};
use alto_core::{AssetId, ContentHash, IRect, NodeId, TILE};
use alto_raster::TileMap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_MESSAGE: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentSummary {
    pub intent: String,
    pub targets: Vec<NodeId>,
    pub count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub parents: Vec<ContentHash>,
    pub snapshot: ContentHash,
    pub message: String,
    pub intents: Vec<IntentSummary>,
    /// Milliseconds since the Unix epoch.
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}
impl Commit {
    /// Canonical bytes: pretty JSON in field order with a trailing newline.
    pub fn bytes(&self) -> Vec<u8> {
        let mut b = serde_json::to_vec_pretty(self).expect("commit serializes");
        b.push(b'\n');
        b
    }
    pub fn hash(&self) -> ContentHash {
        ContentHash::of(&self.bytes())
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.message.chars().count() > MAX_MESSAGE
            || self
                .message
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err("commit message must be at most 4096 printable characters".into());
        }
        if self.parents.len() > 16 || self.intents.len() > 4096 {
            return Err("too many commit parents or intents".into());
        }
        if let Some(author) = &self.author
            && (author.is_empty()
                || author.chars().count() > 256
                || author
                    .chars()
                    .any(|c| c.is_control() || c == '<' || c == '>'))
        {
            return Err(
                "author name must be a single line of at most 256 characters without < or >".into(),
            );
        }
        Ok(())
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let c: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        c.validate()?;
        Ok(c)
    }
}

/// Groups the edit log by intent and targets, keeping first-use order: 「ブラシ ×3」.
pub fn summarize(log: &[IntentLogEntry]) -> Vec<IntentSummary> {
    let mut out: Vec<IntentSummary> = vec![];
    for e in log {
        match out
            .iter_mut()
            .find(|s| s.intent == e.intent && s.targets == e.targets)
        {
            Some(s) => s.count += 1,
            None => out.push(IntentSummary {
                intent: e.intent.clone(),
                targets: e.targets.clone(),
                count: 1,
            }),
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DiffItem {
    Added {
        id: NodeId,
        name: String,
        kind: String,
        parent: NodeId,
    },
    Removed {
        id: NodeId,
        name: String,
        kind: String,
    },
    Moved {
        id: NodeId,
        name: String,
        from: (NodeId, usize),
        to: (NodeId, usize),
    },
    PropsChanged {
        id: NodeId,
        name: String,
        fields: Vec<String>,
    },
    PixelsChanged {
        id: NodeId,
        name: String,
        /// "content" or "mask".
        target: String,
        tiles: usize,
        /// Changed area in layer pixels.
        bounds: IRect,
        /// The same area in document pixels (axis-aligned, after transforms).
        document_bounds: IRect,
    },
    ContentChanged {
        id: NodeId,
        name: String,
        kind: String,
        detail: String,
    },
    AssetChanged {
        id: AssetId,
        change: String,
        kind: String,
    },
    MetaChanged {
        fields: Vec<String>,
    },
    SelectionChanged {
        tiles: usize,
    },
}

fn world(doc: &Document, id: NodeId) -> alto_core::Affine2 {
    let mut chain = vec![];
    let mut next = Some(id);
    while let Some(n) = next.and_then(|n| doc.nodes.get(&n)) {
        chain.push(n.props.transform);
        next = n.parent;
    }
    chain
        .into_iter()
        .rev()
        .fold(alto_core::Affine2::default(), alto_core::Affine2::concat)
}

fn changed_tiles(a: Option<&TileMap>, b: Option<&TileMap>) -> Vec<alto_core::TileKey> {
    let empty = BTreeMap::new();
    let (a, b) = (
        a.map_or(&empty, |m| &m.tiles),
        b.map_or(&empty, |m| &m.tiles),
    );
    a.keys()
        .chain(b.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .copied()
        .collect()
}

fn pixels(
    doc: &Document,
    node: &Node,
    target: &str,
    keys: &[alto_core::TileKey],
) -> Option<DiffItem> {
    if keys.is_empty() {
        return None;
    }
    let t = TILE as i32;
    let bounds = IRect {
        x0: keys.iter().map(|k| k.tx).min()? * t,
        y0: keys.iter().map(|k| k.ty).min()? * t,
        x1: (keys.iter().map(|k| k.tx).max()? + 1) * t,
        y1: (keys.iter().map(|k| k.ty).max()? + 1) * t,
    };
    let mut m = world(doc, node.id);
    if target == "mask" {
        m = if node.props.mask_linked {
            m.concat(node.props.mask_transform)
        } else {
            node.props.mask_transform
        };
    }
    let m = m.0;
    let pts = [
        (bounds.x0, bounds.y0),
        (bounds.x1, bounds.y0),
        (bounds.x0, bounds.y1),
        (bounds.x1, bounds.y1),
    ]
    .map(|(x, y)| {
        let (x, y) = (x as f64, y as f64);
        (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
    });
    let clamp = |v: f64| v.clamp(i32::MIN as f64, i32::MAX as f64);
    Some(DiffItem::PixelsChanged {
        id: node.id,
        name: node.name.clone(),
        target: target.into(),
        tiles: keys.len(),
        bounds,
        document_bounds: IRect {
            x0: clamp(
                pts.iter()
                    .map(|p| p.0)
                    .fold(f64::INFINITY, f64::min)
                    .floor(),
            ) as i32,
            y0: clamp(
                pts.iter()
                    .map(|p| p.1)
                    .fold(f64::INFINITY, f64::min)
                    .floor(),
            ) as i32,
            x1: clamp(
                pts.iter()
                    .map(|p| p.0)
                    .fold(f64::NEG_INFINITY, f64::max)
                    .ceil(),
            ) as i32,
            y1: clamp(
                pts.iter()
                    .map(|p| p.1)
                    .fold(f64::NEG_INFINITY, f64::max)
                    .ceil(),
            ) as i32,
        },
    })
}

fn prop_fields(a: &Node, b: &Node) -> Vec<String> {
    let (p, q): (&LayerProps, &LayerProps) = (&a.props, &b.props);
    let mut f = vec![];
    let mut check = |name: &str, changed: bool| {
        if changed {
            f.push(name.to_string());
        }
    };
    check("name", a.name != b.name);
    check("visible", p.visible != q.visible);
    check("locked", p.locked != q.locked);
    check("opacity", p.opacity != q.opacity);
    check("blend", p.blend != q.blend);
    check("transform", p.transform != q.transform);
    check("mask", p.mask.is_some() != q.mask.is_some());
    check("mask_enabled", p.mask_enabled != q.mask_enabled);
    check("mask_linked", p.mask_linked != q.mask_linked);
    check("mask_transform", p.mask_transform != q.mask_transform);
    check("vector_mask", p.vector_mask != q.vector_mask);
    check("clip_path", p.clip_path != q.clip_path);
    check("clip_to_below", p.clip_to_below != q.clip_to_below);
    check("effects", p.effects != q.effects);
    if let (NodeKind::Group { isolation: x, .. }, NodeKind::Group { isolation: y, .. }) =
        (&a.kind, &b.kind)
    {
        check("isolation", x != y);
    }
    f
}

fn excerpt(s: &str) -> String {
    let mut t: String = s.chars().take(24).collect();
    if s.chars().count() > 24 {
        t.push('…');
    }
    t.replace('\n', "⏎")
}

fn content_change(a: &Node, b: &Node) -> Option<DiffItem> {
    let detail = match (&a.kind, &b.kind) {
        (NodeKind::Vector { paths: x }, NodeKind::Vector { paths: y }) if x != y => {
            let cmds = |p: &[alto_vector::StyledPath]| {
                p.iter().map(|p| p.data.commands.len()).sum::<usize>()
            };
            format!(
                "パス {}本/{}命令 → {}本/{}命令",
                x.len(),
                cmds(x),
                y.len(),
                cmds(y)
            )
        }
        (NodeKind::Text { content: x }, NodeKind::Text { content: y }) if x != y => {
            let mut parts = vec![];
            if x.text != y.text {
                parts.push(format!("「{}」→「{}」", excerpt(&x.text), excerpt(&y.text)));
            }
            if x.style != y.style || x.spans != y.spans {
                parts.push("書式".into());
            }
            if x.writing_mode != y.writing_mode {
                parts.push(match y.writing_mode {
                    alto_text::WritingMode::VerticalRl => "縦組みへ".into(),
                    alto_text::WritingMode::HorizontalTb => "横組みへ".into(),
                });
            }
            if x.frame != y.frame || x.align != y.align {
                parts.push("枠・揃え".into());
            }
            if x.tcy != y.tcy || x.auto_tcy != y.auto_tcy {
                parts.push("縦中横".into());
            }
            parts.join("、")
        }
        (NodeKind::Adjustment { adjustment: x }, NodeKind::Adjustment { adjustment: y })
            if x != y =>
        {
            "調整値".into()
        }
        (NodeKind::Unsupported { raw: x }, NodeKind::Unsupported { raw: y }) if x != y => {
            "未対応ノードの内容".into()
        }
        _ => return None,
    };
    Some(DiffItem::ContentChanged {
        id: b.id,
        name: b.name.clone(),
        kind: b.kind_name().into(),
        detail,
    })
}

fn tree_order(doc: &Document) -> Vec<NodeId> {
    let mut out = vec![];
    let mut stack = vec![doc.root];
    while let Some(id) = stack.pop() {
        out.push(id);
        if let Some(c) = doc.nodes.get(&id).and_then(|n| n.children()) {
            stack.extend(c.iter().rev());
        }
    }
    out
}

/// Children that changed relative order: those outside the longest common subsequence.
fn reordered(a: &[NodeId], b: &[NodeId]) -> BTreeSet<NodeId> {
    let common: BTreeSet<_> = a.iter().filter(|x| b.contains(x)).copied().collect();
    let a: Vec<_> = a.iter().filter(|x| common.contains(x)).copied().collect();
    let b: Vec<_> = b.iter().filter(|x| common.contains(x)).copied().collect();
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut keep = BTreeSet::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            keep.insert(a[i]);
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    common.difference(&keep).copied().collect()
}

/// Semantic diff from `a` to `b`, matching nodes by NodeId. Pixel changes come from
/// tile hashes, so no pixels are read.
pub fn diff(a: &Document, b: &Document) -> Vec<DiffItem> {
    let mut out = vec![];
    let mut meta = vec![];
    if a.meta.width != b.meta.width || a.meta.height != b.meta.height {
        meta.push("size".to_string());
    }
    if a.meta.color != b.meta.color {
        meta.push("color".into());
    }
    if a.meta.depth != b.meta.depth {
        meta.push("depth".into());
    }
    if !meta.is_empty() {
        out.push(DiffItem::MetaChanged { fields: meta });
    }
    let position = |doc: &Document, id: NodeId| -> Option<(NodeId, usize)> {
        let parent = doc.nodes.get(&id)?.parent?;
        let index = doc
            .nodes
            .get(&parent)?
            .children()?
            .iter()
            .position(|c| *c == id)?;
        Some((parent, index))
    };
    let mut moved = BTreeSet::new();
    for id in tree_order(b) {
        if let (Some(x), Some(y)) = (a.nodes.get(&id), b.nodes.get(&id))
            && let (Some(ca), Some(cb)) = (x.children(), y.children())
        {
            moved.extend(reordered(ca, cb));
        }
    }
    for id in tree_order(a) {
        if !b.nodes.contains_key(&id)
            && let Some(n) = a.nodes.get(&id)
            // Report only the top of a removed subtree.
            && n.parent.is_none_or(|p| b.nodes.contains_key(&p))
        {
            out.push(DiffItem::Removed {
                id,
                name: n.name.clone(),
                kind: n.kind_name().into(),
            });
        }
    }
    for id in tree_order(b) {
        let n = &b.nodes[&id];
        let Some(old) = a.nodes.get(&id) else {
            out.push(DiffItem::Added {
                id,
                name: n.name.clone(),
                kind: n.kind_name().into(),
                parent: n.parent.unwrap_or(id),
            });
            continue;
        };
        if let (Some(from), Some(to)) = (position(a, id), position(b, id))
            && (from.0 != to.0 || moved.contains(&id))
        {
            out.push(DiffItem::Moved {
                id,
                name: n.name.clone(),
                from,
                to,
            });
        }
        let fields = prop_fields(old, n);
        if !fields.is_empty() {
            out.push(DiffItem::PropsChanged {
                id,
                name: n.name.clone(),
                fields,
            });
        }
        if old.kind_name() != n.kind_name() {
            out.push(DiffItem::ContentChanged {
                id,
                name: n.name.clone(),
                kind: n.kind_name().into(),
                detail: format!("{} → {}", old.kind_name(), n.kind_name()),
            });
        } else if let (NodeKind::Raster { tiles: x }, NodeKind::Raster { tiles: y }) =
            (&old.kind, &n.kind)
        {
            out.extend(pixels(b, n, "content", &changed_tiles(Some(x), Some(y))));
        } else {
            out.extend(content_change(old, n));
        }
        if let (Some(x), Some(y)) = (&old.props.mask, &n.props.mask) {
            out.extend(pixels(b, n, "mask", &changed_tiles(Some(x), Some(y))));
        }
    }
    for id in a
        .assets
        .keys()
        .chain(b.assets.keys())
        .collect::<BTreeSet<_>>()
    {
        let (x, y) = (a.assets.get(id), b.assets.get(id));
        if x == y {
            continue;
        }
        let kind = match &y.or(x).expect("present in one side").kind {
            AssetKind::EncodedImage { .. } => "image",
            AssetKind::Font { .. } => "font",
            AssetKind::BrushTip => "brush_tip",
            AssetKind::Pattern => "pattern",
        };
        out.push(DiffItem::AssetChanged {
            id: *id,
            change: match (x, y) {
                (None, _) => "added",
                (_, None) => "removed",
                _ => "changed",
            }
            .into(),
            kind: kind.into(),
        });
    }
    let selection = changed_tiles(a.selection.as_ref(), b.selection.as_ref()).len();
    if selection > 0 {
        out.push(DiffItem::SelectionChanged { tiles: selection });
    }
    out
}

fn subtree(doc: &Document, id: NodeId, out: &mut Vec<Node>) {
    if let Some(n) = doc.nodes.get(&id) {
        if let Some(children) = n.children() {
            for c in children {
                subtree(doc, *c, out);
            }
        }
        out.push(n.clone());
    }
}

/// Emits `locked: value` for every node in `doc` that is itself locked.
/// Parents first so an unlock clears a group before its children. Children
/// first so a relock marks children while the parent is still editable.
fn set_lock_flags(doc: &Document, id: NodeId, value: bool, parents_first: bool, ops: &mut Vec<Op>) {
    let Some(node) = doc.nodes.get(&id) else {
        return;
    };
    let emit = |ops: &mut Vec<Op>| {
        if node.props.locked {
            ops.push(Op::SetProps {
                id,
                patch: PropPatch {
                    locked: Some(value),
                    ..Default::default()
                },
            });
        }
    };
    if parents_first {
        emit(ops);
    }
    if let Some(children) = node.children() {
        for child in children {
            set_lock_flags(doc, *child, value, parents_first, ops);
        }
    }
    if !parents_first {
        emit(ops);
    }
}

/// Operations that turn `current` into `target` in one undoable transaction.
///
/// Locked nodes reject edits, so the sequence unlocks the current tree before
/// rebuilding it and writes the target lock flags last. Undo then clears those
/// flags before it puts the previous tree back.
pub fn restore_ops(current: &Document, target: &Document) -> Result<Vec<Op>, String> {
    if current.root != target.root {
        return Err("the version belongs to a different document lineage".into());
    }
    let (Some(cur_root), Some(tgt_root)) = (
        current.nodes.get(&current.root),
        target.nodes.get(&target.root),
    ) else {
        return Err("missing root".into());
    };
    let mut ops = Vec::new();
    set_lock_flags(current, current.root, false, true, &mut ops);
    ops.extend(
        cur_root
            .children()
            .unwrap_or(&[])
            .iter()
            .map(|id| Op::RemoveNode { id: *id }),
    );
    let p = &tgt_root.props;
    ops.push(Op::SetProps {
        id: target.root,
        patch: PropPatch {
            name: Some(tgt_root.name.clone()),
            visible: Some(p.visible),
            locked: Some(false),
            opacity: Some(p.opacity),
            blend: Some(p.blend),
            transform: Some(p.transform),
            clip_to_below: Some(p.clip_to_below),
            mask_enabled: Some(p.mask_enabled),
            mask_linked: Some(p.mask_linked),
            mask_transform: Some(p.mask_transform),
            effects: Some(p.effects.clone()),
        },
    });
    ops.push(Op::SetMask {
        layer: target.root,
        mask: p.mask.clone(),
    });
    ops.push(Op::SetVectorMask {
        id: target.root,
        mask: p.vector_mask.as_deref().cloned(),
    });
    ops.push(Op::SetClipPath {
        id: target.root,
        path: p.clip_path.as_deref().cloned(),
    });
    for id in current
        .assets
        .keys()
        .chain(target.assets.keys())
        .collect::<BTreeSet<_>>()
    {
        if current.assets.get(id) != target.assets.get(id) {
            ops.push(Op::SetAsset {
                id: *id,
                record: target.assets.get(id).cloned(),
            });
        }
    }
    if current.meta != target.meta {
        ops.push(Op::SetMeta {
            meta: target.meta.clone(),
        });
    }
    if current.selection != target.selection {
        ops.push(Op::SetSelection {
            selection: target.selection.clone(),
        });
    }
    for (index, child) in tgt_root.children().unwrap_or(&[]).iter().enumerate() {
        let mut nodes = vec![];
        subtree(target, *child, &mut nodes);
        for node in &mut nodes {
            node.props.locked = false;
        }
        ops.push(Op::RestoreSubtree {
            nodes,
            parent: target.root,
            index,
        });
    }
    set_lock_flags(target, target.root, true, false, &mut ops);
    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetRecord, DocumentMeta, Editor, Op, TileChange};
    use alto_core::TileKey;
    use alto_text::{TextContent, TextStyle, WritingMode};

    fn editor() -> Editor {
        Editor::new(Document::new(DocumentMeta::srgb_u8(1024, 768)).unwrap())
    }
    fn tx(e: &mut Editor, intent: &str, ops: Vec<Op>) {
        let rev = e.doc.revision;
        e.transact(intent, ops, rev).unwrap();
    }
    fn font(id: AssetId) -> Op {
        Op::SetAsset {
            id,
            record: Some(AssetRecord {
                id,
                hash: ContentHash::of(b"font"),
                kind: AssetKind::Font {
                    family: "Test".into(),
                    face_index: 0,
                    embedding: Default::default(),
                },
                byte_len: 4,
                dims: None,
            }),
        }
    }

    #[test]
    fn text_nodes_validate_fonts_and_undo_text_edits() {
        let mut e = editor();
        let root = e.doc.root;
        let f = AssetId::new();
        let content = TextContent {
            text: "縦書き".into(),
            style: TextStyle {
                fonts: vec![f],
                ..Default::default()
            },
            writing_mode: WritingMode::VerticalRl,
            ..Default::default()
        };
        let node = Node::text("見出し", content.clone());
        let id = node.id;
        let insert = Op::InsertNode {
            node,
            parent: root,
            index: 0,
        };
        let rev = e.doc.revision;
        assert!(
            e.transact("text", vec![insert.clone()], rev).is_err(),
            "font asset required"
        );
        tx(&mut e, "text", vec![font(f), insert]);
        let bytes = e.doc.snapshot_bytes().unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("\"writing_mode\": \"vertical_rl\""));
        let mut edited = content.clone();
        edited.text = "横書き".into();
        tx(
            &mut e,
            "edit",
            vec![Op::SetText {
                id,
                content: Box::new(edited),
            }],
        );
        e.undo().unwrap();
        let NodeKind::Text { content: now } = &e.doc.node(id).unwrap().kind else {
            panic!()
        };
        assert_eq!(**now, content);
        let mut bad = content.clone();
        bad.style.size = 0.0;
        let rev = e.doc.revision;
        assert!(
            e.transact(
                "bad",
                vec![Op::SetText {
                    id,
                    content: Box::new(bad)
                }],
                rev
            )
            .is_err()
        );
        // The font cannot be removed while text uses it.
        let rev = e.doc.revision;
        assert!(
            e.transact(
                "rm",
                vec![Op::SetAsset {
                    id: f,
                    record: None
                }],
                rev
            )
            .is_err()
        );
        assert_eq!(
            Document::from_snapshot_bytes(&bytes)
                .unwrap()
                .snapshot_bytes()
                .unwrap(),
            bytes
        );
    }

    #[test]
    fn intent_log_summarises_and_follows_undo() {
        let mut e = editor();
        let root = e.doc.root;
        let layer = Node::raster("背景");
        let id = layer.id;
        tx(
            &mut e,
            "レイヤー追加",
            vec![Op::InsertNode {
                node: layer,
                parent: root,
                index: 0,
            }],
        );
        for _ in 0..3 {
            tx(
                &mut e,
                "ブラシ",
                vec![Op::SetProps {
                    id,
                    patch: PropPatch {
                        opacity: Some(0.5),
                        ..Default::default()
                    },
                }],
            );
        }
        e.undo().unwrap();
        let s = summarize(e.intent_log());
        assert_eq!(s.len(), 2);
        assert_eq!(
            (s[1].intent.as_str(), s[1].count, s[1].targets.clone()),
            ("ブラシ", 2, vec![id])
        );
        e.take_intent_log();
        e.undo().unwrap();
        assert_eq!(e.intent_log()[0].intent, "取り消し: ブラシ");
    }

    #[test]
    fn semantic_diff_reports_layers_pixels_moves_and_text() {
        let mut e = editor();
        let root = e.doc.root;
        let (a, b, g) = (
            Node::raster("背景"),
            Node::raster("線画"),
            Node::group("グループ"),
        );
        let (ia, ib, ig) = (a.id, b.id, g.id);
        let f = AssetId::new();
        let t = Node::text(
            "本文",
            TextContent {
                text: "吾輩は猫である".into(),
                style: TextStyle {
                    fonts: vec![f],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let it = t.id;
        tx(
            &mut e,
            "setup",
            vec![
                font(f),
                Op::InsertNode {
                    node: a,
                    parent: root,
                    index: 0,
                },
                Op::InsertNode {
                    node: b,
                    parent: root,
                    index: 1,
                },
                Op::InsertNode {
                    node: g,
                    parent: root,
                    index: 2,
                },
                Op::InsertNode {
                    node: t,
                    parent: root,
                    index: 3,
                },
            ],
        );
        let before = e.doc.clone();
        let hash = ContentHash::of(b"pixels");
        tx(
            &mut e,
            "edit",
            vec![
                Op::SetTiles {
                    layer: ia,
                    changes: vec![
                        TileChange {
                            key: TileKey { tx: 1, ty: 0 },
                            before: None,
                            after: Some(hash),
                        },
                        TileChange {
                            key: TileKey { tx: 2, ty: 1 },
                            before: None,
                            after: Some(hash),
                        },
                    ],
                },
                Op::SetProps {
                    id: ia,
                    patch: PropPatch {
                        transform: Some(alto_core::Affine2([2., 0., 0., 2., 10., 0.])),
                        ..Default::default()
                    },
                },
                Op::MoveNode {
                    id: ib,
                    parent: ig,
                    index: 0,
                },
                Op::SetProps {
                    id: ig,
                    patch: PropPatch {
                        opacity: Some(0.5),
                        name: Some("人物".into()),
                        ..Default::default()
                    },
                },
                Op::SetText {
                    id: it,
                    content: Box::new(TextContent {
                        text: "吾輩は犬である".into(),
                        style: TextStyle {
                            fonts: vec![f],
                            ..Default::default()
                        },
                        writing_mode: alto_text::WritingMode::VerticalRl,
                        ..Default::default()
                    }),
                },
            ],
        );
        let mut extra = Node::raster("追加");
        extra.props.visible = false;
        let ie = extra.id;
        tx(
            &mut e,
            "add",
            vec![
                Op::InsertNode {
                    node: extra,
                    parent: root,
                    index: 0,
                },
                Op::RemoveNode { id: it },
            ],
        );
        let d = diff(&before, &e.doc);
        let has = |p: &dyn Fn(&DiffItem) -> bool| d.iter().any(p);
        assert!(
            has(
                &|i| matches!(i, DiffItem::PixelsChanged { id, tiles: 2, bounds, document_bounds, .. }
            if *id == ia && *bounds == IRect { x0: 256, y0: 0, x1: 768, y1: 512 }
            && *document_bounds == IRect { x0: 522, y0: 0, x1: 1546, y1: 1024 })
            ),
            "{d:#?}"
        );
        assert!(has(
            &|i| matches!(i, DiffItem::PropsChanged { id, fields, .. } if *id == ia && fields == &["transform"])
        ));
        assert!(has(
            &|i| matches!(i, DiffItem::Moved { id, from, to, .. } if *id == ib && from.0 == root && to == &(ig, 0))
        ));
        assert!(has(
            &|i| matches!(i, DiffItem::PropsChanged { id, fields, name } if *id == ig && name == "人物" && fields == &["name", "opacity"])
        ));
        assert!(has(
            &|i| matches!(i, DiffItem::Added { id, kind, .. } if *id == ie && kind == "raster")
        ));
        assert!(has(
            &|i| matches!(i, DiffItem::Removed { id, kind, .. } if *id == it && kind == "text")
        ));
        assert!(
            !has(&|i| matches!(i, DiffItem::Moved { id, .. } if *id == ia)),
            "insertion is not a move"
        );
        // Text change detail is readable.
        let mid = {
            let mut m = before.clone();
            let n = m.nodes.get_mut(&it).unwrap();
            if let NodeKind::Text { content } = &mut n.kind {
                content.text = "吾輩は犬である".into();
                content.writing_mode = alto_text::WritingMode::VerticalRl;
            }
            m
        };
        let d = diff(&before, &mid);
        assert!(
            matches!(&d[0], DiffItem::ContentChanged { detail, .. } if detail.contains("「吾輩は猫である」→「吾輩は犬である」") && detail.contains("縦組みへ")),
            "{d:?}"
        );
        assert!(diff(&e.doc, &e.doc).is_empty());
        // Reordering within a group reports only the moved node.
        let mut r = e.doc.clone();
        let children = match &mut r.nodes.get_mut(&root).unwrap().kind {
            NodeKind::Group { children, .. } => children,
            _ => unreachable!(),
        };
        let first = children.remove(0);
        children.push(first);
        let d = diff(&e.doc, &r);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(matches!(&d[0], DiffItem::Moved { id, .. } if *id == ie));
    }

    #[test]
    fn restore_ops_recreate_an_earlier_version_and_undo() {
        let mut e = editor();
        let root = e.doc.root;
        let a = Node::raster("A");
        let ia = a.id;
        tx(
            &mut e,
            "a",
            vec![Op::InsertNode {
                node: a,
                parent: root,
                index: 0,
            }],
        );
        let v1 = e.doc.clone();
        tx(
            &mut e,
            "b",
            vec![
                Op::InsertNode {
                    node: Node::group("G"),
                    parent: root,
                    index: 1,
                },
                Op::SetProps {
                    id: ia,
                    patch: PropPatch {
                        opacity: Some(0.25),
                        ..Default::default()
                    },
                },
                Op::SetProps {
                    id: root,
                    patch: PropPatch {
                        name: Some("R".into()),
                        ..Default::default()
                    },
                },
            ],
        );
        let v2 = e.doc.clone();
        let ops = restore_ops(&e.doc, &v1).unwrap();
        tx(&mut e, "restore", ops);
        assert!(diff(&v1, &e.doc).is_empty());
        assert_eq!(
            e.doc.snapshot_bytes().unwrap(),
            v1.snapshot_bytes().unwrap()
        );
        e.undo().unwrap();
        assert_eq!(
            e.doc.snapshot_bytes().unwrap(),
            v2.snapshot_bytes().unwrap()
        );
        let other = Document::new(DocumentMeta::srgb_u8(8, 8)).unwrap();
        assert!(restore_ops(&e.doc, &other).is_err());
    }

    #[test]
    fn restore_ops_rebuild_a_locked_tree_and_undo() {
        let mut e = editor();
        let root = e.doc.root;
        let a = Node::raster("A");
        let ia = a.id;
        tx(
            &mut e,
            "a",
            vec![Op::InsertNode {
                node: a,
                parent: root,
                index: 0,
            }],
        );
        let v1 = e.doc.clone();
        tx(
            &mut e,
            "lock",
            vec![
                Op::SetProps {
                    id: ia,
                    patch: PropPatch {
                        locked: Some(true),
                        ..Default::default()
                    },
                },
                Op::SetProps {
                    id: root,
                    patch: PropPatch {
                        locked: Some(true),
                        ..Default::default()
                    },
                },
            ],
        );
        let v2 = e.doc.clone();
        let rev = e.doc.revision;
        assert!(
            e.transact(
                "blocked",
                vec![Op::SetTiles {
                    layer: ia,
                    changes: vec![],
                }],
                rev,
            )
            .is_err()
        );
        let ops = restore_ops(&e.doc, &v1).unwrap();
        tx(&mut e, "restore", ops);
        assert_eq!(
            e.doc.snapshot_bytes().unwrap(),
            v1.snapshot_bytes().unwrap()
        );
        e.undo().unwrap();
        assert_eq!(
            e.doc.snapshot_bytes().unwrap(),
            v2.snapshot_bytes().unwrap()
        );
    }

    #[test]
    fn commit_bytes_are_canonical() {
        let c = Commit {
            parents: vec![],
            snapshot: ContentHash::of(b"s"),
            message: "最初の版".into(),
            intents: vec![IntentSummary {
                intent: "ブラシ".into(),
                targets: vec![],
                count: 3,
            }],
            created_at: 1_760_000_000_000,
            author: None,
        };
        let b = c.bytes();
        assert_eq!(Commit::parse(&b).unwrap(), c);
        assert_eq!(Commit::parse(&b).unwrap().hash(), c.hash());
        assert!(
            String::from_utf8(b).unwrap().lines().count() > 5,
            "line-oriented"
        );
        let mut bad = c.clone();
        bad.message = "x\u{7}".into();
        assert!(bad.validate().is_err());
        bad = c.clone();
        bad.author = Some("Mallory\nreset refs/heads/main".into());
        assert!(bad.validate().is_err());
        bad.author = Some("A <injected>".into());
        assert!(bad.validate().is_err());
    }
}
