//! Deliberately small SVG importer. No unsupported element or attribute is silently dropped.
use crate::{DocHandle, Engine, EngineError};
use alto_core::{Affine2, NodeId};
use alto_doc::{Isolation, Node, NodeKind, Op};
use alto_vector::{
    ClipPath, FillRule, LineCap, LineJoin, PathCommand, PathData, Stroke, StyledPath,
};
use roxmltree::{Document as Xml, Node as XmlNode};
use serde::Serialize;
fn error(n: XmlNode, detail: impl std::fmt::Display) -> EngineError {
    let pos = n.document().text_pos_at(n.range().start);
    EngineError::Operation {
        code: "UNSUPPORTED",
        message: format!(
            "{}: {} at byte {}, line {}, column {}",
            n.tag_name().name(),
            detail,
            n.range().start,
            pos.row,
            pos.col
        ),
    }
}
fn number(n: XmlNode, s: &str) -> Result<f64, EngineError> {
    let v = s
        .trim()
        .parse::<f64>()
        .map_err(|_| error(n, format!("invalid number {s}")))?;
    if !v.is_finite() || v.abs() > 1048576.0 {
        return Err(error(n, "number outside coordinate limits"));
    }
    Ok(v)
}
fn length(n: XmlNode, key: &str, default: f64) -> Result<f64, EngineError> {
    n.attribute(key)
        .map(|s| number(n, s.strip_suffix("px").unwrap_or(s)))
        .unwrap_or(Ok(default))
}
fn unit(n: XmlNode, key: &str, default: f64) -> Result<f64, EngineError> {
    let v = n
        .attribute(key)
        .map(|s| number(n, s))
        .unwrap_or(Ok(default))?;
    if !(0.0..=1.0).contains(&v) {
        return Err(error(n, format!("invalid {key}")));
    }
    Ok(v)
}
fn paint(n: XmlNode, s: &str) -> Result<Option<[f32; 4]>, EngineError> {
    if s == "none" {
        return Ok(None);
    }
    let c = match s {
        "black" => [0, 0, 0],
        "white" => [255, 255, 255],
        _ => {
            let h = s
                .strip_prefix('#')
                .ok_or_else(|| error(n, "unsupported paint"))?;
            if !h.is_ascii() || ![3, 6].contains(&h.len()) {
                return Err(error(n, "unsupported color"));
            }
            let h = if h.len() == 3 {
                h.chars().flat_map(|c| [c, c]).collect::<String>()
            } else {
                h.to_string()
            };
            let v = u32::from_str_radix(&h, 16).map_err(|_| error(n, "invalid color"))?;
            [(v >> 16) as u8, (v >> 8) as u8, v as u8]
        }
    };
    Ok(Some([
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
        1.0,
    ]))
}
fn numbers(n: XmlNode, s: &str) -> Result<Vec<f64>, EngineError> {
    svgtypes::NumberListParser::from(s)
        .map(|v| {
            let v = v.map_err(|e| error(n, e))?;
            if !v.is_finite() || v.abs() > 1048576.0 {
                Err(error(n, "coordinate out of range"))
            } else {
                Ok(v)
            }
        })
        .collect()
}
fn transform(n: XmlNode) -> Result<Affine2, EngineError> {
    let t = n
        .attribute("transform")
        .unwrap_or("")
        .parse::<svgtypes::Transform>()
        .map_err(|e| error(n, e))?;
    let m = Affine2([t.a, t.b, t.c, t.d, t.e, t.f]);
    if !m.is_finite() || alto_core::FixedAffine::from_affine(m).is_none() {
        return Err(error(n, "transform out of range"));
    }
    Ok(m)
}
#[derive(Clone)]
struct Style {
    fill: Option<[f32; 4]>,
    stroke: Option<[f32; 4]>,
    fill_opacity: f64,
    stroke_opacity: f64,
    width: f32,
    cap: LineCap,
    join: LineJoin,
    miter: f32,
    rule: FillRule,
}
impl Default for Style {
    fn default() -> Self {
        Self {
            fill: Some([0.0, 0.0, 0.0, 1.0]),
            stroke: None,
            fill_opacity: 1.0,
            stroke_opacity: 1.0,
            width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter: 4.0,
            rule: FillRule::NonZero,
        }
    }
}
fn style(n: XmlNode, mut s: Style) -> Result<Style, EngineError> {
    if let Some(v) = n.attribute("fill") {
        s.fill = paint(n, v)?;
    }
    if let Some(v) = n.attribute("stroke") {
        s.stroke = paint(n, v)?;
    }
    s.fill_opacity = unit(n, "fill-opacity", s.fill_opacity)?;
    s.stroke_opacity = unit(n, "stroke-opacity", s.stroke_opacity)?;
    s.width = length(n, "stroke-width", s.width as f64)? as f32;
    if s.width < 0.0 {
        return Err(error(n, "negative stroke width"));
    }
    s.miter = length(n, "stroke-miterlimit", s.miter as f64)? as f32;
    if let Some(v) = n.attribute("stroke-linecap") {
        s.cap = match v {
            "butt" => LineCap::Butt,
            "round" => LineCap::Round,
            "square" => LineCap::Square,
            _ => return Err(error(n, "unsupported linecap")),
        };
    }
    if let Some(v) = n.attribute("stroke-linejoin") {
        s.join = match v {
            "miter" => LineJoin::Miter,
            "round" => LineJoin::Round,
            "bevel" => LineJoin::Bevel,
            _ => return Err(error(n, "unsupported linejoin")),
        };
    }
    if let Some(v) = n.attribute("fill-rule") {
        s.rule = match v {
            "nonzero" => FillRule::NonZero,
            "evenodd" => FillRule::EvenOdd,
            _ => return Err(error(n, "unsupported fill-rule")),
        };
    }
    Ok(s)
}
fn validate_attrs(n: XmlNode) -> Result<(), EngineError> {
    let name = n.tag_name().name();
    let extra: &[&str] = match name {
        "svg" => &["width", "height", "viewBox", "preserveAspectRatio"],
        "g" => &[],
        "path" => &["d"],
        "rect" => &["x", "y", "width", "height", "rx", "ry"],
        "circle" => &["cx", "cy", "r"],
        "ellipse" => &["cx", "cy", "rx", "ry"],
        "line" => &["x1", "y1", "x2", "y2"],
        "polyline" | "polygon" => &["points"],
        _ => return Err(error(n, "unsupported element")),
    };
    if n.tag_name()
        .namespace()
        .is_some_and(|ns| ns != "http://www.w3.org/2000/svg")
    {
        return Err(error(n, "unsupported namespace"));
    }
    for ns in n.namespaces() {
        if ns.uri() != "http://www.w3.org/2000/svg" {
            return Err(error(n, "unsupported namespace declaration"));
        }
    }
    for a in n.attributes() {
        if a.namespace().is_some()
            || (![
                "id",
                "fill",
                "stroke",
                "fill-opacity",
                "stroke-opacity",
                "stroke-width",
                "stroke-linecap",
                "stroke-linejoin",
                "stroke-miterlimit",
                "fill-rule",
                "opacity",
                "transform",
            ]
            .contains(&a.name())
                && !extra.contains(&a.name()))
        {
            return Err(error(n, format!("unsupported attribute {}", a.name())));
        }
    }
    Ok(())
}
/// Scale the parsed path by ten before cubic normalization: svgtypes' 0.1 tolerance
/// then becomes 0.01 in local coordinates. Validate after scaling back.
fn scaled_path(n: XmlNode, s: &str) -> Result<String, EngineError> {
    if s.len() > 262144 {
        return Err(error(n, "path data too large"));
    }
    let mut scaled = String::new();
    let mut count = 0;
    use svgtypes::PathSegment as S;
    for segment in svgtypes::PathParser::from(s) {
        count += 1;
        if count > 4096 {
            return Err(error(n, "too many path commands"));
        }
        let (abs, cmd, values): (bool, char, Vec<f64>) = match segment.map_err(|e| error(n, e))? {
            S::MoveTo { abs, x, y } => (abs, 'M', vec![x * 10.0, y * 10.0]),
            S::LineTo { abs, x, y } => (abs, 'L', vec![x * 10.0, y * 10.0]),
            S::HorizontalLineTo { abs, x } => (abs, 'H', vec![x * 10.0]),
            S::VerticalLineTo { abs, y } => (abs, 'V', vec![y * 10.0]),
            S::CurveTo {
                abs,
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => (
                abs,
                'C',
                vec![
                    x1 * 10.0,
                    y1 * 10.0,
                    x2 * 10.0,
                    y2 * 10.0,
                    x * 10.0,
                    y * 10.0,
                ],
            ),
            S::SmoothCurveTo { abs, x2, y2, x, y } => {
                (abs, 'S', vec![x2 * 10.0, y2 * 10.0, x * 10.0, y * 10.0])
            }
            S::Quadratic { abs, x1, y1, x, y } => {
                (abs, 'Q', vec![x1 * 10.0, y1 * 10.0, x * 10.0, y * 10.0])
            }
            S::SmoothQuadratic { abs, x, y } => (abs, 'T', vec![x * 10.0, y * 10.0]),
            S::EllipticalArc {
                abs,
                rx,
                ry,
                x_axis_rotation,
                large_arc,
                sweep,
                x,
                y,
            } => (
                abs,
                'A',
                vec![
                    rx * 10.0,
                    ry * 10.0,
                    x_axis_rotation,
                    large_arc as u8 as f64,
                    sweep as u8 as f64,
                    x * 10.0,
                    y * 10.0,
                ],
            ),
            S::ClosePath { abs } => (abs, 'Z', vec![]),
        };
        if values
            .iter()
            .any(|v| !v.is_finite() || v.abs() > 10485760.0)
        {
            return Err(error(n, "path coordinate out of range"));
        }
        scaled.push(if abs { cmd } else { cmd.to_ascii_lowercase() });
        for v in values {
            // Arc flags use the single-character SVG grammar, not an exponent.
            if v == 0.0 || v == 1.0 {
                scaled.push_str(&format!(" {v}"));
            } else {
                scaled.push_str(&format!(" {v:e}"));
            }
        }
    }
    Ok(scaled)
}
fn path(n: XmlNode, s: &str) -> Result<PathData, EngineError> {
    let scaled = scaled_path(n, s)?;
    let mut commands = vec![];
    use svgtypes::SimplePathSegment as P;
    for segment in svgtypes::SimplifyingPathParser::from(scaled.as_str()) {
        commands.push(match segment.map_err(|e| error(n, e))? {
            P::MoveTo { x, y } => PathCommand::Move {
                x: x / 10.0,
                y: y / 10.0,
            },
            P::LineTo { x, y } => PathCommand::Line {
                x: x / 10.0,
                y: y / 10.0,
            },
            P::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => PathCommand::Cubic {
                c1x: x1 / 10.0,
                c1y: y1 / 10.0,
                c2x: x2 / 10.0,
                c2y: y2 / 10.0,
                x: x / 10.0,
                y: y / 10.0,
            },
            P::Quadratic { x1, y1, x, y } => PathCommand::Quad {
                cx: x1 / 10.0,
                cy: y1 / 10.0,
                x: x / 10.0,
                y: y / 10.0,
            },
            P::ClosePath => PathCommand::Close,
        });
        if commands.len() > 4096 {
            return Err(error(n, "too many normalized commands"));
        }
    }
    let p = PathData { commands };
    p.validate().map_err(|e| error(n, e))?;
    Ok(p)
}
fn shape(n: XmlNode) -> Result<PathData, EngineError> {
    let d = match n.tag_name().name() {
        "path" => n.attribute("d").unwrap_or("").to_string(),
        "line" => format!(
            "M{:e} {:e}L{:e} {:e}",
            length(n, "x1", 0.0)?,
            length(n, "y1", 0.0)?,
            length(n, "x2", 0.0)?,
            length(n, "y2", 0.0)?
        ),
        "polyline" | "polygon" => {
            let pts = numbers(n, n.attribute("points").unwrap_or(""))?;
            if pts.len() % 2 != 0 {
                return Err(error(n, "odd point coordinate count"));
            }
            let mut d = String::new();
            for (i, p) in pts.chunks(2).enumerate() {
                d.push_str(&format!(
                    "{}{:e} {:e}",
                    if i == 0 { "M" } else { "L" },
                    p[0],
                    p[1]
                ));
            }
            if n.tag_name().name() == "polygon" && !pts.is_empty() {
                d.push('Z');
            }
            d
        }
        "circle" | "ellipse" => {
            let cx = length(n, "cx", 0.0)?;
            let cy = length(n, "cy", 0.0)?;
            let r = length(n, "r", 0.0)?;
            let rx = length(n, "rx", r)?;
            let ry = length(n, "ry", r)?;
            if rx < 0.0 || ry < 0.0 {
                return Err(error(n, "negative radius"));
            }
            if rx == 0.0 || ry == 0.0 {
                String::new()
            } else {
                format!(
                    "M{:e} {:e} A{rx:e} {ry:e} 0 1 0 {:e} {:e} A{rx:e} {ry:e} 0 1 0 {:e} {:e} Z",
                    cx + rx,
                    cy,
                    cx - rx,
                    cy,
                    cx + rx,
                    cy
                )
            }
        }
        "rect" => {
            let x = length(n, "x", 0.0)?;
            let y = length(n, "y", 0.0)?;
            let w = length(n, "width", 0.0)?;
            let h = length(n, "height", 0.0)?;
            let rx = length(n, "rx", length(n, "ry", 0.0)?)?;
            let ry = length(n, "ry", rx)?;
            if w < 0.0 || h < 0.0 || rx < 0.0 || ry < 0.0 {
                return Err(error(n, "negative rect dimension"));
            }
            let rx = rx.min(w / 2.0);
            let ry = ry.min(h / 2.0);
            if w == 0.0 || h == 0.0 {
                String::new()
            } else if rx == 0.0 || ry == 0.0 {
                format!("M{x:e} {y:e}h{w:e}v{h:e}h{:e}Z", -w)
            } else {
                format!(
                    "M{:e} {y:e}H{:e}A{rx:e} {ry:e} 0 0 1 {:e} {:e}V{:e}A{rx:e} {ry:e} 0 0 1 {:e} {:e}H{:e}A{rx:e} {ry:e} 0 0 1 {x:e} {:e}V{:e}A{rx:e} {ry:e} 0 0 1 {:e} {y:e}Z",
                    x + rx,
                    x + w - rx,
                    x + w,
                    y + ry,
                    y + h - ry,
                    x + w - rx,
                    y + h,
                    x + rx,
                    y + h - ry,
                    y + ry,
                    x + rx
                )
            }
        }
        _ => return Err(error(n, "unsupported shape")),
    };
    path(n, &d)
}
#[derive(Clone, Debug, Serialize)]
pub struct SvgImportResult {
    pub node: NodeId,
    pub nodes: usize,
}
pub struct PreparedSvgImport {
    pub ops: Vec<Op>,
    pub revision: u64,
    pub result: SvgImportResult,
}
struct Builder {
    ops: Vec<Op>,
    nodes: usize,
    commands: usize,
}
impl Builder {
    fn push(&mut self, node: Node, parent: NodeId, index: usize) -> Result<NodeId, EngineError> {
        self.nodes += 1;
        if self.nodes > 10000 {
            return Err(EngineError::Operation {
                code: "RESOURCE_LIMIT",
                message: "SVG node limit".into(),
            });
        }
        let id = node.id;
        self.ops.push(Op::InsertNode {
            node,
            parent,
            index,
        });
        Ok(id)
    }
    fn children(
        &mut self,
        n: XmlNode,
        parent: NodeId,
        s: Style,
        depth: usize,
    ) -> Result<(), EngineError> {
        if depth > 64 {
            return Err(error(n, "SVG depth limit"));
        }
        let mut index = 0;
        for child in n.children() {
            if child.is_comment() {
                continue;
            }
            if child.is_text() {
                if child.text().unwrap_or("").trim().is_empty() {
                    continue;
                }
                return Err(error(n, "text content is unsupported"));
            }
            if !child.is_element() {
                return Err(error(n, "processing instructions are unsupported"));
            }
            validate_attrs(child)?;
            if child.tag_name().name() == "svg" {
                return Err(error(child, "nested svg is unsupported"));
            }
            let s = style(child, s.clone())?;
            let opacity = unit(child, "opacity", 1.0)? as f32;
            let mut node = if child.tag_name().name() == "g" {
                let mut n = Node::group(child.attribute("id").unwrap_or("SVG group"));
                n.kind = NodeKind::Group {
                    children: vec![],
                    isolation: Isolation::Isolated,
                };
                n
            } else {
                if child.children().any(|c| {
                    c.is_element()
                        || c.is_pi()
                        || (c.is_text() && !c.text().unwrap_or("").trim().is_empty())
                }) {
                    return Err(error(child, "shape children are unsupported"));
                }
                let data = shape(child)?;
                self.commands += data.commands.len();
                if self.commands > 65536 {
                    return Err(error(child, "total path command limit"));
                }
                let fill = s.fill.map(|mut c| {
                    c[3] = s.fill_opacity as f32;
                    c
                });
                let stroke = s.stroke.filter(|_| s.width > 0.0).map(|mut c| {
                    c[3] = s.stroke_opacity as f32;
                    Stroke {
                        color: c,
                        width: s.width,
                        cap: s.cap,
                        join: s.join,
                        miter_limit: s.miter,
                    }
                });
                let paths = vec![StyledPath {
                    data,
                    fill,
                    stroke,
                    fill_rule: s.rule,
                }];
                alto_vector::validate_paths(&paths).map_err(|e| error(child, e))?;
                Node::vector(
                    child.attribute("id").unwrap_or(child.tag_name().name()),
                    paths,
                )
            };
            node.props.opacity = opacity;
            node.props.transform = transform(child)?;
            let id = self.push(node, parent, index)?;
            index += 1;
            if child.tag_name().name() == "g" {
                self.children(child, id, s, depth + 1)?;
            }
        }
        Ok(())
    }
}
impl Engine {
    pub fn prepare_svg_import(
        &self,
        h: DocHandle,
        svg: &str,
        parent: NodeId,
        index: usize,
    ) -> Result<PreparedSvgImport, EngineError> {
        if svg.len() > 4 * 1024 * 1024 {
            return Err(EngineError::Operation {
                code: "RESOURCE_LIMIT",
                message: "SVG exceeds 4 MiB".into(),
            });
        }
        let xml = Xml::parse_with_options(
            svg,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: 30000,
            },
        )
        .map_err(|e| EngineError::Operation {
            code: "UNSUPPORTED",
            message: format!("SVG XML at {}: {e}", e.pos()),
        })?;
        let root = xml.root_element();
        validate_attrs(root)?;
        if root.tag_name().name() != "svg" {
            return Err(error(root, "svg root required"));
        }
        if xml.root().children().any(|n| n.is_pi()) {
            return Err(error(root, "processing instructions are unsupported"));
        }
        if root.descendants().filter(|n| n.is_element()).count() > 10000 {
            return Err(error(root, "element limit"));
        }
        let vb = root
            .attribute("viewBox")
            .map(|s| numbers(root, s))
            .transpose()?;
        if vb
            .as_ref()
            .is_some_and(|v| v.len() != 4 || v[2] <= 0.0 || v[3] <= 0.0)
        {
            return Err(error(root, "invalid viewBox"));
        }
        let w = length(root, "width", vb.as_ref().map_or(0.0, |v| v[2]))?;
        let ht = length(root, "height", vb.as_ref().map_or(0.0, |v| v[3]))?;
        if w <= 0.0 || ht <= 0.0 {
            return Err(error(root, "positive viewport dimensions required"));
        }
        let aspect = root
            .attribute("preserveAspectRatio")
            .unwrap_or("xMidYMid meet");
        if !["none", "xMidYMid", "xMidYMid meet"].contains(&aspect) {
            return Err(error(root, "unsupported preserveAspectRatio"));
        }
        let viewport = if let Some(v) = vb {
            let (mut sx, mut sy) = (w / v[2], ht / v[3]);
            if aspect != "none" {
                sx = sx.min(sy);
                sy = sx;
            }
            Affine2([
                sx,
                0.0,
                0.0,
                sy,
                (w - v[2] * sx) / 2.0 - v[0] * sx,
                (ht - v[3] * sy) / 2.0 - v[1] * sy,
            ])
        } else {
            Affine2::default()
        };
        let mut container = Node::group(root.attribute("id").unwrap_or("SVG"));
        container.kind = NodeKind::Group {
            children: vec![],
            isolation: Isolation::Isolated,
        };
        container.props.opacity = unit(root, "opacity", 1.0)? as f32;
        container.props.transform = transform(root)?;
        container.props.clip_path = Some(Box::new(ClipPath {
            path: path(root, &format!("M0 0H{w}V{ht}H0Z"))?,
            fill_rule: FillRule::NonZero,
            enabled: true,
            transform: Affine2::default(),
        }));
        let mut builder = Builder {
            ops: vec![],
            nodes: 0,
            commands: 0,
        };
        let id = builder.push(container, parent, index)?;
        let mut content = Node::group("SVG content");
        content.props.transform = viewport;
        let content_id = builder.push(content, id, 0)?;
        builder.children(root, content_id, style(root, Style::default())?, 1)?;
        let doc = self.document(h)?;
        let revision = doc.revision;
        let mut check = alto_doc::Editor::new(doc.clone());
        check.transact("image.import_svg", builder.ops.clone(), revision)?;
        Ok(PreparedSvgImport {
            ops: builder.ops,
            revision,
            result: SvgImportResult {
                node: id,
                nodes: builder.nodes,
            },
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use alto_doc::DocumentMeta;
    #[test]
    fn extreme_small_coordinates_have_bounded_temporary_paths() {
        let xml = Xml::parse("<path/>").unwrap();
        let n = xml.root_element();
        let source = "M1e-300 5e-324".to_owned() + &" L1e-300 5e-324".repeat(4000);
        let normalized = scaled_path(n, &source).unwrap();
        assert!(normalized.len() < source.len() * 4);
        let p = path(n, &source).unwrap();
        assert_eq!(p.commands.len(), 4001);
        let PathCommand::Move { x, .. } = p.commands[0] else {
            panic!()
        };
        assert_eq!(x, 1e-300);
    }
    #[test]
    fn imports_viewbox_groups_arcs_and_undo_as_one_edit() {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(100, 100)).unwrap();
        let root = e.document(h).unwrap().root;
        let before = e.snapshot(h).unwrap();
        let p=e.prepare_svg_import(h,r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 10 10"><g opacity="0.5" fill="#f00"><rect x="1" y="1" width="4" height="4" rx="1"/><path d="M1 1a3 3 0 0 1 6 0" stroke="#fff"/></g></svg>"##,root,0).unwrap();
        let id = p.result.node;
        e.transact(h, "image.import_svg", p.ops, p.revision)
            .unwrap();
        let doc = e.document(h).unwrap();
        assert!(doc.nodes.len() > 4);
        assert!(doc.nodes.values().any(|n| n.props.opacity == 0.5
            && matches!(
                n.kind,
                NodeKind::Group {
                    isolation: Isolation::Isolated,
                    ..
                }
            )));
        assert!(doc.nodes.values().any(|n|matches!(&n.kind,NodeKind::Vector{paths} if paths.iter().any(|p|p.data.commands.iter().any(|c|matches!(c,PathCommand::Cubic{..}))))));
        let bytes = e.save_package(h).unwrap();
        let (d, w) = e.open_package(&bytes).unwrap();
        assert!(w.is_empty());
        assert!(e.document(d).unwrap().nodes.contains_key(&id));
        e.undo(h).unwrap();
        assert_eq!(e.snapshot(h).unwrap(), before);
    }
    #[test]
    fn rejects_unknown_attributes_external_content_and_partial_import() {
        let mut e = Engine::new();
        let h = e.create_document(DocumentMeta::srgb_u8(32, 32)).unwrap();
        let root = e.document(h).unwrap().root;
        let before = e.snapshot(h).unwrap();
        for s in [
            r#"<svg width="10" height="10"><rect width="1" height="1"/><text>bad</text></svg>"#,
            r#"<svg width="10" height="10"><path d="M0 0L1 1" style="fill:red"/></svg>"#,
            r#"<svg width="10" height="10"><use href="https://example.test/x"/></svg>"#,
            r#"<!DOCTYPE svg [<!ENTITY bad SYSTEM "file:///etc/passwd">]><svg width="10" height="10"/>"#,
        ] {
            let error = e
                .prepare_svg_import(h, s, root, 0)
                .err()
                .unwrap()
                .to_string();
            assert!(
                error.contains("UNSUPPORTED")
                    && (error.contains("line") || error.contains("SVG XML")),
                "{error}"
            );
            assert_eq!(e.snapshot(h).unwrap(), before);
        }
    }
}
