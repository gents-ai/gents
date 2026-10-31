use serde_json::Value;

use super::{ProjectionContext, ProjectionRedactionMode};

pub(super) fn redact_option(value: Option<&str>, context: &ProjectionContext) -> Option<String> {
    value.map(|value| redact_str(value, context))
}

pub(super) fn redact_str(value: &str, context: &ProjectionContext) -> String {
    match context.redaction_mode {
        ProjectionRedactionMode::Full => value.to_string(),
        ProjectionRedactionMode::TrainingSafe => redact_training_safe(value),
        ProjectionRedactionMode::Public => "[redacted]".to_string(),
    }
}

pub(super) fn redact_json_value(value: Value, context: &ProjectionContext) -> Value {
    match value {
        Value::String(value) => Value::String(redact_str(&value, context)),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| redact_json_value(value, context))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, redact_json_value(value, context)))
                .collect(),
        ),
        value => value,
    }
}

fn redact_training_safe(value: &str) -> String {
    if value.trim().is_empty() {
        String::new()
    } else {
        "[training_safe_redacted]".to_string()
    }
}
