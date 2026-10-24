#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolServiceFieldError {
    #[error("ToolServiceRegistry field mcp_path must be non-empty")]
    McpPathEmpty,
    #[error("ToolServiceRegistry field {field} must be a string or null, got {value}")]
    NotStringOrNull {
        field: String,
        value: serde_json::Value,
    },
    #[error("ToolServiceRegistry field {field} must be an integer")]
    NotInteger { field: String },
    #[error("ToolServiceRegistry field {field} must be an integer or null, got {value}")]
    NotIntegerOrNull {
        field: String,
        value: serde_json::Value,
    },
}

pub(crate) fn non_empty(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(crate) fn normalize_tool_service_string(value: Option<String>) -> String {
    value.unwrap_or_default().trim().to_string()
}

pub(crate) fn normalize_tool_service_mcp_path(
    value: String,
) -> Result<String, ToolServiceFieldError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ToolServiceFieldError::McpPathEmpty);
    }
    Ok(if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    })
}

pub(in crate::desired_state) fn optional_string_from_value(
    field: &str,
    value: Option<&serde_json::Value>,
) -> Result<Option<String>, ToolServiceFieldError> {
    use serde_json::Value;
    match value {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(value) => Err(ToolServiceFieldError::NotStringOrNull {
            field: field.to_owned(),
            value: value.clone(),
        }),
    }
}

pub(in crate::desired_state) fn optional_i64_from_value(
    field: &str,
    value: Option<&serde_json::Value>,
) -> Result<Option<i64>, ToolServiceFieldError> {
    use serde_json::Value;
    match value {
        Some(Value::Number(value)) => value.as_i64().map(Some).ok_or_else(|| {
            ToolServiceFieldError::NotInteger {
                field: field.to_owned(),
            }
        }),
        Some(Value::Null) | None => Ok(None),
        Some(value) => Err(ToolServiceFieldError::NotIntegerOrNull {
            field: field.to_owned(),
            value: value.clone(),
        }),
    }
}
