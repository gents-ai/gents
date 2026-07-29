//! Fixture lens: additive field migration template.
//!
//! Forward: set `label` from `name` (uppercased) when missing/null.
//! Inverse: drop `label`.
//!
//! Unit-test the pure transform; wasm entry is gated for host tests.

use std::collections::HashMap;
use std::error::Error;

use lens_sdk::StreamOption;
use serde_json::Value;

#[cfg(all(feature = "lens-entry", target_arch = "wasm32"))]
lens_sdk::define!(try_transform, try_inverse);

/// Pure forward transform used by the wasm entry and unit tests.
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

/// Pure inverse: remove the added field.
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn forward_sets_uppercase_label_from_name() {
        let mut doc = HashMap::new();
        doc.insert("name".into(), json!("alice"));
        let out = apply_label(doc);
        assert_eq!(out.get("label"), Some(&json!("ALICE")));
    }

    #[test]
    fn forward_preserves_existing_label() {
        let mut doc = HashMap::new();
        doc.insert("name".into(), json!("alice"));
        doc.insert("label".into(), json!("keep"));
        let out = apply_label(doc);
        assert_eq!(out.get("label"), Some(&json!("keep")));
    }

    #[test]
    fn inverse_drops_label() {
        let mut doc = HashMap::new();
        doc.insert("name".into(), json!("alice"));
        doc.insert("label".into(), json!("ALICE"));
        let out = drop_label(doc);
        assert!(!out.contains_key("label"));
        assert_eq!(out.get("name"), Some(&json!("alice")));
    }
}
