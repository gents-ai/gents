use std::time::Duration;

use anyhow::{anyhow, Context as _};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::health_checker::HealthStatus;

use super::shared::{
    enforce_health_gate, extract_text, lookup_service, MetaToolContext, MetaToolError,
};

#[derive(Debug, Deserialize)]
pub struct CallToolArgs {
    service_id: String,
    tool_name: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct StructuredToolError {
    ok: bool,
    failure_class: &'static str,
    path: String,
    message: String,
    retryable: bool,
    service_id: String,
    tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    available_tools: Option<Vec<String>>,
}

#[derive(Clone)]
pub struct CallToolTool {
    ctx: MetaToolContext,
}

impl CallToolTool {
    pub(crate) fn new(ctx: MetaToolContext) -> Self {
        Self { ctx }
    }
}

impl Tool for CallToolTool {
    const NAME: &'static str = "call_tool";

    type Error = MetaToolError;
    type Args = CallToolArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Invoke a tool on a data service via MCP. Use discover_tools to \
                find available services and tools, then describe_tool to get the input \
                schema, then call_tool with the correct arguments."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "service_id": {
                        "type": "string",
                        "description": "The service_id of the data service."
                    },
                    "tool_name": {
                        "type": "string",
                        "description": "The name of the tool to invoke."
                    },
                    "arguments": {
                        "type": "object",
                        "description": "The arguments to pass to the tool (see describe_tool for schema)."
                    }
                },
                "required": ["service_id", "tool_name", "arguments"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let arguments =
            match normalize_arguments(&args.service_id, &args.tool_name, &args.arguments) {
                Ok(arguments) => arguments,
                Err(error) => return Ok(error.to_result_text()),
            };

        let Value::Object(argument_object) = &arguments else {
            unreachable!("normalize_arguments only returns object arguments")
        };

        let health = enforce_health_gate(&self.ctx.health, &args.service_id)
            .await
            .context("call_tool")?;

        let endpoint = lookup_service(&self.ctx, &args.service_id)
            .await
            .context("call_tool")?;

        let timeout_secs = if matches!(health.as_ref().map(|h| h.status), Some(HealthStatus::Stale))
        {
            120
        } else {
            300
        };

        if let Some(error) = self
            .preflight_arguments(
                &args.service_id,
                &endpoint,
                &args.tool_name,
                argument_object,
            )
            .await
        {
            return Ok(error.to_result_text());
        }

        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            self.ctx
                .mcp_pool
                .call_tool(&args.service_id, &endpoint, &args.tool_name, arguments),
        )
        .await
        .map_err(|_| {
            anyhow!(
                "service '{}' timed out after {}s",
                args.service_id,
                timeout_secs
            )
        })?
        .context("MCP call_tool")?;

        if result.is_error == Some(true) {
            let error_text = extract_text(&result);
            return Err(anyhow!(
                "tool '{}' on service '{}' returned an error: {}",
                args.tool_name,
                args.service_id,
                if error_text.is_empty() {
                    "(no error message)".to_string()
                } else {
                    error_text
                }
            )
            .into());
        }

        let text = extract_text(&result);
        Ok(if text.is_empty() {
            "(tool returned no text content)".to_string()
        } else {
            text
        })
    }
}

impl CallToolTool {
    async fn preflight_arguments(
        &self,
        service_id: &str,
        endpoint: &str,
        tool_name: &str,
        arguments: &Map<String, Value>,
    ) -> Option<StructuredToolError> {
        let list_result = match tokio::time::timeout(
            Duration::from_secs(30),
            self.ctx.mcp_pool.list_tools(service_id, endpoint),
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                tracing::warn!(
                    service_id,
                    tool_name,
                    error = %error,
                    "skipping MCP argument preflight after list_tools failure"
                );
                return None;
            }
            Err(_) => {
                tracing::warn!(
                    service_id,
                    tool_name,
                    "skipping MCP argument preflight after list_tools timeout"
                );
                return None;
            }
        };

        let available_tools = list_result
            .tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();
        let Some(tool) = list_result.tools.iter().find(|tool| tool.name == tool_name) else {
            return Some(StructuredToolError::tool_not_found(
                service_id,
                tool_name,
                available_tools,
            ));
        };

        validate_arguments_against_schema(
            service_id,
            tool_name,
            arguments,
            tool.input_schema.as_ref(),
        )
        .err()
    }
}

impl StructuredToolError {
    fn invalid_tool_arguments(
        service_id: &str,
        tool_name: &str,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            ok: false,
            failure_class: "invalid_tool_arguments",
            path: path.into(),
            message: message.into(),
            retryable: true,
            service_id: service_id.to_string(),
            tool_name: tool_name.to_string(),
            available_tools: None,
        }
    }

    fn tool_not_found(service_id: &str, tool_name: &str, available_tools: Vec<String>) -> Self {
        Self {
            ok: false,
            failure_class: "tool_not_found",
            path: "/tool_name".to_string(),
            message: format!("tool '{tool_name}' was not found on service '{service_id}'"),
            retryable: true,
            service_id: service_id.to_string(),
            tool_name: tool_name.to_string(),
            available_tools: Some(available_tools),
        }
    }

    fn to_result_text(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| {
            format!(
                r#"{{"ok":false,"failure_class":"{}","path":"{}","message":"{}","retryable":true,"service_id":"{}","tool_name":"{}"}}"#,
                self.failure_class, self.path, self.message, self.service_id, self.tool_name
            )
        })
    }
}

fn normalize_arguments(
    service_id: &str,
    tool_name: &str,
    arguments: &Value,
) -> Result<Value, StructuredToolError> {
    match arguments {
        Value::Object(_) => Ok(arguments.clone()),
        Value::String(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(Value::Object(map)) => Ok(Value::Object(map)),
            Ok(other) => Err(StructuredToolError::invalid_tool_arguments(
                service_id,
                tool_name,
                "/arguments",
                format!(
                    "call_tool.arguments must decode to a JSON object, got {}",
                    json_type_name(&other)
                ),
            )),
            Err(error) => Err(StructuredToolError::invalid_tool_arguments(
                service_id,
                tool_name,
                "/arguments",
                format!(
                    "call_tool.arguments must be a JSON object or stringified JSON object: {error}"
                ),
            )),
        },
        other => Err(StructuredToolError::invalid_tool_arguments(
            service_id,
            tool_name,
            "/arguments",
            format!(
                "call_tool.arguments must be a JSON object, got {}",
                json_type_name(other)
            ),
        )),
    }
}

fn validate_arguments_against_schema(
    service_id: &str,
    tool_name: &str,
    arguments: &Map<String, Value>,
    schema: &Map<String, Value>,
) -> Result<(), StructuredToolError> {
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for raw_field in required {
            let Some(field) = raw_field.as_str() else {
                continue;
            };
            if !arguments.contains_key(field) {
                return Err(StructuredToolError::invalid_tool_arguments(
                    service_id,
                    tool_name,
                    format!("/arguments/{field}"),
                    format!("missing required argument '{field}'"),
                ));
            }
        }
    }

    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };

    if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
        for field in arguments.keys() {
            if !properties.contains_key(field) {
                return Err(StructuredToolError::invalid_tool_arguments(
                    service_id,
                    tool_name,
                    format!("/arguments/{field}"),
                    format!("unknown argument '{field}'"),
                ));
            }
        }
    }

    for (field, field_schema) in properties {
        let Some(value) = arguments.get(field) else {
            continue;
        };
        let Some(expected_types) = schema_types(field_schema) else {
            continue;
        };
        if expected_types
            .iter()
            .any(|expected| value_matches_schema_type(value, expected))
        {
            continue;
        }

        return Err(StructuredToolError::invalid_tool_arguments(
            service_id,
            tool_name,
            format!("/arguments/{field}"),
            format!(
                "argument '{field}' must be {}, got {}",
                expected_types.join(" or "),
                json_type_name(value)
            ),
        ));
    }

    Ok(())
}

fn schema_types(schema: &Value) -> Option<Vec<String>> {
    match schema.get("type") {
        Some(Value::String(value)) => Some(vec![value.to_string()]),
        Some(Value::Array(values)) => {
            let types = values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            (!types.is_empty()).then_some(types)
        }
        _ => None,
    }
}

fn value_matches_schema_type(value: &Value, expected: &str) -> bool {
    match expected {
        "array" => value.is_array(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "object" => value.is_object(),
        "string" => value.is_string(),
        _ => true,
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Array(_) => "array",
        Value::Bool(_) => "boolean",
        Value::Null => "null",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::Object(_) => "object",
        Value::String(_) => "string",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn search_schema() -> Map<String, Value> {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "limit": { "type": "integer" }
            },
            "required": ["query"]
        })
        .as_object()
        .expect("object schema")
        .clone()
    }

    #[test]
    fn rejects_missing_required_field_from_mcp_schema() {
        let arguments = json!({}).as_object().expect("arguments").clone();
        let error = validate_arguments_against_schema(
            "x-data",
            "search_bookmarks",
            &arguments,
            &search_schema(),
        )
        .expect_err("missing query should fail");

        assert_eq!(error.failure_class, "invalid_tool_arguments");
        assert_eq!(error.path, "/arguments/query");
        assert!(error.retryable);
        assert_eq!(error.service_id, "x-data");
        assert_eq!(error.tool_name, "search_bookmarks");
    }

    #[test]
    fn rejects_non_object_arguments_before_mcp_call() {
        let error = normalize_arguments("x-data", "search_bookmarks", &json!("query=amy"))
            .expect_err("plain string arguments should fail");

        assert_eq!(error.failure_class, "invalid_tool_arguments");
        assert_eq!(error.path, "/arguments");
        assert!(error.message.contains("stringified JSON object"));
    }

    #[test]
    fn accepts_stringified_json_object_for_compatibility() {
        let arguments =
            normalize_arguments("x-data", "search_bookmarks", &json!("{\"query\":\"amy\"}"))
                .expect("stringified object should normalize");

        assert_eq!(arguments, json!({ "query": "amy" }));
    }

    #[test]
    fn rejects_obvious_schema_type_mismatch() {
        let arguments = json!({ "query": "amy", "limit": "5" })
            .as_object()
            .expect("arguments")
            .clone();
        let error = validate_arguments_against_schema(
            "x-data",
            "search_bookmarks",
            &arguments,
            &search_schema(),
        )
        .expect_err("string limit should fail");

        assert_eq!(error.failure_class, "invalid_tool_arguments");
        assert_eq!(error.path, "/arguments/limit");
        assert!(error.message.contains("integer"));
    }

    #[test]
    fn rejects_unknown_arguments_when_schema_disallows_additional_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "repo_name": { "type": "string" },
                "top_n": { "type": "integer" }
            },
            "additionalProperties": false
        })
        .as_object()
        .expect("object schema")
        .clone();
        let arguments = json!({ "limit": 2 })
            .as_object()
            .expect("arguments")
            .clone();
        let error = validate_arguments_against_schema(
            "coding-session-store",
            "coding_overview",
            &arguments,
            &schema,
        )
        .expect_err("unknown field should fail");

        assert_eq!(error.failure_class, "invalid_tool_arguments");
        assert_eq!(error.path, "/arguments/limit");
        assert!(error.message.contains("unknown argument"));
    }

    #[test]
    fn missing_tool_envelope_stays_tool_not_found() {
        let error = StructuredToolError::tool_not_found(
            "x-data",
            "missing_search",
            vec!["search_bookmarks".to_string()],
        );

        assert_eq!(error.failure_class, "tool_not_found");
        assert_eq!(error.path, "/tool_name");
        assert_eq!(error.service_id, "x-data");
        assert_eq!(error.tool_name, "missing_search");
        assert_eq!(
            error.available_tools,
            Some(vec!["search_bookmarks".to_string()])
        );
    }
}
