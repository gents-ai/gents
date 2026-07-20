use crate::llm::message::{AssistantContent, Message};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub use crate::tool_call_lifecycle::FailureClass as ToolFailureClass;

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
    pub native_tool_output: Option<NativeToolOutputTrace>,
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
    pub requested_tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub retryable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_tools: Option<Vec<String>>,
    pub raw_error_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeToolOutputTrace {
    pub ok: bool,
    pub status: Option<String>,
    pub tool: Option<String>,
    pub path: Option<String>,
    pub pattern: Option<String>,
    pub command: Option<String>,
    pub exit_code: Option<i64>,
    pub timed_out: Option<bool>,
    pub execution_mode: Option<String>,
    pub network_mode: Option<String>,
    pub sandbox: Option<String>,
    pub returned_count: Option<usize>,
    pub total_count: Option<usize>,
    pub truncated: Option<bool>,
    pub default_ignored: Option<Vec<String>>,
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
    pub native_tool_output: Option<NativeToolOutputTrace>,
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
    analysis.native_tool_output = native_tool_output_from_result(tool_name, result);
    let structured_tool_error = structured_tool_error_from_result(result);
    if let Some(error) = &structured_tool_error {
        analysis.tool_failure_class = Some(error.failure_class);
        if error.failure_class == ToolFailureClass::ArgumentInvalid {
            analysis.schema_validation_result = SchemaValidationResult::Failed;
        }
        if let (Some(path), Some(message)) = (&error.path, &error.message) {
            analysis.validation_errors.push(TraceValidationError {
                code: structured_tool_error_code_from_result(result)
                    .unwrap_or_else(|| failure_class_code(error.failure_class).to_string()),
                path: path.clone(),
                message: message.clone(),
                retryable: error.retryable.unwrap_or(false),
            });
        }
    } else if let Some(native) = analysis.native_tool_output.as_ref() {
        if !native.ok {
            analysis.tool_failure_class =
                native_tool_failure_class(native).or_else(|| classify_result_text(result));
        }
    } else if analysis.tool_failure_class.is_none() {
        analysis.tool_failure_class = classify_result_text(result);
    }

    let completed = status.trim().eq_ignore_ascii_case("completed");
    if analysis.tool_failure_class.is_none()
        && analysis
            .native_tool_output
            .as_ref()
            .is_some_and(|output| !output.ok)
    {
        analysis.tool_failure_class = Some(ToolFailureClass::ToolReturnedError);
    }
    if analysis.tool_failure_class.is_none() && !completed {
        analysis.tool_failure_class = Some(ToolFailureClass::ToolReturnedError);
    }

    analysis.tool_result_ok = completed && analysis.tool_failure_class.is_none();
    analysis.tool_error = structured_tool_error.or_else(|| {
        analysis
            .tool_failure_class
            .map(|failure_class| TraceToolError {
                failure_class,
                service_id: analysis.selected_service_id.clone(),
                tool_name: analysis.selected_tool_name.clone(),
                requested_tool_name: None,
                path: analysis
                    .validation_errors
                    .first()
                    .map(|error| error.path.clone()),
                message: analysis
                    .validation_errors
                    .first()
                    .map(|error| error.message.clone()),
                retryable: retryable_for_failure_class(failure_class),
                available_tools: None,
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
    let latency = completed.signed_duration_since(started).num_milliseconds();
    (latency >= 0).then_some(latency)
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
        native_tool_output: None,
        tool_result_ok: false,
        tool_failure_class: None,
        tool_error: None,
    };

    let parsed = match serde_json::from_str::<Value>(raw_args) {
        Ok(value) => value,
        Err(error) => {
            analysis.argument_parse_result = ArgumentParseResult::InvalidJson;
            analysis.schema_validation_result = SchemaValidationResult::Failed;
            analysis.tool_failure_class = Some(ToolFailureClass::ArgumentInvalid);
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
        analysis.tool_failure_class = Some(ToolFailureClass::ArgumentInvalid);
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
                analysis.tool_failure_class = Some(ToolFailureClass::ArgumentInvalid);
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
                analysis.tool_failure_class = Some(ToolFailureClass::ArgumentInvalid);
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
            analysis.tool_failure_class = Some(ToolFailureClass::ArgumentInvalid);
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
            analysis.tool_failure_class = Some(ToolFailureClass::ArgumentInvalid);
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
    let requested_tool_name = object
        .get("requested_tool_name")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let tool_name = object
        .get("tool_name")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| requested_tool_name.clone());

    Some(TraceToolError {
        failure_class,
        service_id: object
            .get("service_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        tool_name,
        requested_tool_name,
        path: object
            .get("path")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        message: object
            .get("message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        retryable,
        available_tools: string_array_field(object, "available_tools"),
        raw_error_text: trimmed.to_string(),
    })
}

fn structured_tool_error_code_from_result(result: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(result.trim()).ok()?;
    let object = value.as_object()?;
    if object.get("ok").and_then(Value::as_bool) != Some(false) {
        return None;
    }

    let raw = object.get("failure_class").and_then(Value::as_str)?;
    // Preserve the policy-block reason for trace consumers while keeping
    // discovery failures rebucketed to the canonical 5-variant vocabulary.
    (raw == "tool_not_allowed").then(|| raw.to_string())
}

fn native_tool_output_from_result(tool_name: &str, result: &str) -> Option<NativeToolOutputTrace> {
    if !is_native_structured_output_tool(tool_name) {
        return None;
    }

    let object = native_tool_output_object(tool_name, result)?;
    let ok = object.get("ok").and_then(Value::as_bool)?;
    Some(NativeToolOutputTrace {
        ok,
        status: object
            .get("status")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        tool: object
            .get("tool")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        path: object
            .get("path")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        pattern: object
            .get("pattern")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        command: object
            .get("command")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        exit_code: object.get("exit_code").and_then(Value::as_i64),
        timed_out: object.get("timed_out").and_then(Value::as_bool),
        execution_mode: object
            .get("execution_mode")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        network_mode: object
            .get("network_mode")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        sandbox: object
            .get("sandbox")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        returned_count: usize_field(&object, "returned_count"),
        total_count: usize_field(&object, "total_count"),
        truncated: object.get("truncated").and_then(Value::as_bool),
        default_ignored: string_array_field(&object, "default_ignored"),
    })
}

fn native_tool_output_object(
    tool_name: &str,
    result: &str,
) -> Option<serde_json::Map<String, Value>> {
    let trimmed = result.trim_start();
    let prefix = if is_native_filesystem_tool(tool_name) {
        "defra_fs: "
    } else if is_native_command_tool(tool_name) {
        "defra_exec: "
    } else {
        return None;
    };
    let metadata = trimmed
        .lines()
        .next()
        .and_then(|line| line.trim().strip_prefix(prefix))
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());

    let value = metadata.or_else(|| serde_json::from_str::<Value>(trimmed).ok())?;
    let Value::Object(object) = value else {
        return None;
    };
    Some(object)
}

fn native_tool_failure_class(output: &NativeToolOutputTrace) -> Option<ToolFailureClass> {
    if output.timed_out == Some(true) || output.status.as_deref() == Some("timeout") {
        return Some(ToolFailureClass::External);
    }
    if output.exit_code.is_some_and(|code| code != 0)
        || output.status.as_deref() == Some("exit_nonzero")
    {
        return Some(ToolFailureClass::ToolReturnedError);
    }
    (!output.ok).then_some(ToolFailureClass::ToolReturnedError)
}

fn is_native_structured_output_tool(tool_name: &str) -> bool {
    is_native_filesystem_tool(tool_name) || is_native_command_tool(tool_name)
}

fn is_native_filesystem_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "read_file" | "list_files" | "glob" | "grep" | "write_file" | "edit_file"
    )
}

fn is_native_command_tool(tool_name: &str) -> bool {
    matches!(tool_name, "bash" | "bash_unrestricted")
}

fn usize_field(object: &serde_json::Map<String, Value>, field: &str) -> Option<usize> {
    object
        .get(field)?
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
}

fn string_array_field(object: &serde_json::Map<String, Value>, field: &str) -> Option<Vec<String>> {
    let values = object.get(field)?.as_array()?;
    Some(
        values
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
    )
}

fn failure_class_from_str(raw: &str) -> Option<ToolFailureClass> {
    // Accept both new camelCase persisted vocab and legacy snake_case strings
    // emitted by older MCP tool envelopes.
    if let Some(failure_class) = ToolFailureClass::from_persisted(raw) {
        return Some(failure_class);
    }
    match raw {
        // Legacy snake_case strings — rebucketed to 5-variant spec.
        "service_unavailable"
        | "tool_not_found"
        | "tool_not_allowed"
        | "resource_not_found"
        | "service_schema_drift" => Some(ToolFailureClass::ServiceUnavailable),
        "invalid_tool_arguments" | "invalid_json_arguments" | "arguments_not_object" => {
            Some(ToolFailureClass::ArgumentInvalid)
        }
        "tool_runtime_error" | "nonzero_command_exit" | "unclassified" => {
            Some(ToolFailureClass::ToolReturnedError)
        }
        "tool_timeout" | "deadline_or_inference_failure" => Some(ToolFailureClass::External),
        _ => None,
    }
}

fn failure_class_code(failure_class: ToolFailureClass) -> &'static str {
    failure_class.as_str()
}

fn classify_result_text(result: &str) -> Option<ToolFailureClass> {
    let trimmed = result.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    if has_nonzero_exit_code(trimmed) {
        return Some(ToolFailureClass::ToolReturnedError);
    }
    if looks_like_service_unavailable(&lower) {
        return Some(ToolFailureClass::ServiceUnavailable);
    }
    if looks_like_invalid_tool_arguments(&lower) {
        return Some(ToolFailureClass::ArgumentInvalid);
    }
    if looks_like_service_schema_drift(&lower) {
        return Some(ToolFailureClass::ServiceUnavailable);
    }
    if looks_like_resource_not_found(&lower) {
        return Some(ToolFailureClass::ServiceUnavailable);
    }
    if looks_like_tool_not_found(&lower) {
        return Some(ToolFailureClass::ServiceUnavailable);
    }
    if looks_like_deadline_or_inference_failure(&lower) {
        return Some(ToolFailureClass::External);
    }
    if looks_like_tool_timeout(&lower) {
        return Some(ToolFailureClass::External);
    }
    if looks_like_opaque_tool_error(&lower) {
        return Some(ToolFailureClass::ToolReturnedError);
    }
    if looks_like_tool_runtime_error(&lower) {
        return Some(ToolFailureClass::ToolReturnedError);
    }

    None
}

fn classify_request_failure_text(text: Option<&str>) -> Option<ToolFailureClass> {
    let lower = text?.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if looks_like_deadline_or_inference_failure(&lower) {
        return Some(ToolFailureClass::External);
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
        || lower.contains("field is not defined")
        || ((lower.contains("unknown field") || lower.contains("unknown argument"))
            && (lower.contains("graphql")
                || lower.contains("query failed")
                || lower.contains(" on type ")
                || lower.contains("validation error")))
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
        // ServiceUnavailable and External are transient; ArgumentInvalid is a
        // model-fixable error so we let the request retry.
        ToolFailureClass::ServiceUnavailable
        | ToolFailureClass::External
        | ToolFailureClass::ArgumentInvalid => Some(true),
        // Transport errors are retriable by definition.
        ToolFailureClass::Transport => Some(true),
        // ToolReturnedError is non-retriable at the tool-call level; a denied
        // approval is an operator verdict, never retried automatically.
        ToolFailureClass::ToolReturnedError
        | ToolFailureClass::PolicyDenied
        | ToolFailureClass::ApprovalDenied => Some(false),
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
mod tests;
