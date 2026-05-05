use chrono::{DateTime, Utc};
use rig::completion::message::{AssistantContent, Message};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolFailureClass {
    ServiceUnavailable,
    ToolNotFound,
    ResourceNotFound,
    ServiceSchemaDrift,
    InvalidToolArguments,
    InvalidJsonArguments,
    ArgumentsNotObject,
    ToolRuntimeError,
    ToolTimeout,
    NonzeroCommandExit,
    DeadlineOrInferenceFailure,
    Unclassified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentParseResult {
    ValidObject,
    InvalidJson,
    ArgumentsNotObject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaValidationResult {
    NotEvaluated,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceValidationError {
    pub code: String,
    pub path: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallTraceAnalysis {
    pub selected_service_id: Option<String>,
    pub selected_tool_name: Option<String>,
    pub argument_parse_result: ArgumentParseResult,
    pub schema_validation_result: SchemaValidationResult,
    pub validation_errors: Vec<TraceValidationError>,
    pub final_arguments_sent: Option<Value>,
    pub tool_result_ok: bool,
    pub tool_failure_class: Option<ToolFailureClass>,
    pub tool_error: Option<TraceToolError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceToolError {
    pub failure_class: ToolFailureClass,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub retryable: Option<bool>,
    pub raw_error_text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AmyToolCallTraceRecord {
    pub run_id: Option<String>,
    pub case_id: Option<String>,
    pub prompt: Option<String>,
    pub agent_did: Option<String>,
    pub behavior_id: Option<String>,
    pub session_id: String,
    pub request_id: Option<String>,
    pub request_status: Option<String>,
    pub request_lifecycle_state: Option<String>,
    pub request_failure_reason: Option<String>,
    pub response_status: Option<String>,
    pub response_error_message: Option<String>,
    pub request_failure_class: Option<ToolFailureClass>,
    pub backend_id: Option<String>,
    pub model_name: Option<String>,
    pub inference_profile_id: Option<String>,
    pub raw_assistant_message: Option<Value>,
    pub raw_tool_call_json: Option<Value>,
    pub tool_call_id: String,
    pub native_or_meta_tool: String,
    pub selected_service_id: Option<String>,
    pub selected_tool_name: Option<String>,
    pub raw_arguments: String,
    pub argument_parse_result: ArgumentParseResult,
    pub schema_validation_result: SchemaValidationResult,
    pub validation_errors: Vec<TraceValidationError>,
    pub repair_attempt: Option<Value>,
    pub final_arguments_sent: Option<Value>,
    pub tool_result: String,
    pub tool_result_ok: bool,
    pub tool_call_completed: bool,
    pub tool_status: String,
    pub task_outcome: Option<String>,
    pub tool_failure_class: Option<ToolFailureClass>,
    pub tool_error: Option<TraceToolError>,
    /// Deprecated compatibility alias for tool_failure_class.
    pub failure_class: Option<ToolFailureClass>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub latency_ms: Option<i64>,
    pub retry_count: Option<i64>,
}

pub fn analyze_tool_call(
    tool_name: &str,
    raw_args: &str,
    result: &str,
    status: &str,
) -> ToolCallTraceAnalysis {
    let mut analysis = analyze_arguments(tool_name, raw_args);
    let structured_tool_error = structured_tool_error_from_result(result);
    if let Some(error) = &structured_tool_error {
        analysis.tool_failure_class = Some(error.failure_class);
        if error.failure_class == ToolFailureClass::InvalidToolArguments {
            analysis.schema_validation_result = SchemaValidationResult::Failed;
        }
        if let (Some(path), Some(message)) = (&error.path, &error.message) {
            analysis.validation_errors.push(TraceValidationError {
                code: failure_class_code(error.failure_class).to_string(),
                path: path.clone(),
                message: message.clone(),
                retryable: error.retryable.unwrap_or(false),
            });
        }
    } else if analysis.tool_failure_class.is_none() {
        analysis.tool_failure_class = classify_result_text(result);
    }

    let completed = status.trim().eq_ignore_ascii_case("completed");
    if analysis.tool_failure_class.is_none() && !completed {
        analysis.tool_failure_class = Some(ToolFailureClass::Unclassified);
    }

    analysis.tool_result_ok = completed && analysis.tool_failure_class.is_none();
    analysis.tool_error = structured_tool_error.or_else(|| {
        analysis
            .tool_failure_class
            .map(|failure_class| TraceToolError {
                failure_class,
                service_id: analysis.selected_service_id.clone(),
                tool_name: analysis.selected_tool_name.clone(),
                path: analysis
                    .validation_errors
                    .first()
                    .map(|error| error.path.clone()),
                message: analysis
                    .validation_errors
                    .first()
                    .map(|error| error.message.clone()),
                retryable: retryable_for_failure_class(failure_class),
                raw_error_text: raw_tool_error_text(result, &analysis),
            })
    });
    analysis
}

pub fn analyze_request_failure(text: Option<&str>) -> Option<ToolFailureClass> {
    classify_request_failure_text(text)
}

pub fn latency_ms(started_at: Option<&str>, completed_at: Option<&str>) -> Option<i64> {
    let started = parse_rfc3339(started_at?)?;
    let completed = parse_rfc3339(completed_at?)?;
    Some(completed.signed_duration_since(started).num_milliseconds())
}

pub fn raw_message_json(content: &str) -> Value {
    serde_json::from_str(content).unwrap_or_else(|_| Value::String(content.to_string()))
}

pub fn extract_raw_tool_call_json(
    role: &str,
    content: &str,
    persisted_tool_call_id: &str,
    persisted_tool_name: &str,
) -> Option<Value> {
    let message = defra_agent_protocol::transcript::decode_persisted_message(role, content);
    let Message::Assistant { content, .. } = message else {
        return None;
    };

    let mut first_name_match = None;
    for item in content.iter() {
        let AssistantContent::ToolCall(tool_call) = item else {
            continue;
        };

        if tool_call.id == persisted_tool_call_id
            || tool_call.call_id.as_deref() == Some(persisted_tool_call_id)
        {
            return serde_json::to_value(tool_call).ok();
        }

        if first_name_match.is_none() && tool_call.function.name == persisted_tool_name {
            first_name_match = serde_json::to_value(tool_call).ok();
        }
    }

    first_name_match
}

fn analyze_arguments(tool_name: &str, raw_args: &str) -> ToolCallTraceAnalysis {
    let mut analysis = ToolCallTraceAnalysis {
        selected_service_id: None,
        selected_tool_name: None,
        argument_parse_result: ArgumentParseResult::ValidObject,
        schema_validation_result: SchemaValidationResult::NotEvaluated,
        validation_errors: Vec::new(),
        final_arguments_sent: None,
        tool_result_ok: false,
        tool_failure_class: None,
        tool_error: None,
    };

    let parsed = match serde_json::from_str::<Value>(raw_args) {
        Ok(value) => value,
        Err(error) => {
            analysis.argument_parse_result = ArgumentParseResult::InvalidJson;
            analysis.schema_validation_result = SchemaValidationResult::Failed;
            analysis.tool_failure_class = Some(ToolFailureClass::InvalidJsonArguments);
            analysis.validation_errors.push(TraceValidationError {
                code: "invalid_json_arguments".to_string(),
                path: "/".to_string(),
                message: error.to_string(),
                retryable: true,
            });
            return analysis;
        }
    };

    let Some(object) = parsed.as_object() else {
        analysis.argument_parse_result = ArgumentParseResult::ArgumentsNotObject;
        analysis.schema_validation_result = SchemaValidationResult::Failed;
        analysis.tool_failure_class = Some(ToolFailureClass::ArgumentsNotObject);
        analysis.validation_errors.push(TraceValidationError {
            code: "arguments_not_object".to_string(),
            path: "/".to_string(),
            message: "tool arguments must be a JSON object".to_string(),
            retryable: true,
        });
        analysis.final_arguments_sent = Some(parsed);
        return analysis;
    };

    analysis.final_arguments_sent = Some(parsed.clone());
    if matches!(tool_name, "call_tool" | "describe_tool") {
        analysis.selected_service_id = object
            .get("service_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        analysis.selected_tool_name = object
            .get("tool_name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }

    if tool_name == "call_tool" {
        apply_call_tool_argument_analysis(&mut analysis, object.get("arguments"));
    }

    analysis
}

fn apply_call_tool_argument_analysis(
    analysis: &mut ToolCallTraceAnalysis,
    arguments: Option<&Value>,
) {
    let Some(arguments) = arguments else {
        analysis.final_arguments_sent = None;
        return;
    };

    match arguments {
        Value::Object(_) => {
            analysis.final_arguments_sent = Some(arguments.clone());
        }
        Value::String(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(parsed) if parsed.is_object() => {
                analysis.final_arguments_sent = Some(parsed);
            }
            Ok(parsed) => {
                analysis.argument_parse_result = ArgumentParseResult::ArgumentsNotObject;
                analysis.schema_validation_result = SchemaValidationResult::Failed;
                analysis.tool_failure_class = Some(ToolFailureClass::ArgumentsNotObject);
                analysis.final_arguments_sent = Some(json!({ "input": parsed }));
                analysis.validation_errors.push(TraceValidationError {
                    code: "arguments_not_object".to_string(),
                    path: "/arguments".to_string(),
                    message: "call_tool.arguments must decode to a JSON object".to_string(),
                    retryable: true,
                });
            }
            Err(error) => {
                analysis.argument_parse_result = ArgumentParseResult::InvalidJson;
                analysis.schema_validation_result = SchemaValidationResult::Failed;
                analysis.tool_failure_class = Some(ToolFailureClass::InvalidJsonArguments);
                analysis.final_arguments_sent = Some(json!({ "input": raw }));
                analysis.validation_errors.push(TraceValidationError {
                    code: "invalid_json_arguments".to_string(),
                    path: "/arguments".to_string(),
                    message: error.to_string(),
                    retryable: true,
                });
            }
        },
        Value::Null => {
            analysis.argument_parse_result = ArgumentParseResult::ArgumentsNotObject;
            analysis.schema_validation_result = SchemaValidationResult::Failed;
            analysis.tool_failure_class = Some(ToolFailureClass::ArgumentsNotObject);
            analysis.final_arguments_sent = None;
            analysis.validation_errors.push(TraceValidationError {
                code: "arguments_not_object".to_string(),
                path: "/arguments".to_string(),
                message: "call_tool.arguments must be a JSON object".to_string(),
                retryable: true,
            });
        }
        other => {
            analysis.argument_parse_result = ArgumentParseResult::ArgumentsNotObject;
            analysis.schema_validation_result = SchemaValidationResult::Failed;
            analysis.tool_failure_class = Some(ToolFailureClass::ArgumentsNotObject);
            analysis.final_arguments_sent = Some(json!({ "input": other }));
            analysis.validation_errors.push(TraceValidationError {
                code: "arguments_not_object".to_string(),
                path: "/arguments".to_string(),
                message: "call_tool.arguments must be a JSON object".to_string(),
                retryable: true,
            });
        }
    }
}

fn structured_tool_error_from_result(result: &str) -> Option<TraceToolError> {
    let trimmed = result.trim();
    if trimmed.is_empty() {
        return None;
    }

    let value = serde_json::from_str::<Value>(trimmed).ok()?;
    let object = value.as_object()?;
    if object.get("ok").and_then(Value::as_bool) != Some(false) {
        return None;
    }

    let failure_class = object
        .get("failure_class")
        .and_then(Value::as_str)
        .and_then(failure_class_from_str)?;
    let retryable = object.get("retryable").and_then(Value::as_bool);

    Some(TraceToolError {
        failure_class,
        service_id: object
            .get("service_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        tool_name: object
            .get("tool_name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        path: object
            .get("path")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        message: object
            .get("message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        retryable,
        raw_error_text: trimmed.to_string(),
    })
}

fn failure_class_from_str(raw: &str) -> Option<ToolFailureClass> {
    serde_json::from_value(Value::String(raw.to_string())).ok()
}

fn failure_class_code(failure_class: ToolFailureClass) -> &'static str {
    match failure_class {
        ToolFailureClass::ServiceUnavailable => "service_unavailable",
        ToolFailureClass::ToolNotFound => "tool_not_found",
        ToolFailureClass::ResourceNotFound => "resource_not_found",
        ToolFailureClass::ServiceSchemaDrift => "service_schema_drift",
        ToolFailureClass::InvalidToolArguments => "invalid_tool_arguments",
        ToolFailureClass::InvalidJsonArguments => "invalid_json_arguments",
        ToolFailureClass::ArgumentsNotObject => "arguments_not_object",
        ToolFailureClass::ToolRuntimeError => "tool_runtime_error",
        ToolFailureClass::ToolTimeout => "tool_timeout",
        ToolFailureClass::NonzeroCommandExit => "nonzero_command_exit",
        ToolFailureClass::DeadlineOrInferenceFailure => "deadline_or_inference_failure",
        ToolFailureClass::Unclassified => "unclassified",
    }
}

fn classify_result_text(result: &str) -> Option<ToolFailureClass> {
    let trimmed = result.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    if has_nonzero_exit_code(trimmed) {
        return Some(ToolFailureClass::NonzeroCommandExit);
    }
    if looks_like_service_unavailable(&lower) {
        return Some(ToolFailureClass::ServiceUnavailable);
    }
    if looks_like_invalid_tool_arguments(&lower) {
        return Some(ToolFailureClass::InvalidToolArguments);
    }
    if looks_like_service_schema_drift(&lower) {
        return Some(ToolFailureClass::ServiceSchemaDrift);
    }
    if looks_like_resource_not_found(&lower) {
        return Some(ToolFailureClass::ResourceNotFound);
    }
    if looks_like_tool_not_found(&lower) {
        return Some(ToolFailureClass::ToolNotFound);
    }
    if looks_like_deadline_or_inference_failure(&lower) {
        return Some(ToolFailureClass::DeadlineOrInferenceFailure);
    }
    if looks_like_tool_timeout(&lower) {
        return Some(ToolFailureClass::ToolTimeout);
    }
    if looks_like_opaque_tool_error(&lower) {
        return Some(ToolFailureClass::Unclassified);
    }
    if looks_like_tool_runtime_error(&lower) {
        return Some(ToolFailureClass::ToolRuntimeError);
    }

    None
}

fn classify_request_failure_text(text: Option<&str>) -> Option<ToolFailureClass> {
    let lower = text?.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if looks_like_deadline_or_inference_failure(&lower) {
        return Some(ToolFailureClass::DeadlineOrInferenceFailure);
    }
    None
}

fn has_nonzero_exit_code(result: &str) -> bool {
    for line in result.lines() {
        let trimmed = line.trim();
        if let Some(raw_code) = trimmed.strip_prefix("exit_code:") {
            return raw_code.trim().parse::<i64>().is_ok_and(|code| code != 0);
        }
    }

    let lower = result.to_ascii_lowercase();
    lower.contains("exit status: 1")
        || lower.contains("exit status: 2")
        || lower.contains("exit code 1")
        || lower.contains("exit code 2")
}

fn looks_like_tool_not_found(lower: &str) -> bool {
    (lower.contains("tool '") && lower.contains("' not found"))
        || lower.contains("tool not found")
        || lower.contains("unknown tool")
}

fn looks_like_resource_not_found(lower: &str) -> bool {
    lower.contains("session not found")
        || lower.contains("resource not found")
        || lower.contains("document not found")
        || lower.contains("record not found")
        || lower.contains("entity not found")
}

fn looks_like_service_schema_drift(lower: &str) -> bool {
    (lower.contains("parse error") && lower.contains("cannot query field"))
        || lower.contains("cannot query field")
        || lower.contains("unknown field")
        || lower.contains("unknown argument")
        || lower.contains("field is not defined")
}

fn looks_like_invalid_tool_arguments(lower: &str) -> bool {
    lower.contains("invalid_tool_arguments")
        || lower.contains("invalid tool arguments")
        || lower.contains("invalid arguments")
        || lower.contains("invalid params")
        || lower.contains("missing field")
        || lower.contains("arguments must")
        || (lower.contains("failed to deserialize") && lower.contains("parameter"))
}

fn looks_like_service_unavailable(lower: &str) -> bool {
    lower.contains("currently unreachable")
        || lower.contains("probe timed out")
        || lower.contains("not found or offline")
        || lower.contains("mcp handshake failed")
        || lower.contains("connection refused")
        || lower.contains("failed to connect")
        || lower.contains("connection error")
        || lower.contains("service unavailable")
}

fn looks_like_deadline_or_inference_failure(lower: &str) -> bool {
    lower.contains("request deadline exceeded")
        || lower.contains("deadline exceeded")
        || lower.contains("inference timed out")
        || lower.contains("stream liveness timeout")
        || lower.contains("completion failed")
        || lower.contains("model unreachable")
}

fn looks_like_tool_timeout(lower: &str) -> bool {
    lower.contains("timed out after")
        || lower.contains("timed out waiting")
        || lower.contains("tool timeout")
}

fn looks_like_opaque_tool_error(lower: &str) -> bool {
    lower.trim_end().ends_with("mcp call_tool")
}

fn looks_like_tool_runtime_error(lower: &str) -> bool {
    lower.contains("returned an error")
        || lower.contains("tool call failed")
        || lower.contains("permission denied")
        || lower.contains("no such file or directory")
}

fn retryable_for_failure_class(failure_class: ToolFailureClass) -> Option<bool> {
    match failure_class {
        ToolFailureClass::ServiceUnavailable
        | ToolFailureClass::ToolTimeout
        | ToolFailureClass::DeadlineOrInferenceFailure => Some(true),
        ToolFailureClass::InvalidToolArguments
        | ToolFailureClass::InvalidJsonArguments
        | ToolFailureClass::ArgumentsNotObject
        | ToolFailureClass::ToolNotFound => Some(true),
        ToolFailureClass::ResourceNotFound
        | ToolFailureClass::ServiceSchemaDrift
        | ToolFailureClass::NonzeroCommandExit => Some(false),
        ToolFailureClass::ToolRuntimeError | ToolFailureClass::Unclassified => None,
    }
}

fn raw_tool_error_text(result: &str, analysis: &ToolCallTraceAnalysis) -> String {
    let trimmed = result.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    analysis
        .validation_errors
        .first()
        .map(|error| error.message.clone())
        .unwrap_or_default()
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::completion::message::{Text, ToolCall, ToolFunction};
    use rig::one_or_many::OneOrMany;

    #[test]
    fn successful_completed_result_has_no_failure_class() {
        let analysis =
            analyze_tool_call("read", r#"{"path":"README.md"}"#, "contents", "completed");

        assert!(analysis.tool_result_ok);
        assert_eq!(analysis.tool_failure_class, None);
        assert_eq!(
            analysis.argument_parse_result,
            ArgumentParseResult::ValidObject
        );
    }

    #[test]
    fn completed_nonzero_command_exit_is_not_a_successful_outcome() {
        let result = "cwd: /tmp\ncommand: grep -P foo README.md\nexit_code: 2\nstdout:\n(empty)\nstderr:\ngrep: invalid option -- P";
        let analysis = analyze_tool_call("bash", r#"{"command":"grep"}"#, result, "completed");

        assert!(!analysis.tool_result_ok);
        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::NonzeroCommandExit)
        );
        assert_eq!(
            analysis
                .tool_error
                .as_ref()
                .and_then(|error| error.retryable),
            Some(false)
        );
    }

    #[test]
    fn classifies_service_unavailable_from_tool_result_text() {
        let result = "call_tool: service 'coding-session-store' is currently unreachable (last error: probe timed out)";
        let analysis = analyze_tool_call(
            "call_tool",
            r#"{"service_id":"coding-session-store","tool_name":"search","arguments":{}}"#,
            result,
            "completed",
        );

        assert!(!analysis.tool_result_ok);
        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::ServiceUnavailable)
        );
        assert_eq!(
            analysis.selected_service_id.as_deref(),
            Some("coding-session-store")
        );
        assert_eq!(analysis.selected_tool_name.as_deref(), Some("search"));
    }

    #[test]
    fn classifies_missing_mcp_tool() {
        let result = "tool 'missing' not found on service 'x-data'. Available tools: search_posts";
        let analysis = analyze_tool_call(
            "describe_tool",
            r#"{"service_id":"x-data","tool_name":"missing"}"#,
            result,
            "completed",
        );

        assert!(!analysis.tool_result_ok);
        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::ToolNotFound)
        );
    }

    #[test]
    fn structured_invalid_argument_envelope_marks_completed_tool_call_failed() {
        let result = json!({
            "ok": false,
            "failure_class": "invalid_tool_arguments",
            "path": "/arguments/query",
            "message": "missing required argument 'query'",
            "retryable": true,
            "service_id": "x-data",
            "tool_name": "search_bookmarks"
        })
        .to_string();
        let analysis = analyze_tool_call(
            "call_tool",
            r#"{"service_id":"x-data","tool_name":"search_bookmarks","arguments":{}}"#,
            &result,
            "completed",
        );

        assert!(!analysis.tool_result_ok);
        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::InvalidToolArguments)
        );
        assert_eq!(
            analysis.schema_validation_result,
            SchemaValidationResult::Failed
        );
        assert_eq!(analysis.validation_errors[0].path, "/arguments/query");
        let error = analysis.tool_error.as_ref().expect("tool error");
        assert_eq!(error.failure_class, ToolFailureClass::InvalidToolArguments);
        assert_eq!(error.service_id.as_deref(), Some("x-data"));
        assert_eq!(error.tool_name.as_deref(), Some("search_bookmarks"));
        assert_eq!(error.path.as_deref(), Some("/arguments/query"));
        assert_eq!(error.retryable, Some(true));
    }

    #[test]
    fn classifies_legacy_mcp_missing_field_text_as_invalid_arguments() {
        let result = "Toolset error: ToolCallError: ToolCallError: MCP call_tool: JsonRpcError { code: -32602, message: \"missing field `query`\" }";
        let analysis = analyze_tool_call(
            "call_tool",
            r#"{"service_id":"x-data","tool_name":"search_bookmarks","arguments":{}}"#,
            result,
            "completed",
        );

        assert!(!analysis.tool_result_ok);
        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::InvalidToolArguments)
        );
        assert_eq!(
            analysis
                .tool_error
                .as_ref()
                .and_then(|error| error.retryable),
            Some(true)
        );
    }

    #[test]
    fn classifies_mcp_unknown_field_deserialize_error_as_invalid_arguments() {
        let result = "Toolset error: ToolCallError: ToolCallError: MCP call_tool: call_tool: Mcp error: -32602: failed to deserialize parameters: unknown field `limit`, expected one of `repo_name`, `path_contains`, `top_n`";
        let analysis = analyze_tool_call(
            "call_tool",
            r#"{"service_id":"coding-session-store","tool_name":"coding_overview","arguments":{"limit":2}}"#,
            result,
            "completed",
        );

        assert!(!analysis.tool_result_ok);
        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::InvalidToolArguments)
        );
        assert_eq!(
            analysis
                .tool_error
                .as_ref()
                .and_then(|error| error.retryable),
            Some(true)
        );
    }

    #[test]
    fn classifies_missing_resource_separately_from_missing_tool() {
        let result = "tool 'get_coding_session' on service 'coding-session-store' returned an error: session not found: stale-session";
        let analysis = analyze_tool_call(
            "call_tool",
            r#"{"service_id":"coding-session-store","tool_name":"get_coding_session","arguments":{"session_id":"stale-session"}}"#,
            result,
            "completed",
        );

        assert!(!analysis.tool_result_ok);
        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::ResourceNotFound)
        );
        assert_eq!(
            analysis
                .tool_error
                .as_ref()
                .and_then(|error| error.retryable),
            Some(false)
        );
    }

    #[test]
    fn classifies_service_schema_drift() {
        let result = "tool 'deploy_history' on service 'observability-mcp' returned an error: deploy log query failed: parse error: Cannot query field \"DeployEvent\" on type \"Query\".";
        let analysis = analyze_tool_call(
            "call_tool",
            r#"{"service_id":"observability-mcp","tool_name":"deploy_history","arguments":{}}"#,
            result,
            "completed",
        );

        assert!(!analysis.tool_result_ok);
        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::ServiceSchemaDrift)
        );
    }

    #[test]
    fn preserves_opaque_mcp_errors_as_unclassified_tool_errors() {
        let result = "Toolset error: ToolCallError: ToolCallError: MCP call_tool";
        let analysis = analyze_tool_call(
            "call_tool",
            r#"{"service_id":"coding-session-store","tool_name":"search_coding_notes","arguments":{"topic":"amy"}}"#,
            result,
            "completed",
        );

        assert!(!analysis.tool_result_ok);
        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::Unclassified)
        );
        let error = analysis.tool_error.as_ref().expect("tool error");
        assert_eq!(error.failure_class, ToolFailureClass::Unclassified);
        assert_eq!(error.retryable, None);
        assert_eq!(error.raw_error_text, result);
    }

    #[test]
    fn request_deadline_is_classified_separately() {
        let analysis = analyze_tool_call(
            "read",
            r#"{"path":"README.md"}"#,
            "README contents",
            "completed",
        );

        assert!(analysis.tool_result_ok);
        assert_eq!(analysis.tool_failure_class, None);
        assert_eq!(
            analyze_request_failure(Some("request deadline exceeded while awaiting inference")),
            Some(ToolFailureClass::DeadlineOrInferenceFailure)
        );
    }

    #[test]
    fn classifies_invalid_top_level_json_arguments() {
        let analysis = analyze_tool_call("read", r#"{"path":"README.md""#, "", "called");

        assert_eq!(
            analysis.argument_parse_result,
            ArgumentParseResult::InvalidJson
        );
        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::InvalidJsonArguments)
        );
        assert_eq!(analysis.validation_errors[0].code, "invalid_json_arguments");
    }

    #[test]
    fn classifies_non_object_top_level_arguments() {
        let analysis = analyze_tool_call("read", r#""README.md""#, "", "called");

        assert_eq!(
            analysis.argument_parse_result,
            ArgumentParseResult::ArgumentsNotObject
        );
        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::ArgumentsNotObject)
        );
    }

    #[test]
    fn classifies_stringified_call_tool_arguments_that_are_not_json() {
        let analysis = analyze_tool_call(
            "call_tool",
            r#"{"service_id":"x-data","tool_name":"search","arguments":"query=defra"}"#,
            "",
            "called",
        );

        assert_eq!(
            analysis.argument_parse_result,
            ArgumentParseResult::InvalidJson
        );
        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::InvalidJsonArguments)
        );
        assert_eq!(analysis.validation_errors[0].path, "/arguments");
        assert_eq!(
            analysis.final_arguments_sent,
            Some(json!({ "input": "query=defra" }))
        );
    }

    #[test]
    fn normalizes_stringified_call_tool_object_arguments() {
        let analysis = analyze_tool_call(
            "call_tool",
            r#"{"service_id":"x-data","tool_name":"search","arguments":"{\"query\":\"defra\"}"}"#,
            "ok",
            "completed",
        );

        assert!(analysis.tool_result_ok);
        assert_eq!(analysis.tool_failure_class, None);
        assert_eq!(
            analysis.final_arguments_sent,
            Some(json!({ "query": "defra" }))
        );
    }

    #[test]
    fn classifies_tool_timeout() {
        let analysis = analyze_tool_call(
            "call_tool",
            r#"{"service_id":"hf-data","tool_name":"search","arguments":{}}"#,
            "service 'hf-data' timed out after 300s",
            "completed",
        );

        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::ToolTimeout)
        );
    }

    #[test]
    fn classifies_uncompleted_tool_call_without_tool_error_as_unclassified() {
        let analysis = analyze_tool_call("read", r#"{"path":"README.md"}"#, "", "called");

        assert!(!analysis.tool_result_ok);
        assert_eq!(
            analysis.tool_failure_class,
            Some(ToolFailureClass::Unclassified)
        );
    }

    #[test]
    fn extracts_raw_tool_call_json_from_persisted_assistant_message() {
        let message = Message::Assistant {
            id: None,
            content: OneOrMany::many(vec![
                AssistantContent::Text(Text {
                    text: "Checking.".to_string(),
                }),
                AssistantContent::ToolCall(ToolCall {
                    id: "internal-1".to_string(),
                    call_id: Some("call-1".to_string()),
                    function: ToolFunction {
                        name: "read".to_string(),
                        arguments: json!({ "path": "README.md" }),
                    },
                    signature: None,
                    additional_params: None,
                }),
            ])
            .unwrap(),
        };
        let raw = serde_json::to_string(&message).unwrap();

        let tool_call = extract_raw_tool_call_json("assistant", &raw, "internal-1", "read")
            .expect("tool call json");

        assert_eq!(tool_call["id"], "internal-1");
        assert_eq!(tool_call["function"]["name"], "read");
    }

    #[test]
    fn computes_latency_from_rfc3339_timestamps() {
        assert_eq!(
            latency_ms(
                Some("2026-05-04T12:00:00Z"),
                Some("2026-05-04T12:00:01.250Z")
            ),
            Some(1250)
        );
    }
}
