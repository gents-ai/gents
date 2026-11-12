use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

const CHECKED_IN_SCHEMA: &str = include_str!("../families.schema.json");

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

pub fn replay_vocabulary<T>(key: &str, all: &[T])
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_shape_is_checked_in("vocabularies", key);
    let lean = &crate::lean_contract_snapshot().family_vocabularies[key]["values"];
    let mutated = !mutated_cases(key, 1).is_empty();
    let lean: Vec<Value> = lean
        .as_array()
        .expect("vocabulary values")
        .iter()
        .map(|name| {
            if mutated {
                perturbed(name)
            } else {
                name.clone()
            }
        })
        .collect();
    for name in &lean {
        let decoded: T = serde_json::from_value(name.clone()).unwrap_or_else(|error| {
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
            lean.contains(&name),
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
    let mutated = mutated_cases(key, family.cases.len());
    family
        .cases
        .iter()
        .enumerate()
        .map(|(index, case)| Case {
            input: serde_json::from_value(case.input.clone())
                .unwrap_or_else(|error| panic!("family {key} input {}: {error}", case.input)),
            key: key.to_owned(),
            raw_input: case.input.clone(),
            expected: if mutated.contains(&index) {
                perturbed(&case.expected)
            } else {
                case.expected.clone()
            },
            output: std::marker::PhantomData,
        })
        .collect()
}

/// A consumer that passes while its expectations are wrong compares nothing.
/// `GENTS_FAMILY_MUTATION=<key>` perturbs every expectation of that family and
/// `<key>#<n>` only case `n` (modulo the case count); the consumer must then
/// fail. Nothing in the repository sets it: the conversion gate does.
const MUTATION_ENV: &str = "GENTS_FAMILY_MUTATION";

fn mutated_cases(key: &str, count: usize) -> std::ops::Range<usize> {
    let Ok(mutation) = std::env::var(MUTATION_ENV) else {
        return 0..0;
    };
    let (family, case) = match mutation.split_once('#') {
        Some((family, case)) => (family, Some(case)),
        None => (mutation.as_str(), None),
    };
    if family != key {
        return 0..0;
    }
    match case {
        None => 0..count,
        Some(case) => {
            let index = case.parse::<usize>().expect("mutated case index") % count;
            index..index + 1
        }
    }
}

fn perturbed(value: &Value) -> Value {
    match value {
        Value::Null => Value::Bool(true),
        Value::Bool(flag) => Value::Bool(!flag),
        Value::Number(number) => match number.as_i64() {
            Some(signed) => Value::from(signed.wrapping_add(1)),
            None => Value::from(number.as_u64().unwrap_or(0).wrapping_add(1)),
        },
        Value::String(text) => Value::String(format!("{text}~")),
        Value::Array(items) if items.is_empty() => Value::Array(vec![Value::Null]),
        Value::Array(items) => Value::Array(items[1..].to_vec()),
        Value::Object(fields) => match fields.iter().next() {
            None => Value::Null,
            Some((name, first)) => {
                let mut fields = fields.clone();
                fields.insert(name.clone(), perturbed(first));
                Value::Object(fields)
            }
        },
    }
}

pub fn recode<T, U>(value: &T) -> U
where
    T: Serialize,
    U: DeserializeOwned,
{
    let wire = serde_json::to_value(value).expect("family value encodes");
    serde_json::from_value(wire.clone())
        .unwrap_or_else(|error| panic!("family value {wire} does not recode: {error}"))
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

    fn render(schema: &BTreeMap<&str, BTreeMap<&str, Value>>) -> String {
        let kinds: Vec<String> = schema
            .iter()
            .map(|(kind, families)| {
                let families: Vec<String> = families
                    .iter()
                    .map(|(key, shape)| format!("  {key:?}: {shape}"))
                    .collect();
                format!(" {kind:?}: {{\n{}\n }}", families.join(",\n"))
            })
            .collect();
        format!("{{\n{}\n}}\n", kinds.join(",\n"))
    }

    #[test]
    fn family_schema_is_current() {
        let live = render(&live_schema());
        if std::env::var_os("GENTS_WRITE_FAMILY_SCHEMA").is_some() {
            fs::write(SCHEMA_PATH, &live).unwrap();
            return;
        }
        assert_eq!(live, CHECKED_IN_SCHEMA);
    }
}
