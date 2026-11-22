use serde_json::Value;

use super::{ProjectionContext, ProjectionRedactionMode};

/// Strings naming a conversation's structure, not its text: a redacted
/// transcript still shows who spoke, each part's kind, and which call a result
/// answers.
const CONVERSATION_STRUCTURE_KEYS: &[&str] =
    &["role", "type", "id", "tool_call_id", "call_id", "name"];

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

/// Redacts every string a provider conversation carries (content, reasoning,
/// tool arguments and results) except those under
/// [`CONVERSATION_STRUCTURE_KEYS`].
pub fn redact_conversation(value: Value, mode: ProjectionRedactionMode) -> Value {
    let context = ProjectionContext {
        actor_did: None,
        redaction_mode: mode,
    };
    redact_conversation_value(value, &context)
}

fn redact_conversation_value(value: Value, context: &ProjectionContext) -> Value {
    match value {
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let value = match value {
                        Value::String(_) if CONVERSATION_STRUCTURE_KEYS.contains(&key.as_str()) => {
                            value
                        }
                        value => redact_conversation_value(value, context),
                    };
                    (key, value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| redact_conversation_value(value, context))
                .collect(),
        ),
        Value::String(value) => Value::String(redact_str(&value, context)),
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
