use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct StrokeSample {
    pub pos: [f32; 2],
    pub pressure: f32,
    pub tilt: [f32; 2],
    pub t_ms: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dab {
    pub center: [f32; 2],
    pub radius: f32,
    pub opacity: f32,
    /// Rotation in radians, in layer coordinates.
    pub angle: f32,
    #[serde(default = "round_tip")]
    pub roundness: f32,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurveInput {
    Pressure,
    Speed,
    Tilt,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushCurve {
    pub input: CurveInput,
    pub points: Vec<[f32; 2]>,
}

const DEFAULT_PRESSURE_SIZE_POINTS: [[f32; 2]; 3] = [[0.0, 0.05], [0.05, 0.05], [1.0, 1.0]];

impl BrushCurve {
    fn pressure_size() -> Self {
        Self {
            input: CurveInput::Pressure,
            points: DEFAULT_PRESSURE_SIZE_POINTS.to_vec(),
        }
    }

    fn constant() -> Self {
        Self {
            input: CurveInput::Pressure,
            points: vec![[0.0, 1.0], [1.0, 1.0]],
        }
    }

    fn validate(&self) -> Result<(), &'static str> {
        let points = &self.points;
        if (2..=8).contains(&points.len())
            && points[0][0] == 0.0
            && points[points.len() - 1][0] == 1.0
            && points
                .iter()
                .all(|p| p.iter().all(|v| (0.0..=1.0).contains(v)))
            && points.windows(2).all(|pair| pair[0][0] < pair[1][0])
        {
            Ok(())
        } else {
            Err("INVALID_ARGUMENT: brush curve")
        }
    }

    fn evaluate(&self, input: f32) -> f32 {
        let x = input.clamp(0.0, 1.0);
        for pair in self.points.windows(2) {
            let [x0, y0] = pair[0];
            let [x1, y1] = pair[1];
            if x <= x1 {
                // Preserve the exact pressure value on the default curve's linear segment.
                return if x0 == y0 && x1 == y1 {
                    x
                } else {
                    y0 + (y1 - y0) * ((x - x0) / (x1 - x0))
                };
            }
        }
        1.0
    }

    fn factor(&self, pressure: f32, speed: f32, tilt: [f32; 2]) -> f32 {
        self.evaluate(match self.input {
            CurveInput::Pressure => pressure,
            CurveInput::Speed => speed,
            CurveInput::Tilt => (tilt[0].hypot(tilt[1]) / 90.0).min(1.0),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrushDynamics {
    pub size: BrushCurve,
    pub opacity: BrushCurve,
    pub flow: BrushCurve,
    pub angle_from_tilt: bool,
}

impl Default for BrushDynamics {
    fn default() -> Self {
        Self {
            size: BrushCurve::pressure_size(),
            opacity: BrushCurve::constant(),
            flow: BrushCurve::constant(),
            angle_from_tilt: false,
        }
    }
}

impl BrushDynamics {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.size.validate()?;
        self.opacity.validate()?;
        self.flow.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrushConfig {
    pub size: f32,
    pub hardness: f32,
    pub spacing: f32,
    pub opacity: f32,
    pub flow: f32,
    #[serde(default)]
    pub dynamics: BrushDynamics,
    /// Rotation in degrees, in layer coordinates.
    #[serde(default)]
    pub angle: f32,
    /// Short-axis radius divided by the long-axis radius.
    #[serde(default = "round_tip")]
    pub roundness: f32,
}
fn round_tip() -> f32 {
    1.0
}
impl Default for BrushConfig {
    fn default() -> Self {
        Self {
            size: 24.0,
            hardness: 0.8,
            spacing: 0.15,
            opacity: 1.0,
            flow: 1.0,
            dynamics: BrushDynamics::default(),
            angle: 0.0,
            roundness: round_tip(),
        }
    }
}
impl BrushConfig {
    pub fn normalized(mut self) -> Result<Self, &'static str> {
        if !self.size.is_finite() || self.size <= 0.0 || self.size > 2048.0 {
            return Err("INVALID_ARGUMENT: brush size");
        }
        if ![self.hardness, self.spacing, self.opacity, self.flow]
            .iter()
            .all(|value| value.is_finite())
        {
            return Err("INVALID_ARGUMENT: brush config");
        }
        if !self.angle.is_finite() {
            return Err("INVALID_ARGUMENT: brush angle");
        }
        if !self.roundness.is_finite() {
            return Err("INVALID_ARGUMENT: brush roundness");
        }
        self.dynamics.validate()?;
        self.angle = self.angle.rem_euclid(360.0);
        self.roundness = self.roundness.clamp(0.01, 1.0);
        self.hardness = self.hardness.clamp(0.0, 0.999);
        self.spacing = self.spacing.clamp(0.01, 1.0);
        self.flow = self.flow.clamp(0.0, 1.0);
        self.opacity = self.opacity.clamp(0.0, 1.0);
        Ok(self)
    }
}
pub struct StrokeSampler {
    config: BrushConfig,
    last: Option<StrokeSample>,
    previous: Option<StrokeSample>,
    until: f32,
    speed: Option<f64>,
    tilt_angle: f32,
}
impl StrokeSampler {
    pub fn new(config: BrushConfig) -> Result<Self, &'static str> {
        Ok(Self {
            config: config.normalized()?,
            last: None,
            previous: None,
            until: 0.0,
            speed: None,
            tilt_angle: 0.0,
        })
    }

    pub fn config(&self) -> &BrushConfig {
        &self.config
    }

    fn dab(&mut self, pos: [f32; 2], pressure: f32, speed: f32, tilt: [f32; 2]) -> Dab {
        let dynamics = &self.config.dynamics;
        let size_factor = dynamics.size.factor(pressure, speed, tilt).max(0.05);
        self.until = (self.config.size * self.config.spacing * size_factor).max(0.25);
        if dynamics.angle_from_tilt && tilt != [0.0; 2] {
            self.tilt_angle = tilt[1].atan2(tilt[0]);
        }
        Dab {
            center: pos,
            radius: self.config.size * 0.5 * size_factor,
            opacity: self.config.flow
                * dynamics.flow.factor(pressure, speed, tilt)
                * dynamics.opacity.factor(pressure, speed, tilt),
            angle: self.config.angle.to_radians() + self.tilt_angle,
            roundness: self.config.roundness,
        }
    }

    pub fn input(&mut self, sample: StrokeSample) -> Vec<Dab> {
        if !sample.pos.iter().all(|v| v.is_finite()) || !sample.pressure.is_finite() {
            return vec![];
        }
        let sample = StrokeSample {
            pressure: sample.pressure.clamp(0.0, 1.0),
            tilt: if sample.tilt.iter().all(|v| v.is_finite()) {
                sample.tilt.map(|v| v.clamp(-90.0, 90.0))
            } else {
                [0.0; 2]
            },
            ..sample
        };
        let Some(last) = self.last else {
            self.last = Some(sample);
            let dab = self.dab(sample.pos, sample.pressure, 0.0, sample.tilt);
            // Preserve the original distance to the second dab for the default pressure curve.
            if self.config.dynamics.size.input == CurveInput::Pressure
                && self.config.dynamics.size.points == DEFAULT_PRESSURE_SIZE_POINTS
            {
                self.until = (self.config.size * self.config.spacing).max(0.25);
            }
            return vec![dab];
        };
        let prev = self.previous.unwrap_or(last);
        let length =
            ((sample.pos[0] - last.pos[0]).powi(2) + (sample.pos[1] - last.pos[1]).powi(2)).sqrt();
        let start_speed = self.speed.unwrap_or(0.0);
        let dt = sample.t_ms - last.t_ms;
        if dt.is_finite() && dt > 0.0 {
            let measured = f64::from(length) / dt;
            if measured.is_finite() {
                self.speed = Some(
                    self.speed
                        .map_or(measured, |speed| 0.3 * measured + 0.7 * speed),
                );
            }
        }
        let normalize_speed = |speed: f64| (speed / 4.0).min(1.0) as f32;
        let start_speed = normalize_speed(start_speed);
        let end_speed = normalize_speed(self.speed.unwrap_or(0.0));
        let steps = (length / 0.5).ceil().max(1.0) as usize;
        let mut from = last.pos;
        let mut out = Vec::new();
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            let mut p = [0.0; 2];
            for (i, v) in p.iter_mut().enumerate() {
                let p0 = prev.pos[i];
                let p1 = last.pos[i];
                let p2 = sample.pos[i];
                let p3 = 2.0 * p2 - p1;
                *v = 0.5
                    * ((2.0 * p1)
                        + (-p0 + p2) * t
                        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t
                        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t * t * t);
            }
            let dx = p[0] - from[0];
            let dy = p[1] - from[1];
            let d = (dx * dx + dy * dy).sqrt();
            let pressure = last.pressure + (sample.pressure - last.pressure) * t;
            if d > 0.0 {
                let mut consumed = 0.0;
                while self.until <= d - consumed {
                    consumed += self.until;
                    let a = consumed / d;
                    let speed = start_speed + (end_speed - start_speed) * t;
                    let tilt =
                        std::array::from_fn(|i| last.tilt[i] + (sample.tilt[i] - last.tilt[i]) * t);
                    out.push(self.dab([from[0] + dx * a, from[1] + dy * a], pressure, speed, tilt));
                }
                self.until -= d - consumed;
            }
            from = p;
        }
        self.previous = self.last;
        self.last = Some(sample);
        out
    }
}
/// Coverage image in texel order, owned by one stroke.
pub struct BrushImage {
    width: u32,
    height: u32,
    pixels: Box<[u8]>,
}

impl BrushImage {
    pub fn new(width: u32, height: u32, pixels: &[u8]) -> Result<Self, &'static str> {
        if !(16..=1024).contains(&width)
            || !(16..=1024).contains(&height)
            || pixels.len() != (width * height) as usize
        {
            return Err("INVALID_ARGUMENT: brush tip");
        }
        Ok(Self {
            width,
            height,
            pixels: pixels.into(),
        })
    }

    pub fn dimensions(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    fn sample(&self, u: f32, v: f32, radius: f32) -> f32 {
        let scale = self.width.max(self.height) as f32 / (2.0 * radius);
        let x = u * scale + self.width as f32 * 0.5 - 0.5;
        let y = v * scale + self.height as f32 * 0.5 - 0.5;
        if x <= -1.0 || y <= -1.0 || x >= self.width as f32 || y >= self.height as f32 {
            return 0.0;
        }
        let texel = |x: i32, y: i32| {
            if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
                0.0
            } else {
                f32::from(self.pixels[(y as u32 * self.width + x as u32) as usize]) / 255.0
            }
        };
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let fx = x - x0 as f32;
        let fy = y - y0 as f32;
        let mix = |a, b, t| a + (b - a) * t;
        mix(
            mix(texel(x0, y0), texel(x0 + 1, y0), fx),
            mix(texel(x0, y0 + 1), texel(x0 + 1, y0 + 1), fx),
            fy,
        )
    }
}

pub fn cpu_paint(
    base: &[u8],
    origin: [f32; 2],
    dabs: &[Dab],
    config: BrushConfig,
    color: [f32; 4],
    erase: bool,
    tip: Option<&BrushImage>,
) -> Vec<u8> {
    let mut out = base.to_vec();
    let rotations: Vec<_> = dabs
        .iter()
        .map(|d| (tip.is_some() || d.roundness < 1.0).then(|| d.angle.sin_cos()))
        .collect();
    for y in 0..256 {
        for x in 0..256 {
            let mut coverage = 0.0;
            let p = [origin[0] + x as f32 + 0.5, origin[1] + y as f32 + 0.5];
            for (d, rotation) in dabs.iter().zip(&rotations) {
                let dx = p[0] - d.center[0];
                let dy = p[1] - d.center[1];
                let [u, v] = if let Some((sin, cos)) = rotation {
                    [
                        dx * cos + dy * sin,
                        (-dx * sin + dy * cos) / d.roundness.max(0.01),
                    ]
                } else {
                    [dx, dy]
                };
                let radius = d.radius.max(0.01);
                let shape = if let Some(tip) = tip {
                    tip.sample(u, v, radius)
                } else {
                    let dist = (u.powi(2) + v.powi(2)).sqrt() / radius;
                    let t = ((dist - config.hardness) / (1.0 - config.hardness).max(0.001))
                        .clamp(0.0, 1.0);
                    1.0 - t * t * (3.0 - 2.0 * t)
                };
                coverage = 1.0 - (1.0 - coverage) * (1.0 - shape * d.opacity);
            }
            let alpha = coverage.min(config.opacity) * color[3];
            let i = (y * 256 + x) * 4;
            let ba = base[i + 3] as f32 / 255.0;
            let a = if erase {
                ba * (1.0 - alpha)
            } else {
                alpha + ba * (1.0 - alpha)
            };
            for c in 0..3 {
                out[i + c] = if erase {
                    base[i + c]
                } else {
                    (((color[c] * alpha + base[i + c] as f32 / 255.0 * ba * (1.0 - alpha))
                        / a.max(0.000001))
                        * 255.0)
                        .round() as u8
                };
            }
            out[i + 3] = (a * 255.0).round() as u8;
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sample(x: f32, pressure: f32, tilt: [f32; 2], t_ms: f64) -> StrokeSample {
        StrokeSample {
            pos: [x, 128.0],
            pressure,
            tilt,
            t_ms,
        }
    }

    fn linear(input: CurveInput) -> BrushCurve {
        BrushCurve {
            input,
            points: vec![[0.0, 0.0], [1.0, 1.0]],
        }
    }

    #[test]
    fn brush_image_rejects_invalid_dimensions_and_lengths() {
        for (width, height, length) in [
            (8, 16, 128),
            (2048, 16, 32768),
            (32, 16, 511),
            (u32::MAX, 32, 0),
        ] {
            assert_eq!(
                BrushImage::new(width, height, &vec![0; length]).err(),
                Some("INVALID_ARGUMENT: brush tip")
            );
        }
    }

    #[test]
    fn tip_uses_the_long_side_for_scale_and_blends_to_transparent() {
        let image = BrushImage::new(32, 16, &[255; 32 * 16]).unwrap();
        assert_eq!(image.sample(0.0, 0.0, 32.0), 1.0);
        assert_eq!(image.sample(31.0, 0.0, 32.0), 1.0);
        assert_eq!(image.sample(32.0, 0.0, 32.0), 0.5);
        assert_eq!(image.sample(33.0, 0.0, 32.0), 0.0);
        assert_eq!(image.sample(0.0, -15.0, 32.0), 1.0);
        assert_eq!(image.sample(0.0, -16.0, 32.0), 0.5);
        assert_eq!(image.sample(0.0, -17.0, 32.0), 0.0);
    }

    #[test]
    fn curve_interpolates_and_clamps() {
        let curve = BrushCurve {
            input: CurveInput::Pressure,
            points: vec![[0.0, 0.25], [0.5, 1.0], [1.0, 0.5]],
        };
        assert_eq!(curve.evaluate(-1.0), 0.25);
        assert_eq!(curve.evaluate(0.25), 0.625);
        assert_eq!(curve.evaluate(0.75), 0.75);
        assert_eq!(curve.evaluate(2.0), 0.5);
    }

    #[test]
    fn invalid_curves_are_rejected() {
        for points in [
            vec![[0.0, 0.0]],
            (0..=8).map(|i| [i as f32 / 8.0, 0.0]).collect(),
            vec![[0.0, 0.0], [0.0, 0.5], [1.0, 1.0]],
            vec![[0.1, 0.0], [1.0, 1.0]],
            vec![[0.0, 0.0], [0.9, 1.0]],
            vec![[0.0, 0.0], [1.0, 1.1]],
            vec![[0.0, f32::NAN], [1.0, 1.0]],
        ] {
            let curve = BrushCurve {
                input: CurveInput::Pressure,
                points,
            };
            let result = StrokeSampler::new(BrushConfig {
                dynamics: BrushDynamics {
                    size: curve,
                    ..BrushDynamics::default()
                },
                ..BrushConfig::default()
            });
            assert_eq!(result.err(), Some("INVALID_ARGUMENT: brush curve"));
        }
    }

    #[test]
    fn default_size_curve_matches_the_old_pressure_rule_exactly() {
        for pressure in [0.0, 0.03, 0.05, 0.1, 0.3, 0.5, 1.0] {
            let mut sampler = StrokeSampler::new(BrushConfig::default()).unwrap();
            let dabs = sampler.input(sample(0.0, pressure, [0.0; 2], 0.0));
            assert_eq!(dabs[0].radius, 12.0 * pressure.max(0.05));
            assert_eq!(sampler.until, 24.0 * 0.15);
        }
    }

    #[test]
    fn default_stroke_preserves_pixels_on_both_tiles() {
        let mut sampler = StrokeSampler::new(BrushConfig::default()).unwrap();
        let samples = [
            ([230.5, 128.5], 0.03, [8.0, 24.0], 0.0),
            ([250.5, 116.0], 0.4, [0.0; 2], 10.0),
            ([279.0, 140.5], 0.9, [12.0, 0.0], 23.0),
            ([303.5, 160.5], 0.2, [0.0; 2], 23.0),
        ];
        let dabs: Vec<_> = samples
            .into_iter()
            .flat_map(|(pos, pressure, tilt, t_ms)| {
                sampler.input(StrokeSample {
                    pos,
                    pressure,
                    tilt,
                    t_ms,
                })
            })
            .collect();
        // BLAKE3 of the RGBA tiles painted at the pre-dynamics main commit a3d4651.
        for (x, expected) in [
            (
                0.0,
                "f8da9c89ce4e36b5d6a163a92c7f039f74a5c56f17968b5e2317694cc2129ba5",
            ),
            (
                256.0,
                "48a1bf71ac039d6a15b57f11269a885364a079a4561c4141c17220c7d9922c79",
            ),
        ] {
            let pixels = cpu_paint(
                &vec![0; 256 * 256 * 4],
                [x, 0.0],
                &dabs,
                sampler.config().clone(),
                [0.25, 0.5, 0.75, 1.0],
                false,
                None,
            );
            assert_eq!(alto_core::ContentHash::of(&pixels).to_string(), expected);
        }
    }

    #[test]
    fn speed_curve_interpolates_smoothed_speed_and_handles_invalid_timestamps() {
        let mut sampler = StrokeSampler::new(BrushConfig {
            size: 10.0,
            spacing: 0.1,
            dynamics: BrushDynamics {
                opacity: linear(CurveInput::Speed),
                ..BrushDynamics::default()
            },
            ..BrushConfig::default()
        })
        .unwrap();
        let mut input = |x, time| sampler.input(sample(x, 1.0, [0.0; 2], time));
        assert_eq!(input(0.0, 0.0)[0].opacity, 0.0);
        let first = input(10.0, 10.0);
        assert!(first[0].opacity > 0.0 && first[0].opacity < 0.25);
        assert_eq!(first.last().unwrap().opacity, 0.25);
        let next = input(30.0, 20.0);
        let smoothed = (0.3 * 2.0 + 0.7 * 1.0) / 4.0;
        assert!(next[0].opacity > 0.25 && next[0].opacity < smoothed);
        assert_eq!(next.last().unwrap().opacity, smoothed);
        for (x, time) in [
            (40.0, 20.0),
            (50.0, 19.0),
            (60.0, f64::NAN),
            (70.0, f64::INFINITY),
        ] {
            let dabs = input(x, time);
            assert!(!dabs.is_empty());
            assert!(dabs.iter().all(|dab| dab.opacity == smoothed));
        }
        let recovering = input(80.0, 50.0);
        assert!(!recovering.is_empty());
        assert!(recovering.iter().all(|dab| dab.opacity == smoothed));
        let recovered = input(90.0, 55.0);
        assert!(recovered.last().unwrap().opacity > smoothed);
    }

    #[test]
    fn tilt_rotates_dabs_remembers_the_previous_angle_and_scales_flow() {
        let config = BrushConfig {
            angle: 30.0,
            dynamics: BrushDynamics {
                angle_from_tilt: true,
                flow: linear(CurveInput::Tilt),
                opacity: linear(CurveInput::Pressure),
                ..BrushDynamics::default()
            },
            ..BrushConfig::default()
        };
        let samples = [
            sample(0.0, 1.0, [0.0; 2], 0.0),
            sample(20.0, 0.5, [0.0, 90.0], 10.0),
            sample(40.0, 0.5, [0.0; 2], 20.0),
            sample(60.0, 0.5, [f32::NAN, 90.0], 30.0),
        ];
        let mut sampler = StrokeSampler::new(config.clone()).unwrap();
        let dabs = samples.map(|s| sampler.input(s));
        let fixed = 30.0_f32.to_radians();
        let tilted = fixed + std::f32::consts::FRAC_PI_2;
        assert_eq!(dabs[0][0].angle, fixed);
        assert_eq!(dabs[0][0].opacity, 0.0);
        let last_tilted = dabs[1].last().unwrap();
        assert_eq!(last_tilted.angle, tilted);
        assert!(last_tilted.opacity > 0.4 && last_tilted.opacity <= 0.5);
        assert!(dabs[2].iter().all(|dab| dab.angle == tilted));
        assert!(!dabs[3].is_empty());
        assert!(
            dabs[3]
                .iter()
                .all(|dab| dab.angle == tilted && dab.opacity == 0.0)
        );
        let mut replay = StrokeSampler::new(config).unwrap();
        assert_eq!(samples.map(|s| replay.input(s)), dabs);
    }

    #[test]
    fn mouse_stroke_with_tilt_enabled_matches_the_fixed_angle() {
        let config = BrushConfig {
            angle: 45.0,
            roundness: 0.4,
            ..BrushConfig::default()
        };
        let mut enabled = config.clone();
        enabled.dynamics.angle_from_tilt = true;
        let mut fixed = StrokeSampler::new(config).unwrap();
        let mut tilted = StrokeSampler::new(enabled).unwrap();
        for s in [
            sample(0.0, 1.0, [0.0; 2], 0.0),
            sample(20.0, 1.0, [0.0; 2], 15.0),
        ] {
            assert_eq!(tilted.input(s), fixed.input(s));
        }
    }

    #[test]
    fn config_normalization_clamps_roundness_and_wraps_angle() {
        let config = BrushConfig {
            angle: -90.0,
            roundness: 0.0,
            ..BrushConfig::default()
        }
        .normalized()
        .unwrap();
        assert_eq!(config.angle, 270.0);
        assert_eq!(config.roundness, 0.01);
        assert!(
            BrushConfig {
                angle: f32::NAN,
                ..config.clone()
            }
            .normalized()
            .is_err()
        );
        assert!(
            BrushConfig {
                roundness: f32::INFINITY,
                ..config
            }
            .normalized()
            .is_err()
        );
    }

    #[test]
    fn round_dab_with_roundness_one_is_unchanged() {
        let config = BrushConfig::default();
        let dab = Dab {
            center: [128.5, 128.5],
            radius: 12.0,
            opacity: 0.75,
            angle: 1.0,
            roundness: 1.0,
        };
        let base = vec![0; 256 * 256 * 4];
        let painted = cpu_paint(
            &base,
            [0.0; 2],
            &[dab],
            config,
            [0.0, 0.0, 0.0, 1.0],
            false,
            None,
        );
        // BLAKE3 of the RGBA bytes painted by the same dab at 3dc0b48.
        assert_eq!(
            alto_core::ContentHash::of(&painted).to_string(),
            "62e710d7e6e4cfb3fbb00b1a79f467ddf6b653a6530b051d40c64fdf0dcf23a5"
        );
    }

    #[test]
    fn thin_ellipse_paints_its_center_and_stays_narrow() {
        let config = BrushConfig {
            roundness: 0.01,
            ..BrushConfig::default()
        }
        .normalized()
        .unwrap();
        let mut sampler = StrokeSampler::new(config.clone()).unwrap();
        let dabs = sampler.input(StrokeSample {
            pos: [128.5, 128.5],
            pressure: 1.0,
            tilt: [0.0; 2],
            t_ms: 0.0,
        });
        let base = vec![0; 256 * 256 * 4];
        let pixels = cpu_paint(
            &base,
            [0.0; 2],
            &dabs,
            config,
            [0.0, 0.0, 0.0, 1.0],
            false,
            None,
        );
        assert_eq!(pixels[(128 * 256 + 128) * 4 + 3], 255);
        assert_eq!(pixels[(129 * 256 + 128) * 4 + 3], 0);
    }

    #[test]
    fn pressure_and_spacing_cross_tile_boundaries() {
        let mut s = StrokeSampler::new(BrushConfig::default()).unwrap();
        let mut d = s.input(StrokeSample {
            pos: [250.0, 8.0],
            pressure: 1.0,
            tilt: [0.0; 2],
            t_ms: 0.0,
        });
        d.extend(s.input(StrokeSample {
            pos: [280.0, 8.0],
            pressure: 0.5,
            tilt: [0.0; 2],
            t_ms: 10.0,
        }));
        assert!(d.iter().any(|d| d.center[0] > 256.0));
        assert!(d.last().unwrap().radius < 12.0);
        assert!(
            d.windows(2)
                .all(|p| (p[1].center[0] - p[0].center[0]).abs() < 4.0)
        );
    }
}
