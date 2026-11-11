use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The checked-in wire shapes `build.rs` generates the family types from.
const CHECKED_IN_SCHEMA: &str = include_str!("../families.schema.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFamily {
    pub input: Value,
    pub output: Value,
    pub cases: Vec<LeanFamilyCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanFamilyCase {
    pub input: Value,
    pub expected: Value,
}

fn live_schema() -> BTreeMap<&'static str, Value> {
    crate::lean_contract_snapshot()
        .families
        .iter()
        .map(|(key, family)| {
            let shape = serde_json::json!({ "input": family.input, "output": family.output });
            (key.as_str(), shape)
        })
        .collect()
}

/// Decodes each Lean input into the owner's types, runs the owner and compares
/// its encoded result with the model's.
pub fn replay<I, O>(key: &str, run: impl Fn(&I) -> O)
where
    I: DeserializeOwned,
    O: Serialize,
{
    let checked_in: BTreeMap<&str, Value> =
        serde_json::from_str(CHECKED_IN_SCHEMA).expect("checked-in family schema");
    assert_eq!(
        live_schema().get(key),
        checked_in.get(key),
        "family {key} changed shape in Lean; regenerate families.schema.json with \
         GENTS_WRITE_FAMILY_SCHEMA=1 cargo test -p gents-lean-vocab family_schema"
    );
    let family = &crate::lean_contract_snapshot().families[key];
    assert!(!family.cases.is_empty(), "family {key} has no cases");
    for case in &family.cases {
        let input: I = serde_json::from_value(case.input.clone())
            .unwrap_or_else(|error| panic!("family {key} input {}: {error}", case.input));
        let actual = serde_json::to_value(run(&input)).expect("family output encodes");
        assert_eq!(actual, case.expected, "family {key} input {}", case.input);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const SCHEMA_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/families.schema.json");

    #[test]
    fn family_schema_is_current() {
        let live = serde_json::to_string_pretty(&live_schema()).unwrap() + "\n";
        if std::env::var_os("GENTS_WRITE_FAMILY_SCHEMA").is_some() {
            fs::write(SCHEMA_PATH, &live).unwrap();
        }
        assert_eq!(live, CHECKED_IN_SCHEMA);
    }
}
