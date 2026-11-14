use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Number, Value};

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

const NUMBER_TOKEN: &str = "$number";

/// A Lean `Json` family field. On the wire a number that is not an `i64` or
/// `u64` integer is `{"$number": token}` (`Conformance.jsonNumberWire`). A
/// token decodes only when serde_json reads it as a float that writes the same
/// token back, so a decoded value re-encodes to its own wire form and
/// `Case::check` compares such numbers by token: `1` and `1.0` differ.
#[derive(Debug, Clone)]
pub struct Json(pub Value);

fn is_number_token(fields: &Map<String, Value>) -> bool {
    fields.len() == 1 && fields.contains_key(NUMBER_TOKEN)
}

fn json_wire(value: &Value) -> Result<Value, String> {
    Ok(match value {
        Value::Number(number) if number.is_f64() => Value::Object(Map::from_iter([(
            NUMBER_TOKEN.to_owned(),
            Value::String(number.to_string()),
        )])),
        Value::Array(items) => Value::Array(items.iter().map(json_wire).collect::<Result<_, _>>()?),
        Value::Object(fields) if is_number_token(fields) => {
            return Err(format!(
                "family JSON object {value} uses the reserved {NUMBER_TOKEN} key"
            ))
        }
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, field)| Ok((key.clone(), json_wire(field)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

fn json_native(value: Value) -> Result<Value, String> {
    Ok(match value {
        Value::Object(fields) if is_number_token(&fields) => {
            let token = fields[NUMBER_TOKEN].as_str().ok_or_else(|| {
                format!(
                    "family JSON number token {} is not a string",
                    fields[NUMBER_TOKEN]
                )
            })?;
            let number: Number = serde_json::from_str(token)
                .map_err(|error| format!("family JSON number {token:?}: {error}"))?;
            if !number.is_f64() || number.to_string() != token {
                return Err(format!(
                    "family JSON number {token:?} has no exact serde_json value"
                ));
            }
            Value::Number(number)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(json_native)
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, field)| Ok((key, json_native(field)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other,
    })
}

impl Serialize for Json {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        json_wire(&self.0)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        json_native(Value::deserialize(deserializer)?)
            .map(Json)
            .map_err(serde::de::Error::custom)
    }
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

    fn schema_path() -> std::path::PathBuf {
        std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("runtime CARGO_MANIFEST_DIR"),
        )
        .join("families.schema.json")
    }

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
    fn json_numbers_compare_by_integer_value_or_token() {
        let native = serde_json::json!({
            "integers": [1, -42, u64::MAX, i64::MIN],
            "decimals": [1.0, -0.5, 12.75],
            "nested": [null, [], {}],
        });
        let wire = serde_json::to_value(Json(native.clone())).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "integers": [1, -42, u64::MAX, i64::MIN],
                "decimals": [{"$number": "1.0"}, {"$number": "-0.5"}, {"$number": "12.75"}],
                "nested": [null, [], {}],
            })
        );
        let decoded: Json = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(decoded.0, native);
        assert_ne!(
            serde_json::to_value(Json(serde_json::json!(1))).unwrap(),
            serde_json::to_value(Json(serde_json::json!(1.0))).unwrap()
        );
        assert_ne!(perturbed(&wire["decimals"][0]), wire["decimals"][0]);
    }

    #[test]
    fn json_tokens_without_an_exact_native_value_are_rejected() {
        for token in ["1.00", "18446744073709551616", "5", "1e1", "x"] {
            let wire = serde_json::json!({ "$number": token });
            assert!(
                serde_json::from_value::<Json>(wire).is_err(),
                "{token} decoded"
            );
        }
        assert!(serde_json::from_value::<Json>(serde_json::json!({ "$number": 1 })).is_err());
        assert!(serde_json::to_value(Json(serde_json::json!({ "$number": "1.0" }))).is_err());
    }

    #[test]
    fn family_schema_is_current() {
        let live = render(&live_schema());
        if std::env::var_os("GENTS_WRITE_FAMILY_SCHEMA").is_some() {
            fs::write(schema_path(), &live).unwrap();
            return;
        }
        assert_eq!(live, CHECKED_IN_SCHEMA);
    }
}
