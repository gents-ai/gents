// Shared transport for the two version-specific workspace migration modules.
use std::collections::HashMap;
use std::error::Error;

use lens_sdk::StreamOption;
use serde_json::Value;

#[cfg(all(feature = "lens-entry", target_arch = "wasm32"))]
lens_sdk::define!(try_transform, try_inverse);

/// This field did not exist in the registered source schema. Always stamp the
/// migration value; do not accept an injected value as a source-version grant.
pub fn forward(mut doc: HashMap<String, Value>) -> HashMap<String, Value> {
    doc.insert(FIELD.to_owned(), Value::String(LEGACY_VALUE.to_owned()));
    doc
}

pub fn remove_legacy_field(mut doc: HashMap<String, Value>) -> HashMap<String, Value> {
    doc.remove(FIELD);
    doc
}

#[cfg_attr(
    not(all(feature = "lens-entry", target_arch = "wasm32")),
    allow(dead_code)
)]
fn try_transform(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    match iter.next() {
        Some(item) => Ok(match item? {
            Some(doc) => StreamOption::Some(forward(doc)),
            None => StreamOption::None,
        }),
        None => Ok(StreamOption::EndOfStream),
    }
}

#[cfg_attr(
    not(all(feature = "lens-entry", target_arch = "wasm32")),
    allow(dead_code)
)]
fn try_inverse(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    match iter.next() {
        Some(item) => Ok(match item? {
            Some(doc) => StreamOption::Some(remove_legacy_field(doc)),
            None => StreamOption::None,
        }),
        None => Ok(StreamOption::EndOfStream),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_version_stamps_explicit_compatibility_and_preserves_identity() {
        let mut input = HashMap::new();
        input.insert(
            "workspace_id".to_owned(),
            Value::String("legacy".to_owned()),
        );
        let migrated = forward(input.clone());
        assert_eq!(
            migrated.get(FIELD),
            Some(&Value::String(LEGACY_VALUE.to_owned()))
        );
        assert_eq!(remove_legacy_field(migrated.clone()), input);
        assert_eq!(forward(migrated.clone()), migrated);
    }

    #[test]
    fn source_version_does_not_accept_an_injected_capability() {
        let input = HashMap::from([(FIELD.to_owned(), Value::String("forged".to_owned()))]);
        assert_eq!(
            forward(input).get(FIELD),
            Some(&Value::String(LEGACY_VALUE.to_owned()))
        );
    }
}
