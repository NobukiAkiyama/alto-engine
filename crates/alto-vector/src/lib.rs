//! Canonical, editable f64 paths. Tessellation and GPU meshes are derived caches.
use alto_core::{Affine2, ContentHash};
use kurbo::{BezPath, PathEl, Shape};
use lyon_tessellation as lyon;
use serde::{Deserialize, Serialize};

mod glyph;
pub use glyph::{GlyphOutline, TextGlyph, TextRun};

pub type Color = [f32; 4];
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum PathCommand {
    Move {
        x: f64,
        y: f64,
    },
    Line {
        x: f64,
        y: f64,
    },
    Quad {
        cx: f64,
        cy: f64,
        x: f64,
        y: f64,
    },
    Cubic {
        c1x: f64,
        c1y: f64,
        c2x: f64,
        c2y: f64,
        x: f64,
        y: f64,
    },
    Close,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathData {
    pub commands: Vec<PathCommand>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillRule {
    #[default]
    NonZero,
    EvenOdd,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}
fn width() -> f32 {
    1.0
}
fn miter() -> f32 {
    4.0
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    pub color: Color,
    #[serde(default = "width")]
    pub width: f32,
    #[serde(default)]
    pub cap: LineCap,
    #[serde(default)]
    pub join: LineJoin,
    #[serde(default = "miter")]
    pub miter_limit: f32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyledPath {
    pub data: PathData,
    pub fill: Option<Color>,
    pub stroke: Option<Stroke>,
    #[serde(default)]
    pub fill_rule: FillRule,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorMask {
    pub path: PathData,
    #[serde(default)]
    pub fill_rule: FillRule,
    #[serde(default)]
    pub feather: f32,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub transform: Affine2,
}
fn enabled() -> bool {
    true
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipPath {
    pub path: PathData,
    #[serde(default)]
    pub fill_rule: FillRule,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub transform: Affine2,
}
#[derive(Debug, thiserror::Error)]
#[error("Invalid vector: {0}")]
pub struct VectorError(pub String);
type Result<T> = std::result::Result<T, VectorError>;
fn invalid(s: &str) -> VectorError {
    VectorError(s.into())
}
impl PathData {
    pub fn validate(&self) -> Result<()> {
        if self.commands.len() > 4096 {
            return Err(invalid("at most 4096 commands per path"));
        }
        let mut open = false;
        for c in &self.commands {
            let coords: Vec<f64> = match *c {
                PathCommand::Move { x, y } => {
                    open = true;
                    vec![x, y]
                }
                PathCommand::Line { x, y } if open => vec![x, y],
                PathCommand::Quad { cx, cy, x, y } if open => vec![cx, cy, x, y],
                PathCommand::Cubic {
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                } if open => vec![c1x, c1y, c2x, c2y, x, y],
                PathCommand::Close if open => {
                    open = false;
                    vec![]
                }
                _ => {
                    return Err(invalid(
                        "each subpath must start with move; closed paths need a new move",
                    ));
                }
            };
            if coords
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 1_048_576.0)
            {
                return Err(invalid("coordinates must be finite and within +/-1048576"));
            }
        }
        Ok(())
    }
    pub fn from_svg(svg: &str) -> Result<Self> {
        if svg.len() > 262144 {
            return Err(invalid("SVG path data too long"));
        }
        let p = BezPath::from_svg(svg).map_err(|e| VectorError(e.to_string()))?;
        let mut commands = vec![];
        // SVG permits a drawing command immediately after Z. Canonical data starts a new subpath.
        let mut last = (0.0, 0.0);
        let mut start = last;
        let mut closed = false;
        for e in p.elements() {
            if closed && !matches!(e, PathEl::MoveTo(_) | PathEl::ClosePath) {
                commands.push(PathCommand::Move {
                    x: last.0,
                    y: last.1,
                });
                closed = false;
            }
            let c = match *e {
                PathEl::MoveTo(p) => {
                    last = (p.x, p.y);
                    start = last;
                    closed = false;
                    PathCommand::Move { x: p.x, y: p.y }
                }
                PathEl::LineTo(p) => {
                    last = (p.x, p.y);
                    PathCommand::Line { x: p.x, y: p.y }
                }
                PathEl::QuadTo(c, p) => {
                    last = (p.x, p.y);
                    PathCommand::Quad {
                        cx: c.x,
                        cy: c.y,
                        x: p.x,
                        y: p.y,
                    }
                }
                PathEl::CurveTo(a, b, p) => {
                    last = (p.x, p.y);
                    PathCommand::Cubic {
                        c1x: a.x,
                        c1y: a.y,
                        c2x: b.x,
                        c2y: b.y,
                        x: p.x,
                        y: p.y,
                    }
                }
                PathEl::ClosePath => {
                    if closed {
                        continue;
                    }
                    last = start;
                    closed = true;
                    PathCommand::Close
                }
            };
            commands.push(c);
        }
        let data = Self { commands };
        data.validate()?;
        Ok(data)
    }
    pub fn bez_path(&self) -> BezPath {
        let mut b = BezPath::new();
        for c in &self.commands {
            match *c {
                PathCommand::Move { x, y } => b.move_to((x, y)),
                PathCommand::Line { x, y } => b.line_to((x, y)),
                PathCommand::Quad { cx, cy, x, y } => b.quad_to((cx, cy), (x, y)),
                PathCommand::Cubic {
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                } => b.curve_to((c1x, c1y), (c2x, c2y), (x, y)),
                PathCommand::Close => b.close_path(),
            }
        }
        b
    }
    pub fn to_svg(&self) -> String {
        self.bez_path().to_svg()
    }
    pub fn bounds(&self) -> [f64; 4] {
        let r = self.bez_path().bounding_box();
        [r.x0, r.y0, r.x1, r.y1]
    }
}
fn color(c: Color) -> bool {
    c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
}
impl StyledPath {
    pub fn validate(&self) -> Result<()> {
        self.data.validate()?;
        if self.fill.is_some_and(|c| !color(c)) {
            return Err(invalid("fill color"));
        }
        if let Some(s) = &self.stroke
            && (!color(s.color)
                || !s.width.is_finite()
                || !(0.0..=4096.0).contains(&s.width)
                || !s.miter_limit.is_finite()
                || !(1.0..=64.0).contains(&s.miter_limit))
        {
            return Err(invalid("stroke style"));
        }
        Ok(())
    }
    pub fn fingerprint(&self) -> ContentHash {
        ContentHash::of(&serde_json::to_vec(self).expect("validated vector serializes"))
    }
}
pub fn validate_paths(paths: &[StyledPath]) -> Result<()> {
    if paths.len() > 128 || paths.iter().map(|p| p.data.commands.len()).sum::<usize>() > 16384 {
        return Err(invalid("at most 128 paths / 16384 commands per layer"));
    }
    for p in paths {
        p.validate()?;
    }
    Ok(())
}
impl VectorMask {
    pub fn validate(&self) -> Result<()> {
        self.path.validate()?;
        if !self.feather.is_finite()
            || !(0.0..=64.0).contains(&self.feather)
            || !self.transform.is_finite()
        {
            return Err(invalid("vector mask settings"));
        }
        Ok(())
    }
}
impl ClipPath {
    pub fn validate(&self) -> Result<()> {
        self.path.validate()?;
        if !self.transform.is_finite() {
            return Err(invalid("clip transform"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct Mesh {
    pub origin: [f64; 2],
    pub vertices: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub color: Color,
}
impl Mesh {
    pub fn bytes(&self) -> usize {
        self.vertices.len() * 8 + self.indices.len() * 4
    }
}
pub trait VectorRasterizer {
    type Prepared;
    fn prepare(&mut self, paths: &[StyledPath], tolerance: f32) -> Result<Self::Prepared>;
}
#[derive(Default)]
pub struct LyonRasterizer {
    fill: lyon::FillTessellator,
    stroke: lyon::StrokeTessellator,
}
#[derive(Default)]
struct Limited {
    vertices: Vec<[f32; 2]>,
    indices: Vec<u32>,
    overflow: bool,
}
impl Limited {
    fn vertex(
        &mut self,
        p: lyon::math::Point,
    ) -> std::result::Result<lyon::VertexId, lyon::GeometryBuilderError> {
        if self.vertices.len() >= 262144 || self.overflow {
            return Err(lyon::GeometryBuilderError::TooManyVertices);
        }
        let id = lyon::VertexId(self.vertices.len() as u32);
        self.vertices.push(p.to_array());
        Ok(id)
    }
}
impl lyon::geometry_builder::GeometryBuilder for Limited {
    fn add_triangle(&mut self, a: lyon::VertexId, b: lyon::VertexId, c: lyon::VertexId) {
        if self.indices.len() + 3 > 1_572_864 {
            self.overflow = true;
            return;
        }
        self.indices.extend([a.0, b.0, c.0]);
    }
    fn abort_geometry(&mut self) {
        self.vertices.clear();
        self.indices.clear();
    }
}
impl lyon::geometry_builder::FillGeometryBuilder for Limited {
    fn add_fill_vertex(
        &mut self,
        v: lyon::FillVertex,
    ) -> std::result::Result<lyon::VertexId, lyon::GeometryBuilderError> {
        self.vertex(v.position())
    }
}
impl lyon::geometry_builder::StrokeGeometryBuilder for Limited {
    fn add_stroke_vertex(
        &mut self,
        v: lyon::StrokeVertex,
    ) -> std::result::Result<lyon::VertexId, lyon::GeometryBuilderError> {
        self.vertex(v.position())
    }
}
impl VectorRasterizer for LyonRasterizer {
    type Prepared = Vec<Mesh>;
    fn prepare(&mut self, paths: &[StyledPath], tolerance: f32) -> Result<Vec<Mesh>> {
        validate_paths(paths)?;
        let mut meshes = vec![];
        for p in paths {
            meshes.extend(self.tessellate(p, tolerance)?);
        }
        Ok(meshes)
    }
}
impl LyonRasterizer {
    pub fn tessellate(&mut self, p: &StyledPath, tolerance: f32) -> Result<Vec<Mesh>> {
        p.validate()?;
        if !tolerance.is_finite() || tolerance <= 0.0 {
            return Err(invalid("tolerance"));
        }
        let bounds = p.data.bounds();
        let origin = [(bounds[0] + bounds[2]) * 0.5, (bounds[1] + bounds[3]) * 0.5];
        let pt = |x: f64, y: f64| lyon::math::point((x - origin[0]) as f32, (y - origin[1]) as f32);
        let mut builder = lyon::path::Path::builder();
        let mut open = false;
        for c in &p.data.commands {
            match *c {
                PathCommand::Move { x, y } => {
                    if open {
                        builder.end(false);
                    }
                    builder.begin(pt(x, y));
                    open = true;
                }
                PathCommand::Line { x, y } => {
                    builder.line_to(pt(x, y));
                }
                PathCommand::Quad { cx, cy, x, y } => {
                    builder.quadratic_bezier_to(pt(cx, cy), pt(x, y));
                }
                PathCommand::Cubic {
                    c1x,
                    c1y,
                    c2x,
                    c2y,
                    x,
                    y,
                } => {
                    builder.cubic_bezier_to(pt(c1x, c1y), pt(c2x, c2y), pt(x, y));
                }
                PathCommand::Close => {
                    builder.end(true);
                    open = false;
                }
            }
        }
        if open {
            builder.end(false);
        }
        let path = builder.build();
        let mut out = vec![];
        if let Some(color) = p.fill.filter(|c| c[3] > 0.0) {
            let mut g = Limited::default();
            self.fill
                .tessellate_path(
                    &path,
                    &lyon::FillOptions::tolerance(tolerance).with_fill_rule(match p.fill_rule {
                        FillRule::NonZero => lyon::FillRule::NonZero,
                        FillRule::EvenOdd => lyon::FillRule::EvenOdd,
                    }),
                    &mut g,
                )
                .map_err(|e| VectorError(e.to_string()))?;
            if g.overflow {
                return Err(invalid("mesh index budget"));
            }
            out.push(Mesh {
                origin,
                vertices: g.vertices,
                indices: g.indices,
                color,
            });
        }
        if let Some(s) = &p.stroke
            && s.width > 0.0
            && s.color[3] > 0.0
        {
            let mut g = Limited::default();
            let options = lyon::StrokeOptions::tolerance(tolerance)
                .with_line_width(s.width)
                .with_line_cap(match s.cap {
                    LineCap::Butt => lyon::LineCap::Butt,
                    LineCap::Round => lyon::LineCap::Round,
                    LineCap::Square => lyon::LineCap::Square,
                })
                .with_line_join(match s.join {
                    LineJoin::Miter => lyon::LineJoin::Miter,
                    LineJoin::Round => lyon::LineJoin::Round,
                    LineJoin::Bevel => lyon::LineJoin::Bevel,
                })
                .with_miter_limit(s.miter_limit);
            self.stroke
                .tessellate_path(&path, &options, &mut g)
                .map_err(|e| VectorError(e.to_string()))?;
            if g.overflow {
                return Err(invalid("mesh index budget"));
            }
            out.push(Mesh {
                origin,
                vertices: g.vertices,
                indices: g.indices,
                color: s.color,
            });
        }
        Ok(out)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn svg_roundtrip_and_invalid_commands() {
        let p = PathData::from_svg("M 0 0 h 50 v 30 a 10 10 0 0 1 -10 10 Z l 10 0").unwrap();
        assert_eq!(PathData::from_svg(&p.to_svg()).unwrap(), p);
        assert!(
            PathData {
                commands: vec![PathCommand::Line { x: 1., y: 2. }]
            }
            .validate()
            .is_err()
        );
        assert!(PathData::from_svg("M NaN 0 L 4 3").is_err());
    }
    #[test]
    fn zoom_tolerance_refines_curves_and_handles_holes_strokes() {
        let p = StyledPath {
            data: PathData::from_svg(
                "M0 100 C0 -40 200 -40 200 100 C200 240 0 240 0 100 Z M60 100 L140 100 L100 160 Z",
            )
            .unwrap(),
            fill: Some([1., 0., 0., 0.5]),
            stroke: Some(Stroke {
                color: [0., 0., 0., 1.],
                width: 4.,
                cap: LineCap::Round,
                join: LineJoin::Round,
                miter_limit: 4.,
            }),
            fill_rule: FillRule::EvenOdd,
        };
        let mut r = LyonRasterizer::default();
        let a = r.tessellate(&p, 2.).unwrap();
        let b = r.tessellate(&p, 0.01).unwrap();
        assert_eq!(a.len(), 2);
        assert!(b[0].vertices.len() > a[0].vertices.len());
        for m in b {
            assert!(m.indices.iter().all(|i| (*i as usize) < m.vertices.len()));
        }
    }
}
