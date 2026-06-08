use std::time::Duration;

use anyhow::{anyhow, Context as _};
use crate::llm::tool::ToolDefinition;
use crate::llm::tool::Tool;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::health_checker::HealthStatus;
use crate::truncation::{truncate_text, TruncationLimits, TruncationMode};

use super::shared::{
    enforce_health_gate, extract_text, lookup_service, MetaToolContext, MetaToolError,
    StructuredToolError,
};

/// Maximum size returned verbatim to the model per `call_tool` invocation.
/// Aligns with `DefraSpillTruncator`'s default byte budget (50 KiB / 2 000 lines).
/// The full remote response still reaches the persistence layer via the
/// turn-level backstop; this caps the in-loop copy that fits in the model's
/// context window.
const CALL_TOOL_MAX_BYTES: usize = 50 * 1024;
const CALL_TOOL_MAX_LINES: usize = 2_000;

#[derive(Debug, Deserialize)]
pub struct CallToolArgs {
    service_id: String,
    tool_name: String,
    arguments: serde_json::Value,
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
            description: "Invoke a tool on an MCP data service. Use discover_tools to \
                find available services and tools, then describe_tool to get the compact \
                input contract, then call_tool with the exact argument object. Native direct \
                tools such as file or bash tools are called directly, not through call_tool."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "service_id": {
                        "type": "string",
                        "description": "The service_id of the MCP data service; not a native tool namespace."
                    },
                    "tool_name": {
                        "type": "string",
                        "description": "The name of the tool to invoke."
                    },
                    "arguments": {
                        "type": "object",
                        "description": "The exact tool arguments object from describe_tool's contract. Required fields appear as /arguments/<field> paths."
                    }
                },
                "required": ["service_id", "tool_name", "arguments"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if let Some(error) = self
            .ctx
            .blocked_service_error(&args.service_id, &args.tool_name)
        {
            return Ok(error.to_result_text());
        }

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

        let service = lookup_service(&self.ctx, &args.service_id)
            .await
            .context("call_tool")?;
        let outbound_agent_did = service.outbound_agent_did(&self.ctx);

        let timeout_secs = if matches!(health.as_ref().map(|h| h.status), Some(HealthStatus::Stale))
        {
            120
        } else {
            300
        };

        if let Some(error) = self
            .preflight_arguments(
                &args.service_id,
                &service.endpoint,
                &args.tool_name,
                argument_object,
                outbound_agent_did,
            )
            .await
        {
            return Ok(error.to_result_text());
        }

        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            self.ctx.mcp_pool.call_tool_with_agent_did(
                &args.service_id,
                &service.endpoint,
                &args.tool_name,
                arguments,
                outbound_agent_did,
            ),
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
        if text.is_empty() {
            return Ok("(tool returned no text content)".to_string());
        }

        let limits = TruncationLimits {
            max_bytes: CALL_TOOL_MAX_BYTES,
            max_lines: CALL_TOOL_MAX_LINES,
        };
        let (capped, _trigger, _was_truncated) =
            truncate_text(&text, TruncationMode::Head, &limits);
        Ok(capped)
    }
}

impl CallToolTool {
    async fn preflight_arguments(
        &self,
        service_id: &str,
        endpoint: &str,
        tool_name: &str,
        arguments: &Map<String, Value>,
        agent_did: Option<&str>,
    ) -> Option<StructuredToolError> {
        let list_result = match tokio::time::timeout(
            Duration::from_secs(30),
            self.ctx
                .mcp_pool
                .list_tools_with_agent_did(service_id, endpoint, agent_did),
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
    use chrono::Utc;
    use serde_json::json;
    use std::sync::Arc;

    use crate::health_checker::{HealthStatus, ServiceHealth, ServiceHealthMap};
    use crate::lean_vocab_test::{lean_tool_preflight_cases, LeanToolPreflightCase};
    use crate::mcp_pool::McpPool;

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

    #[tokio::test]
    async fn generated_tool_preflight_cases_match_health_and_schema_gates() {
        for case in lean_tool_preflight_cases() {
            let actual = actual_preflight_contract(case).await;
            assert_eq!(
                actual.0, case.decision,
                "Lean ToolExecution preflight case {} must match Rust dispatch decision",
                case.name
            );
            assert_eq!(
                actual.1, case.failure_class,
                "Lean ToolExecution preflight case {} must match Rust failure class",
                case.name
            );
        }
    }

    async fn actual_preflight_contract(case: &LeanToolPreflightCase) -> (String, Option<String>) {
        let health_map = ServiceHealthMap::new();
        health_map
            .set_for_test(
                "x-data",
                ServiceHealth {
                    status: rust_health_status(&case.health),
                    last_seen: Utc::now(),
                    last_error: (case.health == "unreachable")
                        .then(|| "probe timed out".to_string()),
                },
            )
            .await;

        if enforce_health_gate(&health_map, "x-data").await.is_err() {
            return ("block".to_string(), Some("serviceUnavailable".to_string()));
        }

        match case.schema_status.as_str() {
            "unchecked" => ("dispatch".to_string(), None),
            "valid" => {
                let arguments = json!({ "query": "amy", "limit": 5 })
                    .as_object()
                    .expect("arguments")
                    .clone();
                let result = validate_arguments_against_schema(
                    "x-data",
                    "search_bookmarks",
                    &arguments,
                    &search_schema(),
                );
                assert!(
                    result.is_ok(),
                    "{} should pass schema validation",
                    case.name
                );
                ("dispatch".to_string(), None)
            }
            "invalid" => {
                let arguments = json!({ "limit": 5 })
                    .as_object()
                    .expect("arguments")
                    .clone();
                let error = validate_arguments_against_schema(
                    "x-data",
                    "search_bookmarks",
                    &arguments,
                    &search_schema(),
                )
                .expect_err("generated invalid-schema case should fail validation");
                assert_eq!(error.failure_class, "invalid_tool_arguments");
                ("block".to_string(), Some("argumentInvalid".to_string()))
            }
            other => panic!("unknown Lean schema status {other:?}"),
        }
    }

    fn rust_health_status(value: &str) -> HealthStatus {
        match value {
            "healthy" => HealthStatus::Healthy,
            "stale" => HealthStatus::Stale,
            "unreachable" => HealthStatus::Unreachable,
            other => panic!("unknown Lean health status {other:?}"),
        }
    }

    #[tokio::test]
    async fn disallowed_service_returns_before_argument_validation() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        let tool = CallToolTool::new(MetaToolContext {
            node,
            mcp_pool: McpPool::new(),
            health: ServiceHealthMap::new(),
            local_hostname: "studio-1".to_string(),
            local_subnet: None,
            agent_did: "did:key:z-test-agent".to_string(),
            allowed_mcp_service_ids: vec!["x-data".to_string()],
        });

        let output = tool
            .call(CallToolArgs {
                service_id: "observability-mcp".to_string(),
                tool_name: "query_metrics".to_string(),
                arguments: json!("not an object"),
            })
            .await
            .expect("disallowed service should return structured text");
        let value: Value = serde_json::from_str(&output).expect("structured json");

        assert_eq!(value["ok"], false);
        assert_eq!(value["failure_class"], "tool_not_allowed");
        assert_eq!(value["path"], "/service_id");
        assert_eq!(value["retryable"], false);
        assert_eq!(value["service_id"], "observability-mcp");
        assert_eq!(value["tool_name"], "query_metrics");
        assert_eq!(value["allowed_mcp_service_ids"], json!(["x-data"]));
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

    /// Synthesize an oversized MCP text result and confirm that `call_tool`
    /// caps the returned string and appends an honest truncation marker.
    #[test]
    fn oversized_mcp_result_is_capped_and_truncation_is_signalled() {
        // Build a string larger than CALL_TOOL_MAX_BYTES (50 KiB).
        let big_line = "x".repeat(200);
        let lines: Vec<String> = (0..500).map(|_| big_line.clone()).collect();
        let big_text = lines.join("\n"); // ~100 KiB, well over the 50 KiB cap

        let limits = TruncationLimits {
            max_bytes: CALL_TOOL_MAX_BYTES,
            max_lines: CALL_TOOL_MAX_LINES,
        };
        let (capped, _trigger, was_truncated) =
            truncate_text(&big_text, TruncationMode::Head, &limits);

        assert!(was_truncated, "oversized result must be truncated");
        assert!(
            capped.len() < big_text.len(),
            "capped output must be smaller than original: capped={}, original={}",
            capped.len(),
            big_text.len()
        );
        // Slack: capped output should be within 2× the byte cap (marker overhead)
        assert!(
            capped.len() < CALL_TOOL_MAX_BYTES * 2,
            "capped output should be near the cap, got {}",
            capped.len()
        );
        // Honest marker must be present so the model knows it was truncated.
        assert!(
            capped.contains("[Showing lines"),
            "honest truncation marker must be present: {capped}"
        );
        assert!(
            capped.contains("bytes total"),
            "total byte count must be reported: {capped}"
        );
    }

    /// Small result under the cap must pass through verbatim (no marker added).
    #[test]
    fn small_mcp_result_passes_through_unchanged() {
        let small_text = "hello from MCP tool";
        let limits = TruncationLimits {
            max_bytes: CALL_TOOL_MAX_BYTES,
            max_lines: CALL_TOOL_MAX_LINES,
        };
        let (result, _trigger, was_truncated) =
            truncate_text(small_text, TruncationMode::Head, &limits);

        assert!(!was_truncated);
        assert_eq!(result, small_text);
    }
}
