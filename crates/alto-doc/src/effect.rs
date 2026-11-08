//! Effects and adjustments, and their validation.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CurveChannel {
    #[default]
    Master,
    Red,
    Green,
    Blue,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurvePoint {
    pub x: f64,
    pub y: f64,
}

/// A layer or adjustment effect. The JSON shape is the document format:
/// `{"type":"gaussian_blur","radius":12}`. Unknown types do not parse. Fields
/// this version does not know are rejected by validation.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    GaussianBlur {
        radius: f64,
        selection: Option<TileMap>,
        extra: serde_json::Map<String, serde_json::Value>,
    },
    ColorAdjust {
        brightness: f64,
        contrast: f64,
        saturation: f64,
        selection: Option<TileMap>,
        extra: serde_json::Map<String, serde_json::Value>,
    },
    Levels {
        in_black: f64,
        in_white: f64,
        gamma: f64,
        out_black: f64,
        out_white: f64,
        selection: Option<TileMap>,
        extra: serde_json::Map<String, serde_json::Value>,
    },
    HueSaturation {
        hue: f64,
        saturation: f64,
        lightness: f64,
        selection: Option<TileMap>,
        extra: serde_json::Map<String, serde_json::Value>,
    },
    Invert {
        selection: Option<TileMap>,
        extra: serde_json::Map<String, serde_json::Value>,
    },
    Curves {
        channel: CurveChannel,
        points: Vec<CurvePoint>,
        selection: Option<TileMap>,
        extra: serde_json::Map<String, serde_json::Value>,
    },
}

fn effect_f64(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    default: f64,
) -> Result<f64, String> {
    let Some(value) = obj.remove(key) else {
        return Ok(default);
    };
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| format!("Invalid {key}"))
}
fn effect_selection(
    obj: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<Option<TileMap>, String> {
    match obj.remove("selection") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value).map_err(|e| e.to_string()),
    }
}
fn effect_from_map(mut obj: serde_json::Map<String, serde_json::Value>) -> Result<Effect, String> {
    let type_name = obj
        .remove("type")
        .and_then(|v| v.as_str().map(str::to_string))
        .ok_or_else(|| "effect type required".to_string())?;
    match type_name.as_str() {
        "gaussian_blur" => Ok(Effect::GaussianBlur {
            radius: effect_f64(&mut obj, "radius", 8.0)?,
            selection: effect_selection(&mut obj)?,
            extra: obj,
        }),
        "color_adjust" => Ok(Effect::ColorAdjust {
            brightness: effect_f64(&mut obj, "brightness", 0.0)?,
            contrast: effect_f64(&mut obj, "contrast", 1.0)?,
            saturation: effect_f64(&mut obj, "saturation", 1.0)?,
            selection: effect_selection(&mut obj)?,
            extra: obj,
        }),
        "levels" => Ok(Effect::Levels {
            in_black: effect_f64(&mut obj, "in_black", 0.0)?,
            in_white: effect_f64(&mut obj, "in_white", 1.0)?,
            gamma: effect_f64(&mut obj, "gamma", 1.0)?,
            out_black: effect_f64(&mut obj, "out_black", 0.0)?,
            out_white: effect_f64(&mut obj, "out_white", 1.0)?,
            selection: effect_selection(&mut obj)?,
            extra: obj,
        }),
        "hue_saturation" => Ok(Effect::HueSaturation {
            hue: effect_f64(&mut obj, "hue", 0.0)?,
            saturation: effect_f64(&mut obj, "saturation", 1.0)?,
            lightness: effect_f64(&mut obj, "lightness", 0.0)?,
            selection: effect_selection(&mut obj)?,
            extra: obj,
        }),
        "invert" => Ok(Effect::Invert {
            selection: effect_selection(&mut obj)?,
            extra: obj,
        }),
        "curves" => {
            let channel = match obj.remove("channel") {
                None => CurveChannel::Master,
                Some(value) => serde_json::from_value(value).map_err(|e| e.to_string())?,
            };
            let points = serde_json::from_value(
                obj.remove("points")
                    .ok_or_else(|| "Curve points required".to_string())?,
            )
            .map_err(|e| e.to_string())?;
            Ok(Effect::Curves {
                channel,
                points,
                selection: effect_selection(&mut obj)?,
                extra: obj,
            })
        }
        _ => Err(format!("unknown effect type {type_name}")),
    }
}
impl Serialize for Effect {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serde_json::Map::new();
        let (type_name, selection, extra) = match self {
            Self::GaussianBlur {
                radius,
                selection,
                extra,
            } => {
                map.insert("radius".into(), serde_json::json!(radius));
                ("gaussian_blur", selection, extra)
            }
            Self::ColorAdjust {
                brightness,
                contrast,
                saturation,
                selection,
                extra,
            } => {
                map.insert("brightness".into(), serde_json::json!(brightness));
                map.insert("contrast".into(), serde_json::json!(contrast));
                map.insert("saturation".into(), serde_json::json!(saturation));
                ("color_adjust", selection, extra)
            }
            Self::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
                selection,
                extra,
            } => {
                map.insert("in_black".into(), serde_json::json!(in_black));
                map.insert("in_white".into(), serde_json::json!(in_white));
                map.insert("gamma".into(), serde_json::json!(gamma));
                map.insert("out_black".into(), serde_json::json!(out_black));
                map.insert("out_white".into(), serde_json::json!(out_white));
                ("levels", selection, extra)
            }
            Self::HueSaturation {
                hue,
                saturation,
                lightness,
                selection,
                extra,
            } => {
                map.insert("hue".into(), serde_json::json!(hue));
                map.insert("saturation".into(), serde_json::json!(saturation));
                map.insert("lightness".into(), serde_json::json!(lightness));
                ("hue_saturation", selection, extra)
            }
            Self::Invert { selection, extra } => ("invert", selection, extra),
            Self::Curves {
                channel,
                points,
                selection,
                extra,
            } => {
                map.insert(
                    "channel".into(),
                    serde_json::to_value(channel).map_err(serde::ser::Error::custom)?,
                );
                map.insert(
                    "points".into(),
                    serde_json::to_value(points).map_err(serde::ser::Error::custom)?,
                );
                ("curves", selection, extra)
            }
        };
        map.insert("type".into(), serde_json::Value::String(type_name.into()));
        if let Some(selection) = selection {
            map.insert(
                "selection".into(),
                serde_json::to_value(selection).map_err(serde::ser::Error::custom)?,
            );
        }
        for (key, value) in extra {
            map.entry(key.clone()).or_insert_with(|| value.clone());
        }
        serde_json::Value::Object(map).serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Effect {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let obj = match value {
            serde_json::Value::Object(map) => map,
            _ => return Err(serde::de::Error::custom("effect must be an object")),
        };
        effect_from_map(obj).map_err(serde::de::Error::custom)
    }
}

impl Effect {
    pub fn selection(&self) -> Option<&TileMap> {
        match self {
            Self::GaussianBlur { selection, .. }
            | Self::ColorAdjust { selection, .. }
            | Self::Levels { selection, .. }
            | Self::HueSaturation { selection, .. }
            | Self::Invert { selection, .. }
            | Self::Curves { selection, .. } => selection.as_ref(),
        }
    }
    pub fn selection_mut(&mut self) -> &mut Option<TileMap> {
        match self {
            Self::GaussianBlur { selection, .. }
            | Self::ColorAdjust { selection, .. }
            | Self::Levels { selection, .. }
            | Self::HueSaturation { selection, .. }
            | Self::Invert { selection, .. }
            | Self::Curves { selection, .. } => selection,
        }
    }
    pub fn validate(&self) -> Result<(), DocError> {
        let extra = match self {
            Self::GaussianBlur { extra, .. }
            | Self::ColorAdjust { extra, .. }
            | Self::Levels { extra, .. }
            | Self::HueSaturation { extra, .. }
            | Self::Invert { extra, .. }
            | Self::Curves { extra, .. } => extra,
        };
        if !extra.is_empty() {
            return Err(DocError::Unsupported("unknown effect fields".into()));
        }
        let finite = |name: &str, n: f64, min: f64, max: f64| {
            if n.is_finite() && (min..=max).contains(&n) {
                Ok(())
            } else {
                Err(DocError::Invalid(format!("Invalid {name}")))
            }
        };
        match self {
            Self::GaussianBlur { radius, .. } => finite("radius", *radius, 0.0, 64.0)?,
            Self::ColorAdjust {
                brightness,
                contrast,
                saturation,
                ..
            } => {
                finite("brightness", *brightness, -1.0, 1.0)?;
                finite("contrast", *contrast, 0.0, 4.0)?;
                finite("saturation", *saturation, 0.0, 4.0)?;
            }
            Self::Levels {
                in_black,
                in_white,
                gamma,
                out_black,
                out_white,
                ..
            } => {
                finite("in_black", *in_black, 0.0, 1.0)?;
                finite("in_white", *in_white, 0.0, 1.0)?;
                finite("gamma", *gamma, 0.1, 10.0)?;
                finite("out_black", *out_black, 0.0, 1.0)?;
                finite("out_white", *out_white, 0.0, 1.0)?;
                if in_black >= in_white {
                    return Err(DocError::Invalid("Invalid levels range".into()));
                }
            }
            Self::HueSaturation {
                hue,
                saturation,
                lightness,
                ..
            } => {
                finite("hue", *hue, -180.0, 180.0)?;
                finite("saturation", *saturation, 0.0, 4.0)?;
                finite("lightness", *lightness, -1.0, 1.0)?;
            }
            Self::Invert { .. } => {}
            Self::Curves { points, .. } => {
                if !(2..=16).contains(&points.len()) {
                    return Err(DocError::Invalid("Curve point count".into()));
                }
                let mut previous = -1.0;
                for point in points {
                    if !(0.0..=1.0).contains(&point.x)
                        || !(0.0..=1.0).contains(&point.y)
                        || point.x <= previous
                    {
                        return Err(DocError::Invalid("Invalid curve point".into()));
                    }
                    previous = point.x;
                }
            }
        }
        if let Some(map) = self.selection() {
            validate_mask(map)?;
        }
        Ok(())
    }
}

pub fn validate_effect(effect: &Effect) -> Result<(), DocError> {
    effect.validate()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> Editor {
        Editor::new(Document::new(DocumentMeta::srgb_u8(1920, 1080)).unwrap())
    }

    #[test]
    fn known_effect_defaults_parse_but_unknown_fields_are_rejected() {
        let shapes = [
            r#"{"type":"gaussian_blur","radius":12}"#,
            r#"{"type":"gaussian_blur"}"#,
            r#"{"type":"gaussian_blur","radius":12,"legacy":true}"#,
            r#"{"type":"color_adjust","brightness":0.25,"contrast":1.5,"saturation":0.5}"#,
            r#"{"type":"color_adjust"}"#,
            r#"{"type":"levels","in_black":0.1,"in_white":0.9,"gamma":1.2,"out_black":0.0,"out_white":1.0}"#,
            r#"{"type":"hue_saturation","hue":20.0,"saturation":1.25,"lightness":-0.2}"#,
            r#"{"type":"invert"}"#,
            r#"{"type":"invert","selection":null,"note":"stored on the untyped format"}"#,
            r#"{"type":"curves","points":[{"x":0.0,"y":0.0},{"x":1.0,"y":1.0}]}"#,
            r#"{"type":"curves","channel":"red","points":[{"x":0,"y":0},{"x":0.5,"y":0.25},{"x":1,"y":1}]}"#,
        ];
        for raw in shapes {
            let effect: Effect = serde_json::from_str(raw).unwrap();
            if raw.contains("legacy") || raw.contains("note") {
                assert!(effect.validate().is_err());
                continue;
            }
            effect.validate().unwrap();
            let again: Effect =
                serde_json::from_str(&serde_json::to_string(&effect).unwrap()).unwrap();
            assert_eq!(effect, again, "{raw}");
        }
        assert_eq!(
            serde_json::from_str::<Effect>(r#"{"type":"gaussian_blur"}"#).unwrap(),
            Effect::GaussianBlur {
                radius: 8.0,
                selection: None,
                extra: Default::default(),
            }
        );
        assert_eq!(
            serde_json::from_str::<Effect>(
                r#"{"type":"curves","points":[{"x":0,"y":0},{"x":1,"y":1}]}"#
            )
            .unwrap(),
            Effect::Curves {
                channel: CurveChannel::Master,
                points: vec![CurvePoint { x: 0.0, y: 0.0 }, CurvePoint { x: 1.0, y: 1.0 }],
                selection: None,
                extra: Default::default(),
            }
        );
        assert!(serde_json::from_str::<Effect>(r#"{"type":"not_an_effect"}"#).is_err());

        let mut editor = editor();
        let root = editor.doc.root;
        let mut layer = Node::raster("Old");
        layer.props.effects = vec![Effect::GaussianBlur {
            radius: 12.0,
            selection: None,
            extra: Default::default(),
        }];
        editor
            .transact(
                "add",
                vec![Op::InsertNode {
                    node: layer,
                    parent: root,
                    index: 0,
                }],
                0,
            )
            .unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&editor.doc.snapshot_bytes().unwrap()).unwrap();
        let nodes = value["nodes"].as_array_mut().unwrap();
        let layer_json = nodes.iter_mut().find(|node| node["name"] == "Old").unwrap();
        layer_json["props"]["effects"][0]["vendor"] = serde_json::json!("alto-0.3");
        let error =
            Document::from_snapshot_bytes(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(error.to_string().contains("unknown effect fields"));
    }
}
