//! Canonical JSON and strict reading for content-addressed documents.
use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Number, Value};
use std::fmt;
/// The key serde_json uses for a number under `arbitrary_precision`.
const NUMBER_TOKEN: &str = "$serde_json::private::Number";
/// Whether this build's serde_json hands numbers over as token maps. Without that
/// feature, an object whose key is the token is an ordinary object.
fn numbers_are_maps() -> bool {
    static MAPS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MAPS.get_or_init(|| {
        struct Probe;
        impl<'de> Visitor<'de> for Probe {
            type Value = bool;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "a number")
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<bool, E> {
                Ok(false)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<bool, A::Error> {
                while map.next_entry::<String, String>()?.is_some() {}
                Ok(true)
            }
        }
        use serde::Deserializer as _;
        // Integers that fit 64 bits still arrive as integers; a fraction does not.
        serde_json::Deserializer::from_slice(b"0.5")
            .deserialize_any(Probe)
            .unwrap_or(false)
    })
}
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "JSON without duplicate keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(Value::Bool(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(Value::Number(v.into())))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(Value::Number(v.into())))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                Number::from_f64(v)
                    .map(|n| Unique(Value::Number(n)))
                    .ok_or_else(|| E::custom("nonfinite JSON number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(Value::String(v.into())))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Unique, E> {
                Ok(Unique(Value::String(v)))
            }
            fn visit_none<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Unique, A::Error> {
                let mut out = vec![];
                while let Some(Unique(v)) = seq.next_element()? {
                    out.push(v);
                }
                Ok(Unique(Value::Array(out)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Unique, A::Error> {
                let mut out = serde_json::Map::new();
                let mut first = true;
                while let Some(key) = map.next_key::<String>()? {
                    // With `serde_json/arbitrary_precision` (which any crate in the build
                    // can enable), a number arrives as a one-entry map holding its digits.
                    if std::mem::take(&mut first) && key == NUMBER_TOKEN && numbers_are_maps() {
                        let digits: String = map.next_value()?;
                        // Without the feature serde_json refuses a number past f64.
                        if !digits.parse::<f64>().is_ok_and(f64::is_finite) {
                            return Err(de::Error::custom("number out of range"));
                        }
                        let number: Number = digits.parse().map_err(de::Error::custom)?;
                        if map.next_key::<String>()?.is_some() {
                            return Err(de::Error::custom("malformed JSON number"));
                        }
                        return Ok(Unique(Value::Number(number)));
                    }
                    if out.contains_key(&key) {
                        return Err(de::Error::custom(format!("duplicate JSON key {key}")));
                    }
                    let Unique(v) = map.next_value()?;
                    out.insert(key, v);
                }
                Ok(Unique(Value::Object(out)))
            }
        }
        d.deserialize_any(V)
    }
}
pub fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> serde_json::Result<T> {
    let mut d = serde_json::Deserializer::from_slice(bytes);
    let Unique(v) = Unique::deserialize(&mut d)?;
    d.end()?;
    serde_json::from_value(v)
}
pub fn canonical_value<T: Serialize>(value: &T) -> serde_json::Result<Value> {
    fn normalize(v: &mut Value) {
        match v {
            Value::Number(n) if n.is_f64() && n.as_f64() == Some(0.0) => {
                *n = Number::from_f64(0.0).unwrap()
            }
            Value::Array(values) => {
                for v in values {
                    normalize(v);
                }
            }
            Value::Object(values) => {
                for v in values.values_mut() {
                    normalize(v);
                }
            }
            _ => {}
        }
    }
    let mut out = serde_json::to_value(value)?;
    normalize(&mut out);
    Ok(out)
}
pub fn canonical_bytes<T: Serialize>(value: &T) -> serde_json::Result<Vec<u8>> {
    let mut out = serde_json::to_vec_pretty(&canonical_value(value)?)?;
    out.push(b'\n');
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_duplicate_keys_and_trailing_input() {
        assert!(from_slice::<Value>(br#"{"node":{"hash":1,"hash":2}}"#).is_err());
        assert!(from_slice::<Value>(b"{} {}").is_err());
    }
    #[test]
    fn numbers_read_as_numbers() {
        assert_eq!(from_slice::<f64>(b"1.5").unwrap(), 1.5);
        assert_eq!(
            from_slice::<Vec<f64>>(b"[1.5, -2, 3e2]").unwrap(),
            [1.5, -2.0, 300.0]
        );
        assert_eq!(
            from_slice::<u64>(b"18446744073709551615").unwrap(),
            u64::MAX
        );
        assert!(from_slice::<f64>(b"1e999").is_err());
        assert!(from_slice::<Value>(b"[-1e999]").is_err());
        assert_eq!(
            from_slice::<Value>(br#"{"a":0.1}"#).unwrap(),
            serde_json::json!({"a": 0.1})
        );
    }
    #[test]
    fn the_arbitrary_precision_number_token_is_a_number() {
        // With `serde_json/arbitrary_precision`, the deserializer hands a number to
        // `visit_map` as a one-entry map under this token.
        let token = de::value::MapDeserializer::<_, de::value::Error>::new(
            [("$serde_json::private::Number", "1.5")].into_iter(),
        );
        let Unique(v) = Unique::deserialize(token).unwrap();
        if numbers_are_maps() {
            assert_eq!(serde_json::from_value::<f64>(v).unwrap(), 1.5);
            let bad = de::value::MapDeserializer::<_, de::value::Error>::new(
                [("$serde_json::private::Number", "1.5x")].into_iter(),
            );
            assert!(Unique::deserialize(bad).is_err());
        } else {
            // Without the feature an object under that key stays an object.
            assert_eq!(
                v,
                serde_json::json!({"$serde_json::private::Number": "1.5"})
            );
        }
    }
    #[test]
    fn negative_zero_has_one_canonical_form() {
        assert_eq!(
            canonical_bytes(&[-0.0f64]).unwrap(),
            canonical_bytes(&[0.0f64]).unwrap()
        );
    }
}
