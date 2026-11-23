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
pub const MAX_DASH_ELEMENTS: usize = 16;
pub const MAX_DASHES_PER_PATH: usize = 4096;
pub const MAX_DASHES_PER_LAYER: usize = 16384;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dash {
    pub pattern: Vec<f32>,
    #[serde(default)]
    pub offset: f32,
}
impl Dash {
    pub fn validate(&self) -> Result<()> {
        if self.pattern.is_empty() || self.pattern.len() > MAX_DASH_ELEMENTS {
            return Err(invalid("dash pattern length must be 1..=16"));
        }
        let mut all_zero = true;
        for &elem in &self.pattern {
            if !elem.is_finite() || !(0.0..=1_048_576.0).contains(&elem) {
                return Err(invalid(
                    "dash pattern elements must be finite and within 0..=1048576",
                ));
            }
            if elem > 0.0 {
                all_zero = false;
            }
        }
        if all_zero {
            return Err(invalid("dash pattern must not be all zeros"));
        }
        let period = self.period();
        if period < 1.0 / 64.0 {
            return Err(invalid("dash period must be >= 1/64"));
        }
        if !self.offset.is_finite() || !(-1_048_576.0..=1_048_576.0).contains(&self.offset) {
            return Err(invalid("dash offset must be finite and within +/-1048576"));
        }
        Ok(())
    }

    pub fn period(&self) -> f64 {
        let sum: f64 = self.pattern.iter().map(|&x| x as f64).sum();
        if self.pattern.len() % 2 == 1 {
            sum * 2.0
        } else {
            sum
        }
    }

    pub fn on_count(&self) -> usize {
        if self.pattern.len() % 2 == 1 {
            self.pattern.len()
        } else {
            self.pattern.len() / 2
        }
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dash: Option<Dash>,
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
        if self.commands.len() > MAX_PATH_COMMANDS {
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
        check_svg_arcs(svg)?;
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

// The renderer rounds each pattern element at a precision between Q8 and Q32.
// Use the shortest period and largest edge allowance across those precisions.
fn dash_budget(dash: &Dash) -> Result<(f64, usize)> {
    let on_count = dash.on_count();
    let pattern = || dash.pattern.iter().copied().cycle().take(on_count * 2);
    let offset = f64::from(dash.offset);
    let mut period = dash.period();
    let mut extra = usize::from(offset.rem_euclid(period) > 0.0)
        + pattern().step_by(2).filter(|&value| value == 0.0).count();
    for bits in 8..=32 {
        let scale = (1u64 << bits) as f64;
        // Promoting a binary32 value to binary64 is exact, so this rounding
        // agrees with the renderer's integer binary32 quantizer.
        let quantize = |value: f64| {
            alto_core::q_from_f64_bits(value.to_bits(), bits).ok_or_else(|| invalid("dash range"))
        };
        let phase = quantize(offset)?;
        let mut exact = phase as f64 == offset * scale;
        let mut cycle = 0;
        let mut zero_ons = 0;
        for (index, value) in pattern().enumerate() {
            let value = f64::from(value);
            let q = quantize(value)?;
            exact &= q as f64 == value * scale;
            cycle += q;
            zero_ons += usize::from(index.is_multiple_of(2) && q == 0);
        }
        // If all lengths vanish at this precision, the renderer uses Q10.
        if cycle > 0 {
            period = period.min(cycle as f64 / scale);
            extra = extra.max(usize::from(phase.rem_euclid(cycle) > 0) + zero_ons);
        }
        if exact {
            break;
        }
    }
    Ok((period, extra))
}

// Each Move resets the phase. Short subpaths cannot share one period's allowance.
fn dash_count(path: &PathData, dash: &Dash) -> Result<usize> {
    let (period, extra) = dash_budget(dash)?;
    let on_count = dash.on_count();
    Ok(path
        .commands
        .chunk_by(|_, next| !matches!(next, PathCommand::Move { .. }))
        .map(control_polygon_length)
        .filter(|length| *length > 0.0)
        .map(|length| {
            ((length / period).ceil() as usize)
                .saturating_mul(on_count)
                .saturating_add(extra)
        })
        .fold(0, usize::saturating_add))
}

fn control_polygon_length(commands: &[PathCommand]) -> f64 {
    let Some((PathCommand::Move { x, y }, rest)) = commands.split_first() else {
        return 0.0;
    };
    let start = (*x, *y);
    // Coordinates on the Q8 grid are exact at every supported precision.
    // Any other coordinate can move by at most half a Q8 unit.
    let snap_error = |(x, y): (f64, f64)| {
        let axes = u8::from((x * 256.0).fract() != 0.0) + u8::from((y * 256.0).fract() != 0.0);
        f64::from(axes).sqrt() / 512.0
    };
    let mut previous = start;
    let mut previous_error = snap_error(start);
    let mut length = 0.0;
    let mut visit = |point: (f64, f64)| {
        if point != previous {
            let x = point.0 - previous.0;
            let y = point.1 - previous.1;
            let error = snap_error(point);
            length += (x * x + y * y).sqrt() + previous_error + error;
            previous = point;
            previous_error = error;
        }
    };
    for command in rest {
        match *command {
            PathCommand::Move { .. } => {}
            PathCommand::Line { x, y } => visit((x, y)),
            PathCommand::Quad { cx, cy, x, y } => {
                visit((cx, cy));
                visit((x, y));
            }
            PathCommand::Cubic {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                visit((c1x, c1y));
                visit((c2x, c2y));
                visit((x, y));
            }
            PathCommand::Close => visit(start),
        }
    }
    length
}

fn color(c: Color) -> bool {
    c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
}
impl StyledPath {
    pub fn validate(&self) -> Result<()> {
        self.validated_dash_count().map(|_| ())
    }

    fn validated_dash_count(&self) -> Result<usize> {
        self.data.validate()?;
        if self.fill.is_some_and(|c| !color(c)) {
            return Err(invalid("fill color"));
        }
        if let Some(s) = &self.stroke {
            if !color(s.color)
                || !s.width.is_finite()
                || !(0.0..=4096.0).contains(&s.width)
                || !s.miter_limit.is_finite()
                || !(1.0..=64.0).contains(&s.miter_limit)
            {
                return Err(invalid("stroke style"));
            }
            if let Some(dash) = &s.dash {
                dash.validate()?;
                let count = dash_count(&self.data, dash)?;
                if count > MAX_DASHES_PER_PATH {
                    return Err(invalid("dash count"));
                }
                return Ok(count);
            }
        }
        Ok(0)
    }
    pub fn fingerprint(&self) -> ContentHash {
        ContentHash::of(&serde_json::to_vec(self).expect("validated vector serializes"))
    }
}
pub fn validate_paths(paths: &[StyledPath]) -> Result<()> {
    if paths.len() > 128 || paths.iter().map(|p| p.data.commands.len()).sum::<usize>() > 16384 {
        return Err(invalid("at most 128 paths / 16384 commands per layer"));
    }
    let mut total_dashes = 0;
    for path in paths {
        total_dashes += path.validated_dash_count()?;
        if total_dashes > MAX_DASHES_PER_LAYER {
            return Err(invalid("dash count"));
        }
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
        if let Some(s) = &p.stroke
            && s.dash.is_some()
        {
            return Err(invalid("dashed strokes are not supported by lyon"));
        }
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
/// The command limit of [`PathData::validate`].
const MAX_PATH_COMMANDS: usize = 4096;

/// Rejects SVG path data that kurbo would flatten into more commands than
/// [`PathData::validate`] accepts, before `BezPath::from_svg` allocates them.
/// kurbo turns each arc into `ceil(n * sweep / 2pi)` cubics, where
/// `n = (1.1163 * radius / 0.1)^(1/6)` (at least 4) grows with the radius kurbo
/// uses: an arc of radius 1e107 that sweeps almost a full turn asks for 1e18 of
/// them. kurbo scales both radii up until the ellipse reaches the endpoint, so the
/// count is taken from the arc kurbo builds (`Arc::from_svg_arc`, which does not
/// flatten). Arc endpoints must also be within the coordinate limit, since a far
/// endpoint scales the radii up.
fn check_svg_arcs(svg: &str) -> Result<()> {
    use svgtypes::PathSegment as S;
    let bounded = |v: f64| v.is_finite() && v.abs() <= 1_048_576.0;
    let (mut x, mut y) = (0.0f64, 0.0f64);
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    let mut commands = 0.0f64;
    for segment in svgtypes::PathParser::from(svg) {
        let segment = segment.map_err(|e| VectorError(e.to_string()))?;
        let to = |abs: bool, px: f64, py: f64| if abs { (px, py) } else { (x + px, y + py) };
        let (nx, ny) = match segment {
            S::MoveTo { abs, x: px, y: py } => {
                let p = to(abs, px, py);
                (sx, sy) = p;
                p
            }
            S::LineTo { abs, x: px, y: py }
            | S::SmoothQuadratic { abs, x: px, y: py }
            | S::Quadratic {
                abs, x: px, y: py, ..
            }
            | S::SmoothCurveTo {
                abs, x: px, y: py, ..
            }
            | S::CurveTo {
                abs, x: px, y: py, ..
            } => to(abs, px, py),
            S::HorizontalLineTo { abs, x: px } => (if abs { px } else { x + px }, y),
            S::VerticalLineTo { abs, y: py } => (x, if abs { py } else { y + py }),
            S::ClosePath { .. } => (sx, sy),
            S::EllipticalArc {
                abs,
                rx,
                ry,
                x_axis_rotation,
                large_arc,
                sweep,
                x: px,
                y: py,
            } => {
                let end = to(abs, px, py);
                if !bounded(x) || !bounded(y) || !bounded(end.0) || !bounded(end.1) {
                    return Err(invalid("coordinates must be finite and within +/-1048576"));
                }
                // The same arc as kurbo's parser builds; `None` draws a line instead.
                let arc = kurbo::SvgArc {
                    from: kurbo::Point::new(x, y),
                    to: kurbo::Point::new(end.0, end.1),
                    radii: kurbo::Vec2::new(rx, ry),
                    x_rotation: x_axis_rotation.to_radians(),
                    large_arc,
                    sweep,
                };
                if let Some(arc) = kurbo::Arc::from_svg_arc(&arc) {
                    let radius = arc.radii.x.max(arc.radii.y) / 0.1;
                    let n = (1.1163 * radius).powf(1.0 / 6.0).max(3.999_999);
                    // Counted one short of kurbo's count: the bound can only be generous.
                    commands += (n * arc.sweep_angle.abs() / std::f64::consts::TAU).ceil() - 1.0;
                }
                end
            }
        };
        commands += 1.0;
        // A non-finite radius makes the count NaN.
        if commands.is_nan() || commands > MAX_PATH_COMMANDS as f64 {
            return Err(invalid("at most 4096 commands per path"));
        }
        (x, y) = (nx, ny);
    }
    Ok(())
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
    fn svg_arcs_are_bounded_before_they_are_flattened() {
        // The `path` fuzzer's input: a near-full turn of radius 1e107 asks kurbo for
        // about 1e18 cubics.
        let long = format!("M0 0 A 5 1{} 0 1 0 10 1", "0".repeat(107));
        assert!(PathData::from_svg(&long).is_err());
        assert!(PathData::from_svg("M0 0 a 1e300 1e300 0 1 1 10 10").is_err());
        // Radii scaled up by kurbo: a small radius across the chord makes the other large.
        assert!(PathData::from_svg("M0 0 A 1e30 0.0001 0 1 1 0 10").is_err());
        // A far current point scales the radii up to reach it.
        assert!(PathData::from_svg("M0 0 h 1e300 a 1 1 0 0 1 1e300 0").is_err());
        assert!(PathData::from_svg("M0 0 h 1e308 h 1e308 a 1 1 0 0 1 1 0").is_err());
        // Many arcs add up.
        let many = format!("M0 0{}", " a 10 10 0 1 1 1 0".repeat(2000));
        assert!(PathData::from_svg(&many).is_err());
        // Ordinary and degenerate arcs keep working: a huge radius over a short sweep
        // is one cubic, and a radius under 1e-5 is a straight line.
        assert!(PathData::from_svg("M0 0 a 1e20 1e20 0 0 1 10 10").is_ok());
        assert!(PathData::from_svg("M0 0 A 1000000 1e-100 0 0 1 0 10").is_ok());
        assert!(PathData::from_svg("M10 10 h 20 a 5 5 0 1 0 10 10 A 3 4 30 0 1 0 0 z").is_ok());
        let fits = format!("M0 0{}", " a 10 10 0 0 1 1 0".repeat(1000));
        assert!(PathData::from_svg(&fits).is_ok());
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
                dash: None,
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

    #[test]
    fn no_dash_serializes_the_same_bytes() {
        let p = StyledPath {
            data: PathData {
                commands: vec![
                    PathCommand::Move { x: 0.0, y: 0.0 },
                    PathCommand::Line { x: 10.0, y: 10.0 },
                ],
            },
            fill: None,
            stroke: Some(Stroke {
                color: [0.0, 0.0, 0.0, 1.0],
                width: 1.0,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                miter_limit: 4.0,
                dash: None,
            }),
            fill_rule: FillRule::NonZero,
        };
        p.validate().unwrap();
        let serialized = serde_json::to_string(&p).unwrap();
        assert!(!serialized.contains("dash"));
        let json_legacy = r#"{"data":{"commands":[{"op":"move","x":0.0,"y":0.0},{"op":"line","x":10.0,"y":10.0}]},"fill":null,"stroke":{"color":[0.0,0.0,0.0,1.0],"width":1.0,"cap":"butt","join":"miter","miter_limit":4.0},"fill_rule":"non_zero"}"#;
        assert_eq!(serialized, json_legacy);
        assert_eq!(p.fingerprint(), ContentHash::of(json_legacy.as_bytes()));
    }

    #[test]
    fn dash_roundtrips_through_json() {
        let dash = Dash {
            pattern: vec![5.0, 2.0],
            offset: 1.0,
        };
        let p = StyledPath {
            data: PathData {
                commands: vec![
                    PathCommand::Move { x: 0.0, y: 0.0 },
                    PathCommand::Line { x: 100.0, y: 0.0 },
                ],
            },
            fill: None,
            stroke: Some(Stroke {
                color: [1.0, 0.0, 0.0, 1.0],
                width: 2.0,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                miter_limit: 4.0,
                dash: Some(dash),
            }),
            fill_rule: FillRule::NonZero,
        };
        p.validate().unwrap();
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"dash\":"));
        let deserialized: StyledPath = serde_json::from_str(&json).unwrap();
        assert_eq!(p, deserialized);

        // default offset is 0.0 when omitted
        let json_no_offset = r#"{"pattern":[10.0,5.0]}"#;
        let d: Dash = serde_json::from_str(json_no_offset).unwrap();
        assert_eq!(d.offset, 0.0);
        assert_eq!(d.pattern, vec![10.0, 5.0]);
    }

    #[test]
    fn all_zero_pattern_is_refused() {
        let d = Dash {
            pattern: vec![0.0, 0.0],
            offset: 0.0,
        };
        assert!(d.validate().is_err());
        let d_single = Dash {
            pattern: vec![0.0],
            offset: 0.0,
        };
        assert!(d_single.validate().is_err());
    }

    #[test]
    fn dash_pattern_longer_than_16_is_refused() {
        let d_16 = Dash {
            pattern: vec![1.0; 16],
            offset: 0.0,
        };
        assert!(d_16.validate().is_ok());

        let d_17 = Dash {
            pattern: vec![1.0; 17],
            offset: 0.0,
        };
        assert!(d_17.validate().is_err());

        let d_empty = Dash {
            pattern: vec![],
            offset: 0.0,
        };
        assert!(d_empty.validate().is_err());
    }

    #[test]
    fn negative_dash_length_is_refused() {
        let d_neg = Dash {
            pattern: vec![10.0, -2.0],
            offset: 0.0,
        };
        assert!(d_neg.validate().is_err());

        let d_nan = Dash {
            pattern: vec![f32::NAN],
            offset: 0.0,
        };
        assert!(d_nan.validate().is_err());

        let d_inf = Dash {
            pattern: vec![f32::INFINITY],
            offset: 0.0,
        };
        assert!(d_inf.validate().is_err());

        let d_too_large = Dash {
            pattern: vec![1_048_577.0],
            offset: 0.0,
        };
        assert!(d_too_large.validate().is_err());

        // negative offset is valid within +/-1048576
        let d_neg_offset = Dash {
            pattern: vec![10.0, 5.0],
            offset: -100.0,
        };
        assert!(d_neg_offset.validate().is_ok());

        let d_offset_too_large = Dash {
            pattern: vec![10.0, 5.0],
            offset: 1_048_577.0,
        };
        assert!(d_offset_too_large.validate().is_err());

        let d_offset_too_negative = Dash {
            pattern: vec![10.0, 5.0],
            offset: -1_048_577.0,
        };
        assert!(d_offset_too_negative.validate().is_err());
    }

    #[test]
    fn dash_past_the_count_limit_is_refused() {
        // Path limit: MAX_DASHES_PER_PATH = 4096
        // Line length = 8192.0, pattern = [1.0, 1.0] -> period = 2.0, on_count = 1
        // ceil(8192 / 2) * 1 = 4096 -> OK
        let make_path = |len: f64| StyledPath {
            data: PathData {
                commands: vec![
                    PathCommand::Move { x: 0.0, y: 0.0 },
                    PathCommand::Line { x: len, y: 0.0 },
                ],
            },
            fill: None,
            stroke: Some(Stroke {
                color: [0.0, 0.0, 0.0, 1.0],
                width: 1.0,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                miter_limit: 4.0,
                dash: Some(Dash {
                    pattern: vec![1.0, 1.0],
                    offset: 0.0,
                }),
            }),
            fill_rule: FillRule::NonZero,
        };

        let p_exact = make_path(8192.0);
        assert!(p_exact.validate().is_ok());

        let p_over = make_path(8192.1);
        assert!(p_over.validate().is_err());

        // Half-away rounding expands this line by one Q8 unit. The rounded
        // line would start a 4097th dash even though its original length is 8192.
        let mut snapped = make_path(8192.0);
        snapped.data.commands = vec![
            PathCommand::Move {
                x: -1.0 / 512.0,
                y: 0.0,
            },
            PathCommand::Line {
                x: 8192.0 - 1.0 / 512.0,
                y: 0.0,
            },
        ];
        assert_eq!(snapped.validate().unwrap_err().0, "dash count");

        // Layer limit: MAX_DASHES_PER_LAYER = 16384
        // 4 paths of 4000 dashes = 16000 <= 16384 -> OK
        let p4000 = make_path(8000.0);
        let paths_ok = vec![p4000.clone(), p4000.clone(), p4000.clone(), p4000.clone()];
        assert!(validate_paths(&paths_ok).is_ok());

        // 5 paths of 4000 dashes = 20000 > 16384 -> Err
        let paths_over = vec![
            p4000.clone(),
            p4000.clone(),
            p4000.clone(),
            p4000.clone(),
            p4000,
        ];
        assert!(validate_paths(&paths_over).is_err());

        // Restarting the phase at Move consumes a full dash for each short
        // subpath, even though their total length is only 250.5 units.
        let mut restarted = make_path(6400.0);
        for _ in 0..501 {
            restarted.data.commands.extend([
                PathCommand::Move { x: 0.0, y: 0.0 },
                PathCommand::Line { x: 0.5, y: 0.0 },
            ]);
        }
        assert!(restarted.validate().is_ok());
        assert!(validate_paths(&vec![restarted; 5]).is_err());
    }

    #[test]
    fn dash_budget_accounts_for_rounding_at_render_precision() {
        let mut dots = vec![0.4 / 256.0; 15];
        dots.push(1.0 / 256.0);
        for (length, pattern) in [
            (64.5, vec![0.0079, 0.0079]),
            (47.65, vec![1.49 / 256.0; 16]),
            (13.8, dots),
        ] {
            let dash = Dash {
                pattern,
                offset: 0.0,
            };
            dash.validate().unwrap();
            assert!(
                (length / dash.period()).ceil() as usize * dash.on_count() <= MAX_DASHES_PER_PATH
            );
            let data = PathData {
                commands: vec![
                    PathCommand::Move { x: 0.0, y: 0.0 },
                    PathCommand::Line { x: length, y: 0.0 },
                ],
            };
            let path = StyledPath {
                data,
                fill: None,
                stroke: Some(Stroke {
                    color: [0.0, 0.0, 0.0, 1.0],
                    width: 1.0,
                    cap: LineCap::Butt,
                    join: LineJoin::Miter,
                    miter_limit: 4.0,
                    dash: Some(dash),
                }),
                fill_rule: FillRule::NonZero,
            };
            assert_eq!(path.validate().unwrap_err().0, "dash count");
        }
    }

    #[test]
    fn odd_pattern_validates_as_its_double() {
        // pattern with length 1: element is 1/128 (< 1/64)
        // Doubled period = 2 * (1/128) = 1/64 >= 1/64, so it validates.
        let d_odd = Dash {
            pattern: vec![1.0 / 128.0],
            offset: 0.0,
        };
        assert_eq!(d_odd.period(), 1.0 / 64.0);
        assert_eq!(d_odd.on_count(), 1);
        assert!(d_odd.validate().is_ok());

        // But 1/256 doubled is 1/128 < 1/64, so it is refused.
        let d_too_small = Dash {
            pattern: vec![1.0 / 256.0],
            offset: 0.0,
        };
        assert!(d_too_small.validate().is_err());
    }

    #[test]
    fn lyon_rejects_dashed_stroke() {
        let p = StyledPath {
            data: PathData::from_svg("M 0 0 L 10 10").unwrap(),
            fill: None,
            stroke: Some(Stroke {
                color: [0., 0., 0., 1.],
                width: 1.,
                cap: LineCap::Butt,
                join: LineJoin::Miter,
                miter_limit: 4.,
                dash: Some(Dash {
                    pattern: vec![5., 5.],
                    offset: 0.,
                }),
            }),
            fill_rule: FillRule::NonZero,
        };
        let mut r = LyonRasterizer::default();
        assert!(r.tessellate(&p, 1.0).is_err());
    }
}
