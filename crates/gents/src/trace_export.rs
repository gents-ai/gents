use crate::llm::message::{AssistantContent, Message};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tool_call_lifecycle::ToolCallState;

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
    pub failure_class: Option<ToolFailureClass>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub latency_ms: Option<i64>,
    pub retry_count: Option<i64>,
}

pub fn analyze_tool_call_with_persisted_outcome(
    tool_name: &str,
    raw_args: &str,
    result: &str,
    lifecycle_state: ToolCallState,
    persisted_failure_class: Option<&str>,
) -> ToolCallTraceAnalysis {
    let mut analysis = analyze_arguments(tool_name, raw_args);
    analysis.native_tool_output = native_tool_output_from_result(tool_name, result);

    let persisted_failure_class = persisted_failure_class.and_then(failure_class_from_str);
    let failure_class = persisted_failure_class.or_else(|| match lifecycle_state {
        ToolCallState::TimedOut => Some(ToolFailureClass::External),
        ToolCallState::Failed => Some(ToolFailureClass::ToolReturnedError),
        _ => None,
    });
    let completed = lifecycle_state == ToolCallState::Completed;

    // Result parsing contributes trace shape only. The durable lifecycle owns
    // the outcome and model-facing text can never override it.
    analysis.tool_failure_class = failure_class;
    analysis.tool_result_ok = completed && analysis.tool_failure_class.is_none();
    analysis.tool_error = failure_class.map(|failure_class| TraceToolError {
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
    let message = gents_protocol::transcript::decode_persisted_message(role, content);
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
        return;
    };

    match arguments {
        Value::Object(_) => {
            analysis.final_arguments_sent = Some(arguments.clone());
        }
        other => {
            analysis.argument_parse_result = ArgumentParseResult::ArgumentsNotObject;
            analysis.schema_validation_result = SchemaValidationResult::Failed;
            analysis.tool_failure_class = Some(ToolFailureClass::ArgumentInvalid);
            analysis.final_arguments_sent = Some(other.clone());
            analysis.validation_errors.push(TraceValidationError {
                code: "arguments_not_object".to_string(),
                path: "/arguments".to_string(),
                message: "call_tool.arguments must be a JSON object".to_string(),
                retryable: true,
            });
        }
    }
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
        "gents_fs: "
    } else if is_native_command_tool(tool_name) {
        "gents_exec: "
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
    ToolFailureClass::from_persisted(raw)
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

fn looks_like_deadline_or_inference_failure(lower: &str) -> bool {
    lower.contains("request deadline exceeded")
        || lower.contains("deadline exceeded")
        || lower.contains("inference timed out")
        || lower.contains("stream liveness timeout")
        || lower.contains("completion failed")
        || lower.contains("model unreachable")
}

fn retryable_for_failure_class(failure_class: ToolFailureClass) -> Option<bool> {
    match failure_class {
        ToolFailureClass::ServiceUnavailable
        | ToolFailureClass::External
        | ToolFailureClass::ArgumentInvalid => Some(true),
        ToolFailureClass::Transport => Some(true),
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
