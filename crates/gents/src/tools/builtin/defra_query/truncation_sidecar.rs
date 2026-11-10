use serde_json::{json, Value};

use super::MAX_FIELD_STRING_BYTES;

pub(super) fn truncated_fields(rows: &Value) -> Vec<Value> {
    let mut fields = Vec::new();
    for (row, value) in rows.as_array().into_iter().flatten().enumerate() {
        if let Some(object) = value.as_object() {
            for (field, value) in object {
                visit(value, row, field, String::new(), &mut fields);
            }
        }
    }
    fields
}

fn visit(value: &Value, row: usize, field: &str, path: String, fields: &mut Vec<Value>) {
    match value {
        Value::String(text) if text.len() > MAX_FIELD_STRING_BYTES => {
            fields
                .push(json!({"row": row, "field": field, "path": path, "total_bytes": text.len()}));
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                visit(item, row, field, format!("{path}/{index}"), fields);
            }
        }
        Value::Object(object) => {
            for (key, item) in object {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                visit(item, row, field, format!("{path}/{escaped}"), fields);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_every_truncated_value_without_treating_markers_as_evidence() {
        let long = "é".repeat(1001);
        let rows = json!([
            {"body": long, "metadata": {"a/b~c": ["small", "x".repeat(2001)]}},
            {"body": " [truncated: showed 1 of 2 bytes]", "exact": "x".repeat(2000)}
        ]);
        assert_eq!(
            truncated_fields(&rows),
            vec![
                json!({"row":0,"field":"body","path":"","total_bytes":2002}),
                json!({"row":0,"field":"metadata","path":"/a~1b~0c/1","total_bytes":2001}),
            ]
        );
    }
}
