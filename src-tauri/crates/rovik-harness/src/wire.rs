//! Strict, bounded JSON and the controller's canonical encoding.

use crate::{Error, Result};
use serde::{
    de::{DeserializeOwned, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::{Map, Value};
use std::fmt;

/// Maximum structured control-plane message size. Blobs have separate limits.
pub const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;

struct Strict(Value);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = Strict;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("unambiguous integer-only JSON")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<Strict, E> {
                Ok(Strict(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Strict, E> {
                Ok(Strict(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Strict, E> {
                Ok(Strict(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<Strict, E> {
                Err(E::custom(
                    "floating-point metadata is forbidden; use a decimal string",
                ))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Strict, E> {
                Ok(Strict(v.into()))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<Strict, E> {
                Ok(Strict(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Strict, E> {
                Ok(Strict(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Strict, A::Error> {
                let mut out = Vec::new();
                while let Some(Strict(value)) = seq.next_element()? {
                    out.push(value);
                }
                Ok(Strict(Value::Array(out)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Strict, A::Error> {
                let mut out = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if out.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!("duplicate key: {key}")));
                    }
                    let Strict(value) = map.next_value()?;
                    out.insert(key, value);
                }
                Ok(Strict(Value::Object(out)))
            }
        }
        deserializer.deserialize_any(JsonVisitor)
    }
}

/// Reject ambiguous keys at every depth, floats, trailing data and large messages.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    crate::require(
        bytes.len() <= MAX_JSON_BYTES,
        "JSON exceeds the control-plane limit",
    )?;
    let Strict(value) =
        serde_json::from_slice(bytes).map_err(|e| Error::new(format!("invalid JSON: {e}")))?;
    serde_json::from_value(value).map_err(|e| Error::new(format!("invalid schema: {e}")))
}

/// Sorted UTF-8 keys, no whitespace or floats. This is not RFC 8785.
pub fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| Error::new(format!("cannot encode artifact: {e}")))?;
    let value: Value = decode(&bytes)?;
    serde_json::to_vec(&value).map_err(|e| Error::new(format!("cannot canonicalize artifact: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ambiguous_json_is_rejected_at_every_depth() {
        for bytes in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":[{"b":0,"b":1}]}"#,
            "1.0",
            "NaN",
            "{} {}",
        ] {
            assert!(decode::<Value>(bytes.as_bytes()).is_err(), "{bytes}");
        }
    }
    #[test]
    fn canonical_identity_is_independent_of_key_order() {
        let a: Value = decode(br#"{"z":0,"a":{"c":3,"b":2}}"#).unwrap();
        assert_eq!(canonical(&a).unwrap(), br#"{"a":{"b":2,"c":3},"z":0}"#);
    }
}
