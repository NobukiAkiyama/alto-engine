use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct StrokeSample {
    pub pos: [f32; 2],
    pub pressure: f32,
    pub tilt: [f32; 2],
    pub t_ms: f64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Dab {
    pub center: [f32; 2],
    pub radius: f32,
    pub opacity: f32,
    /// Rotation in radians, in layer coordinates.
    pub angle: f32,
    #[serde(default = "round_tip")]
    pub roundness: f32,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct BrushConfig {
    pub size: f32,
    pub hardness: f32,
    pub spacing: f32,
    pub opacity: f32,
    pub flow: f32,
    pub pressure_size: bool,
    pub pressure_opacity: bool,
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
            pressure_size: true,
            pressure_opacity: false,
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
}
impl StrokeSampler {
    pub fn new(config: BrushConfig) -> Self {
        Self {
            config,
            last: None,
            previous: None,
            until: 0.0,
        }
    }
    fn dab(&self, pos: [f32; 2], pressure: f32) -> Dab {
        Dab {
            center: pos,
            radius: self.config.size
                * 0.5
                * if self.config.pressure_size {
                    pressure.max(0.05)
                } else {
                    1.0
                },
            opacity: self.config.flow
                * if self.config.pressure_opacity {
                    pressure
                } else {
                    1.0
                },
            angle: self.config.angle.to_radians(),
            roundness: self.config.roundness,
        }
    }
    pub fn input(&mut self, sample: StrokeSample) -> Vec<Dab> {
        if !sample.pos.iter().all(|v| v.is_finite()) || !sample.pressure.is_finite() {
            return vec![];
        }
        let sample = StrokeSample {
            pressure: sample.pressure.clamp(0.0, 1.0),
            ..sample
        };
        let Some(last) = self.last else {
            self.last = Some(sample);
            self.until = (self.config.size * self.config.spacing).max(0.25);
            return vec![self.dab(sample.pos, sample.pressure)];
        };
        let prev = self.previous.unwrap_or(last);
        let length =
            ((sample.pos[0] - last.pos[0]).powi(2) + (sample.pos[1] - last.pos[1]).powi(2)).sqrt();
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
                    out.push(self.dab([from[0] + dx * a, from[1] + dy * a], pressure));
                    self.until = (self.config.size
                        * self.config.spacing
                        * if self.config.pressure_size {
                            pressure.max(0.05)
                        } else {
                            1.0
                        })
                    .max(0.25);
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
pub fn cpu_paint(
    base: &[u8],
    origin: [f32; 2],
    dabs: &[Dab],
    config: BrushConfig,
    color: [f32; 4],
    erase: bool,
) -> Vec<u8> {
    let mut out = base.to_vec();
    let rotations: Vec<_> = dabs
        .iter()
        .map(|d| (d.roundness < 1.0).then(|| d.angle.sin_cos()))
        .collect();
    for y in 0..256 {
        for x in 0..256 {
            let mut coverage = 0.0;
            let p = [origin[0] + x as f32 + 0.5, origin[1] + y as f32 + 0.5];
            for (d, rotation) in dabs.iter().zip(&rotations) {
                let dx = p[0] - d.center[0];
                let dy = p[1] - d.center[1];
                let dist = if let Some((sin, cos)) = rotation {
                    let u = dx * cos + dy * sin;
                    let v = (-dx * sin + dy * cos) / d.roundness.max(0.01);
                    (u.powi(2) + v.powi(2)).sqrt()
                } else {
                    (dx.powi(2) + dy.powi(2)).sqrt()
                } / d.radius.max(0.01);
                let t =
                    ((dist - config.hardness) / (1.0 - config.hardness).max(0.001)).clamp(0.0, 1.0);
                let shape = 1.0 - t * t * (3.0 - 2.0 * t);
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
                ..config
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
        let painted = cpu_paint(&base, [0.0; 2], &[dab], config, [0.0, 0.0, 0.0, 1.0], false);
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
        let mut sampler = StrokeSampler::new(config);
        let dabs = sampler.input(StrokeSample {
            pos: [128.5, 128.5],
            pressure: 1.0,
            tilt: [0.0; 2],
            t_ms: 0.0,
        });
        let base = vec![0; 256 * 256 * 4];
        let pixels = cpu_paint(&base, [0.0; 2], &dabs, config, [0.0, 0.0, 0.0, 1.0], false);
        assert_eq!(pixels[(128 * 256 + 128) * 4 + 3], 255);
        assert_eq!(pixels[(129 * 256 + 128) * 4 + 3], 0);
    }

    #[test]
    fn pressure_and_spacing_cross_tile_boundaries() {
        let mut s = StrokeSampler::new(BrushConfig::default());
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
