use serde_json::Value;

/// Encodes a complete JSON value in the order used by Lean 4's `Json.compress`.
/// Its red-black-tree fold writes object keys in reverse lexical order; this
/// adapter preserves values and array order while matching that wire premise.
pub fn compact_model_json(value: &Value) -> String {
    match value {
        Value::Object(fields) => {
            let mut entries = fields.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|(left, _), (right, _)| right.cmp(left));
            let fields = entries
                .into_iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).expect("JSON key encodes"),
                        compact_model_json(value)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{fields}}}")
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(compact_model_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => value.to_string(),
    }
}
