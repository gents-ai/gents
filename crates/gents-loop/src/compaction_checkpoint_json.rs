use std::fmt;

use serde::de::{Error as _, MapAccess, Visitor};
use serde::Deserializer as _;
use serde_json::{Map, Value};

use super::ContinuationCheckpoint;

/// Only the non-guided JSON boundary accepts singleton text for a canonical
/// string-list property. The type's schema selects those properties; unknown
/// fields, mixed arrays and duplicate names still fail canonical validation.
pub(super) fn decode(raw: &str) -> Result<ContinuationCheckpoint, serde_json::Error> {
    let mut decoder = serde_json::Deserializer::from_str(raw);
    let mut object = decoder.deserialize_map(CheckpointObject)?;
    decoder.end()?;
    let schema = schemars::schema_for!(ContinuationCheckpoint);
    if let Some(properties) = schema.as_value()["properties"].as_object() {
        for (name, property) in properties {
            if property["type"] == "array" && property["items"]["type"] == "string" {
                if let Some(value @ Value::String(_)) = object.get_mut(name) {
                    *value = Value::Array(vec![std::mem::take(value)]);
                }
            }
        }
    }
    serde_json::from_value(Value::Object(object))
}

struct CheckpointObject;

impl<'de> Visitor<'de> for CheckpointObject {
    type Value = Map<String, Value>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("one continuation checkpoint object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = Map::new();
        while let Some(name) = map.next_key::<String>()? {
            if object.contains_key(&name) {
                return Err(A::Error::custom(format!(
                    "duplicate checkpoint field {name:?}"
                )));
            }
            object.insert(name, map.next_value()?);
        }
        Ok(object)
    }
}
