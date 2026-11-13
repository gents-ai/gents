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

    #[test]
    fn schema_argument_repairs_replay_against_the_native_decoder() {
        gents_lean_vocab::families::schema_argument_repair_tool_arguments::replay(|case| {
            for observed in &case.parses {
                assert_eq!(
                    serde_json::from_str::<Value>(&observed.raw).ok(),
                    observed.accepted.then(|| observed.parsed.0.clone()),
                    "{}: native decoder premise {:?}",
                    case.name,
                    observed.raw
                );
            }
            let mut parse = |raw: &str| {
                assert!(
                    case.parses.iter().any(|observed| observed.raw == raw),
                    "{}: unmodeled native parse {raw:?}",
                    case.name
                );
                serde_json::from_str::<Value>(raw).ok()
            };
            let mut value = case.input.0.clone();
            let paths = repair_with_decoder(&case.schema.0, &mut value, &mut parse);
            let mut repeated = value.clone();
            let repeated_paths = repair_with_decoder(&case.schema.0, &mut repeated, &mut parse);
            let mut native = case.native_input.0.clone();
            let native_paths = repair_with_decoder(&case.native_schema.0, &mut native, &mut parse);
            gents_lean_vocab::families::schema_argument_repair_tool_arguments::ToolArgumentsOutput {
                expected: Json(value),
                paths,
                repeated: Json(repeated),
                repeated_paths,
                native_expected: Json(native),
                native_paths,
            }
        });
    }
}
