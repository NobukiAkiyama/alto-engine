//! Canonical JSON and strict reading for content-addressed documents.
use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Number, Value};
use std::fmt;
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
                while let Some(key) = map.next_key::<String>()? {
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
    fn negative_zero_has_one_canonical_form() {
        assert_eq!(
            canonical_bytes(&[-0.0f64]).unwrap(),
            canonical_bytes(&[0.0f64]).unwrap()
        );
    }
}
