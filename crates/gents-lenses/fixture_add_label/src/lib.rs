use std::collections::HashMap;
use std::error::Error;

use lens_sdk::StreamOption;
use serde_json::Value;

#[cfg(all(feature = "lens-entry", target_arch = "wasm32"))]
lens_sdk::define!(try_transform, try_inverse);

pub fn apply_label(mut doc: HashMap<String, Value>) -> HashMap<String, Value> {
    let needs_label = match doc.get("label") {
        None => true,
        Some(Value::Null) => true,
        Some(Value::String(s)) if s.is_empty() => true,
        Some(_) => false,
    };
    if needs_label {
        let label = doc
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_uppercase())
            .unwrap_or_default();
        doc.insert("label".to_string(), Value::String(label));
    }
    doc
}

pub fn drop_label(mut doc: HashMap<String, Value>) -> HashMap<String, Value> {
    doc.remove("label");
    doc
}

#[cfg_attr(
    not(all(feature = "lens-entry", target_arch = "wasm32")),
    allow(dead_code)
)]
fn try_transform(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    if let Some(item) = iter.next() {
        let input = match item? {
            Some(v) => v,
            None => return Ok(StreamOption::None),
        };
        return Ok(StreamOption::Some(apply_label(input)));
    }
    Ok(StreamOption::EndOfStream)
}

#[cfg_attr(
    not(all(feature = "lens-entry", target_arch = "wasm32")),
    allow(dead_code)
)]
fn try_inverse(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    if let Some(item) = iter.next() {
        let input = match item? {
            Some(v) => v,
            None => return Ok(StreamOption::None),
        };
        return Ok(StreamOption::Some(drop_label(input)));
    }
    Ok(StreamOption::EndOfStream)
}
