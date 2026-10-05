use serde_json::Value;

const MAX_DEPTH: usize = 32;

#[derive(Clone, Copy)]
enum Container {
    Object,
    Array,
}

/// References, alternatives and conditional schemas remain with the tool's
/// validator. Only an explicit object/array type, optionally nullable, grants
/// permission to decode a string. Untyped payloads and string unions stay opaque.
fn container(schema: &Value) -> Option<Container> {
    let schema = schema.as_object()?;
    if [
        "$ref",
        "$dynamicRef",
        "anyOf",
        "oneOf",
        "allOf",
        "not",
        "if",
        "then",
        "else",
        "prefixItems",
        "patternProperties",
        "dependentSchemas",
        "unevaluatedProperties",
    ]
    .iter()
    .any(|key| schema.contains_key(*key))
    {
        return None;
    }
    let kind = match schema.get("type")? {
        Value::String(kind) => kind.as_str(),
        Value::Array(types) => {
            let mut concrete = types.iter().filter(|kind| kind.as_str() != Some("null"));
            let kind = concrete.next()?.as_str()?;
            if concrete.next().is_some() {
                return None;
            }
            kind
        }
        _ => return None,
    };
    match kind {
        "object" => Some(Container::Object),
        "array" => Some(Container::Array),
        _ => None,
    }
}

fn has_container(kind: Container, value: &Value) -> bool {
    match kind {
        Container::Object => value.is_object(),
        Container::Array => value.is_array(),
    }
}

fn repair(
    depth: usize,
    schema: &Value,
    value: &mut Value,
    path: &str,
    paths: &mut Vec<String>,
    parse: &mut impl FnMut(&str) -> Option<Value>,
) {
    if depth == 0 {
        return;
    }
    let Some(kind) = container(schema) else {
        return;
    };
    if let Value::String(raw) = value {
        if let Some(parsed) = parse(raw) {
            if has_container(kind, &parsed) {
                *value = parsed;
                paths.push(path.to_owned());
            }
        }
    }
    match (kind, value) {
        (Container::Object, Value::Object(fields)) => {
            let mut keys = fields.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                let child_schema = schema
                    .get("properties")
                    .and_then(|properties| properties.get(&key))
                    .or_else(|| schema.get("additionalProperties"));
                let Some(child_schema) = child_schema else {
                    continue;
                };
                let child_path = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                repair(
                    depth - 1,
                    child_schema,
                    fields.get_mut(&key).expect("key came from this object"),
                    &child_path,
                    paths,
                    parse,
                );
            }
        }
        (Container::Array, Value::Array(values)) => {
            if let Some(items) = schema.get("items") {
                for (index, value) in values.iter_mut().enumerate() {
                    repair(
                        depth - 1,
                        items,
                        value,
                        &format!("{path}/{index}"),
                        paths,
                        parse,
                    );
                }
            }
        }
        _ => {}
    }
}

/// Refines `PromptAssembly.SchemaArgumentRepair.run`. The caller supplies the
/// exact schema advertised for this provider attempt, before acceptance and
/// dispatch. Each eligible string gets one strict parse; tool validation still
/// decides whether the repaired value is a legal call.
pub(super) fn repair_arguments(schema: &Value, arguments: &mut Value) -> Vec<String> {
    repair_with_decoder(schema, arguments, &mut |raw| serde_json::from_str(raw).ok())
}

fn repair_with_decoder(
    schema: &Value,
    arguments: &mut Value,
    parse: &mut impl FnMut(&str) -> Option<Value>,
) -> Vec<String> {
    let mut paths = Vec::new();
    repair(MAX_DEPTH, schema, arguments, "", &mut paths, parse);
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Snapshot {
        schema_argument_repair_cases: Vec<Case>,
    }

    #[derive(Deserialize)]
    struct Case {
        name: String,
        schema: Value,
        input: Value,
        expected: Value,
        paths: Vec<String>,
        parses: Vec<ParseObservation>,
        native_schema: Value,
        native_input: Value,
        native_expected: Value,
        native_paths: Vec<String>,
    }

    #[derive(Deserialize)]
    struct ParseObservation {
        raw: String,
        accepted: bool,
        parsed: Value,
    }

    #[test]
    fn generated_schema_argument_repairs_match_the_executable_model() {
        let snapshot: Snapshot = gents_lean_contract::load_contract_snapshot().unwrap();
        assert!(!snapshot.schema_argument_repair_cases.is_empty());
        for case in snapshot.schema_argument_repair_cases {
            let mut observations = std::collections::BTreeMap::new();
            for observed in case.parses {
                let actual = serde_json::from_str::<Value>(&observed.raw).ok();
                let expected = observed.accepted.then_some(observed.parsed);
                assert_eq!(
                    actual, expected,
                    "{}: native parse {:?}",
                    case.name, observed.raw
                );
                assert!(
                    observations.insert(observed.raw, actual).is_none(),
                    "{}: duplicate parse observation",
                    case.name
                );
            }
            let mut parse = |raw: &str| {
                let actual = serde_json::from_str::<Value>(raw).ok();
                assert_eq!(
                    Some(&actual),
                    observations.get(raw),
                    "{}: unmodeled or mismatched native parse {raw:?}",
                    case.name
                );
                actual
            };
            let mut value = case.input;
            let paths = repair_with_decoder(&case.schema, &mut value, &mut parse);
            assert_eq!(value, case.expected, "{}", case.name);
            assert_eq!(paths, case.paths, "{}", case.name);
            assert!(
                repair_with_decoder(&case.schema, &mut value, &mut parse).is_empty(),
                "{}",
                case.name
            );
            assert_eq!(value, case.expected, "{}: second pass", case.name);
            let mut native = case.native_input;
            let paths = repair_with_decoder(&case.native_schema, &mut native, &mut parse);
            assert_eq!(
                native, case.native_expected,
                "{}: tool-call root",
                case.name
            );
            assert_eq!(paths, case.native_paths, "{}: tool-call paths", case.name);
        }
    }
}
