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

fn live_schema() -> BTreeMap<&'static str, BTreeMap<&'static str, Value>> {
    let snapshot = crate::lean_contract_snapshot();
    let functions = snapshot.families.iter().map(|(key, family)| {
        let shape = serde_json::json!({ "input": family.input, "output": family.output });
        (key.as_str(), shape)
    });
    let vocabularies = snapshot
        .family_vocabularies
        .iter()
        .map(|(key, vocabulary)| (key.as_str(), vocabulary.clone()));
    BTreeMap::from([
        ("functions", functions.collect()),
        ("vocabularies", vocabularies.collect()),
    ])
}

fn assert_shape_is_checked_in(kind: &str, key: &str) {
    let checked_in: BTreeMap<&str, BTreeMap<&str, Value>> =
        serde_json::from_str(CHECKED_IN_SCHEMA).expect("checked-in family schema");
    assert_eq!(
        live_schema()[kind].get(key),
        checked_in[kind].get(key),
        "family {key} changed shape in Lean; regenerate families.schema.json with \
         GENTS_WRITE_FAMILY_SCHEMA=1 cargo test -p gents-lean-vocab family_schema"
    );
}

/// Checks that the production enum and the Lean enum have the same wire names:
/// every Lean name decodes into the production enum, and `all` encodes to
/// exactly the Lean names.
pub fn replay_vocabulary<T>(key: &str, all: &[T])
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_shape_is_checked_in("vocabularies", key);
    let lean = &crate::lean_contract_snapshot().family_vocabularies[key]["values"];
    let lean: Vec<&Value> = lean.as_array().expect("vocabulary values").iter().collect();
    for name in &lean {
        let decoded: T = serde_json::from_value((*name).clone()).unwrap_or_else(|error| {
            panic!("vocabulary {key}: {name} is not a production value: {error}")
        });
        assert!(
            all.contains(&decoded),
            "vocabulary {key}: {decoded:?} is missing from the production list"
        );
    }
    for value in all {
        let name = serde_json::to_value(value).expect("vocabulary value encodes");
        assert!(
            lean.contains(&&name),
            "vocabulary {key}: production {name} is not a Lean value"
        );
    }
    assert_eq!(all.len(), lean.len(), "vocabulary {key} repeats a value");
}

pub struct Case<I, O> {
    pub input: I,
    key: String,
    raw_input: Value,
    expected: Value,
    output: std::marker::PhantomData<fn(O)>,
}

impl<I, O> Case<I, O>
where
    O: Serialize,
{
    pub fn check(&self, actual: O) {
        let actual = serde_json::to_value(actual).expect("family output encodes");
        assert_eq!(
            actual, self.expected,
            "family {} input {}",
            self.key, self.raw_input
        );
    }
}

pub fn cases<I, O>(key: &str) -> Vec<Case<I, O>>
where
    I: DeserializeOwned,
{
    assert_shape_is_checked_in("functions", key);
    let family = &crate::lean_contract_snapshot().families[key];
    assert!(!family.cases.is_empty(), "family {key} has no cases");
    family
        .cases
        .iter()
        .map(|case| Case {
            input: serde_json::from_value(case.input.clone())
                .unwrap_or_else(|error| panic!("family {key} input {}: {error}", case.input)),
            key: key.to_owned(),
            raw_input: case.input.clone(),
            expected: case.expected.clone(),
            output: std::marker::PhantomData,
        })
        .collect()
}

pub fn replay<I, O>(key: &str, run: impl Fn(&I) -> O)
where
    I: DeserializeOwned,
    O: Serialize,
{
    for case in cases::<I, O>(key) {
        case.check(run(&case.input));
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
            return;
        }
        assert_eq!(live, CHECKED_IN_SCHEMA);
    }
}
