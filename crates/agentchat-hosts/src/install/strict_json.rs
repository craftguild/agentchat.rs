//! Reject duplicate keys before serde's ordinary map representation can lose data.
use super::conflict;
use agentchat_core::Result;
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::{collections::HashSet, fmt};
struct Unique;
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        d.deserialize_any(Check)
    }
}
struct Check;
impl<'de> Visitor<'de> for Check {
    type Value = Unique;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("JSON without duplicate object keys")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> std::result::Result<Unique, M::Error> {
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
            map.next_value::<Unique>()?;
        }
        Ok(Unique)
    }
    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> std::result::Result<Unique, S::Error> {
        while seq.next_element::<Unique>()?.is_some() {}
        Ok(Unique)
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<Unique, E> {
        Ok(Unique)
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<Unique, E> {
        Ok(Unique)
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<Unique, E> {
        Ok(Unique)
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Unique, E> {
        Ok(Unique)
    }
    fn visit_str<E: de::Error>(self, _: &str) -> std::result::Result<Unique, E> {
        Ok(Unique)
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Unique, E> {
        Ok(Unique)
    }
    fn visit_none<E: de::Error>(self) -> std::result::Result<Unique, E> {
        Ok(Unique)
    }
}
pub(super) fn parse(text: &str) -> Result<Value> {
    serde_json::from_str::<Unique>(text)
        .map_err(|_| conflict("Existing JSON is invalid or contains duplicate keys."))?;
    reserved_keys(text)?;
    serde_json::from_str(text)
        .map_err(|_| conflict("Existing JSON cannot be represented without loss."))
}

fn reserved_keys(text: &str) -> Result<()> {
    // serde_json's arbitrary-precision/raw-value deserializers reserve magic
    // object keys. Detect literal keys, including escapes, before Value could
    // reinterpret an object as a scalar and silently lose a foreign setting.
    // Validation above already established correct JSON string/token syntax.
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while index < bytes.len() {
            if bytes[index] == b'\\' {
                index += 2;
                continue;
            }
            if bytes[index] == b'"' {
                index += 1;
                break;
            }
            index += 1;
        }
        let mut following = index;
        while following < bytes.len() && bytes[following].is_ascii_whitespace() {
            following += 1;
        }
        if bytes.get(following) == Some(&b':') {
            let key: String = serde_json::from_str(&text[start..index])
                .map_err(|_| conflict("Invalid JSON key."))?;
            if matches!(
                key.as_str(),
                "$serde_json::private::Number" | "$serde_json::private::RawValue"
            ) {
                return Err(conflict(
                    "Existing JSON contains reserved keys that cannot be round-tripped.",
                ));
            }
        }
    }
    Ok(())
}
