use super::*;
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::sync::OnceLock;

pub(super) fn string_list(value: &Value) -> Vec<String> {
    match value {
        Value::String(value) => value
            .split([',', ' '])
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect(),
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

pub(super) fn collect_disabled_skills(
    object: &Map<String, Value>,
    disabled: &mut BTreeSet<String>,
) {
    if let Some(values) = object
        .get("skills")
        .and_then(Value::as_object)
        .and_then(|skills| skills.get("disabled"))
        .and_then(Value::as_array)
    {
        disabled.extend(values.iter().filter_map(Value::as_str).map(sanitize_text));
    }
}

pub(super) fn collect_command_strings(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(command) = object.get("command").and_then(Value::as_str) {
                output.push(command.to_owned());
            }
            for child in object.values() {
                collect_command_strings(child, output);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_command_strings(child, output);
            }
        }
        _ => {}
    }
}

pub(super) fn executable_name(command: &str) -> Option<String> {
    let first = command.split_whitespace().next()?;
    let name = Path::new(first)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(first);
    Some(sanitize_text(name))
}

pub(super) fn safe_endpoint_origin(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    let host = url.host_str()?;
    let mut result = format!("{}://{}", url.scheme(), host);
    if let Some(port) = url.port() {
        result.push_str(&format!(":{port}"));
    }
    Some(sanitize_text(&result))
}

pub(super) fn scalar_summary(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub(super) fn bounded_sanitized_text(value: &str) -> String {
    truncate_text(&redact_text(value), MAX_TEXT_FACT_BYTES)
}

pub(super) fn sanitize_text(value: &str) -> String {
    truncate_text(&redact_text(value), MAX_METADATA_FACT_BYTES)
}

pub(super) fn truncate_text(value: &str, max_bytes: usize) -> String {
    const MARKER: &str = "\n[TRUNCATED]";
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes.saturating_sub(MARKER.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MARKER}", &value[..end])
}

pub(super) fn redact_text(value: &str) -> String {
    static PRIVATE_KEY: OnceLock<Regex> = OnceLock::new();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static PREFIXED_TOKEN: OnceLock<Regex> = OnceLock::new();
    static SECRET_LIKE: OnceLock<Regex> = OnceLock::new();
    static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    let private_key = PRIVATE_KEY.get_or_init(|| {
        Regex::new(r"(?s)-----BEGIN [^-\n]*PRIVATE KEY-----.*?-----END [^-\n]*PRIVATE KEY-----")
            .expect("valid private-key redactor")
    });
    let bearer = BEARER.get_or_init(|| {
        Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{4,}").expect("valid bearer redactor")
    });
    let prefixed_token = PREFIXED_TOKEN.get_or_init(|| {
        Regex::new(r"\b(?:sk|xai|ghp|github_pat)-?[A-Za-z0-9_\-]{8,}\b")
            .expect("valid token redactor")
    });
    let secret_like = SECRET_LIKE.get_or_init(|| {
        Regex::new(
            r"(?i)\b[A-Za-z0-9_-]*(?:api[_-]?key|password|private[_-]?key|secret|token)[A-Za-z0-9_-]{4,}\b",
        )
        .expect("valid secret-like redactor")
    });
    let assignment = ASSIGNMENT.get_or_init(|| {
        Regex::new(
            r#"(?i)([\"']?\b(?:api[_-]?key|access[_-]?token|auth(?:orization)?|bearer[_-]?token|client[_-]?secret|cookie|password|private[_-]?key|refresh[_-]?token|secret|token)\b[\"']?)(\s*[:=]\s*)(?:\"[^\"\n]*\"|'[^'\n]*'|[^\r\n,;}\]]+)"#,
        )
        .expect("valid assignment redactor")
    });
    let value = private_key.replace_all(value, "[REDACTED PRIVATE KEY]");
    let value = assignment.replace_all(&value, "$1$2[REDACTED]");
    let value = bearer.replace_all(&value, "Bearer [REDACTED]");
    let value = prefixed_token.replace_all(&value, "[REDACTED TOKEN]");
    secret_like
        .replace_all(&value, "[REDACTED SECRET-LIKE VALUE]")
        .into_owned()
}
