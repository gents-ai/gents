use serde_json::Value;

const MAX_DEPTH: usize = 32;

#[derive(Clone, Copy)]
enum Container {
    Object,
    Array,
}

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
    use gents_lean_vocab::families::schema_argument_repair_arguments as family;
    use gents_lean_vocab::family::Json;

    #[test]
    fn schema_argument_repair_family_replays_against_repair_arguments() {
        let cases = family::cases();
        assert_eq!(cases.len(), 8);
        for case in &cases {
            let mut arguments = case.input.arguments.0.clone();
            let paths = repair_arguments(&case.input.schema.0, &mut arguments);
            case.check(family::Repaired {
                arguments: Json(arguments),
                paths,
            });
        }
    }

    fn object(value: &std::collections::BTreeMap<String, Value>) -> Value {
        Value::Object(
            value
                .iter()
                .map(|(key, field)| (key.clone(), field.clone()))
                .collect(),
        )
    }

    #[test]
    fn generated_schema_argument_repairs_match_the_executable_model() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().schema_argument_repair_cases;
        assert!(!cases.is_empty());
        for case in cases {
            let mut observations = std::collections::BTreeMap::new();
            for observed in &case.parses {
                let actual = serde_json::from_str::<Value>(&observed.raw).ok();
                let expected = observed
                    .accepted
                    .then(|| observed.parsed.clone().unwrap_or(Value::Null));
                assert_eq!(
                    actual, expected,
                    "{}: native parse {:?}",
                    case.name, observed.raw
                );
                assert!(
                    observations.insert(observed.raw.clone(), actual).is_none(),
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
            let schema = object(&case.schema);
            let mut value = case.input.clone().unwrap_or(Value::Null);
            let paths = repair_with_decoder(&schema, &mut value, &mut parse);
            assert_eq!(
                value,
                case.expected.clone().unwrap_or(Value::Null),
                "{}",
                case.name
            );
            assert_eq!(paths, case.paths, "{}", case.name);
            assert!(
                repair_with_decoder(&schema, &mut value, &mut parse).is_empty(),
                "{}",
                case.name
            );
            assert_eq!(
                value,
                case.expected.clone().unwrap_or(Value::Null),
                "{}: second pass",
                case.name
            );
            let native_schema = serde_json::json!({
                "type": case.native_schema.r#type,
                "properties": { "value": object(&case.native_schema.properties.value) },
                "required": case.native_schema.required,
                "additionalProperties": case.native_schema.additional_properties,
            });
            let mut native = serde_json::json!({ "value": case.native_input.value });
            let paths = repair_with_decoder(&native_schema, &mut native, &mut parse);
            assert_eq!(
                native,
                serde_json::json!({ "value": case.native_expected.value }),
                "{}: tool-call root",
                case.name
            );
            assert_eq!(paths, case.native_paths, "{}: tool-call paths", case.name);
        }
    }
}
