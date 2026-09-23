use super::*;
use crate::llm::message::{Text, ToolCall, ToolFunction};
use gents_protocol::transcript::PresentedMessageRole;
use serde_json::json;
#[test]
fn failure_class_parser_accepts_only_canonical_persisted_values() {
    assert_eq!(
        failure_class_from_str("policyDenied"),
        Some(ToolFailureClass::PolicyDenied)
    );
    assert_eq!(failure_class_from_str("approvalDenied"), None);
    assert_eq!(failure_class_from_str("approval_denied"), None);
    assert_eq!(failure_class_from_str("tool_not_found"), None);
}

#[test]
fn completed_lifecycle_is_the_only_success_owner() {
    let analysis = analyze_tool_call_with_persisted_outcome(
        "read",
        r#"{"path":"README.md"}"#,
        "contents",
        ToolCallState::Completed,
        None,
    );

    assert!(analysis.tool_result_ok);
    assert_eq!(analysis.tool_failure_class, None);
    assert_eq!(
        analysis.argument_parse_result,
        ArgumentParseResult::ValidObject
    );
}

#[test]
fn persisted_failure_is_authoritative_over_success_looking_result_text() {
    let analysis = analyze_tool_call_with_persisted_outcome(
        "call_tool",
        r#"{"service_id":"x-data","tool_name":"query","arguments":{}}"#,
        "ordinary model-facing detail",
        ToolCallState::Failed,
        Some("serviceUnavailable"),
    );

    assert!(!analysis.tool_result_ok);
    assert_eq!(
        analysis.tool_failure_class,
        Some(ToolFailureClass::ServiceUnavailable)
    );
}

#[test]
fn completed_lifecycle_ignores_failure_shaped_result_text() {
    let analysis = analyze_tool_call_with_persisted_outcome(
        "read",
        r#"{"path":"README.md"}"#,
        "tool call failed: permission denied",
        ToolCallState::Completed,
        None,
    );

    assert!(analysis.tool_result_ok);
    assert_eq!(analysis.tool_failure_class, None);
    assert_eq!(analysis.tool_error, None);
}

#[test]
fn timeout_lifecycle_supplies_failure_class_when_absent() {
    let analysis = analyze_tool_call_with_persisted_outcome(
        "bash",
        r#"{"command":"sleep"}"#,
        "",
        ToolCallState::TimedOut,
        None,
    );

    assert!(!analysis.tool_result_ok);
    assert_eq!(
        analysis.tool_failure_class,
        Some(ToolFailureClass::External)
    );
}

#[test]
fn native_output_is_shape_only_and_does_not_override_lifecycle() {
    let result = concat!(
        "gents_exec: {\"ok\":false,\"status\":\"exit_nonzero\",",
        "\"command\":\"false\",\"exit_code\":2,\"timed_out\":false}\n",
        "stderr:\nfailed"
    );
    let analysis = analyze_tool_call_with_persisted_outcome(
        "bash",
        r#"{"command":"false"}"#,
        result,
        ToolCallState::Failed,
        Some("toolReturnedError"),
    );

    assert!(!analysis.tool_result_ok);
    let native = analysis.native_tool_output.expect("native output");
    assert!(!native.ok);
    assert_eq!(native.exit_code, Some(2));
    assert_eq!(native.status.as_deref(), Some("exit_nonzero"));
}

#[test]
fn call_tool_requires_object_arguments_without_string_coercion() {
    let analysis = analyze_tool_call_with_persisted_outcome(
        "call_tool",
        r#"{"service_id":"x-data","tool_name":"search","arguments":"{\"query\":\"gents\"}"}"#,
        "",
        ToolCallState::Failed,
        Some("argumentInvalid"),
    );

    assert_eq!(
        analysis.argument_parse_result,
        ArgumentParseResult::ArgumentsNotObject
    );
    assert_eq!(
        analysis.schema_validation_result,
        SchemaValidationResult::Failed
    );
    assert_eq!(analysis.validation_errors[0].path, "/arguments");
}

#[test]
fn extracts_raw_tool_call_json_from_persisted_assistant_message() {
    let message = Message::Assistant {
        id: None,
        content: vec![
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
        ],
    };
    let tool_call =
        extract_raw_tool_call_json(&message, "internal-1", "read").expect("tool call json");

    assert_eq!(tool_call["id"], "internal-1");
    assert_eq!(tool_call["function"]["name"], "read");
    assert_eq!(tool_call["function"]["arguments"]["path"], "README.md");
    assert!(extract_raw_tool_call_json(&message, "unknown", "read").is_none());
    assert!(extract_raw_tool_call_json(&message, "call-1", "read").is_none());
    assert!(extract_raw_tool_call_json(&message, "internal-1", "other").is_none());
}

#[test]
fn non_assistant_messages_yield_no_raw_tool_call_json() {
    let user = Message::User {
        content: vec![crate::llm::message::UserContent::tool_result(
            "call-1",
            vec![crate::llm::message::ToolResultContent::text("out")],
        )],
    };

    assert_eq!(extract_raw_tool_call_json(&user, "call-1", "read"), None);
}

#[test]
fn raw_message_json_serializes_the_native_message() {
    let message = Message::assistant("hello");
    let raw = raw_message_json(&message).expect("serialize native message");
    assert_eq!(raw["role"], "assistant");
    assert_eq!(raw["content"][0]["text"], "hello");
}

#[test]
fn presented_trace_rows_preserve_sequence_and_render_native_messages() {
    let message = Message::assistant("done");
    let presented = present_trace_message(SequencedMessage {
        sequence: 7,
        message: message.clone(),
    });

    assert_eq!(presented.sequence, 7);
    assert_eq!(presented.message, message);
    assert_eq!(presented.presentation.role, PresentedMessageRole::Assistant);
    assert_eq!(presented.presentation.body_markdown, "done");
}

#[test]
fn computes_latency_from_rfc3339_timestamps() {
    assert_eq!(
        latency_ms(
            Some("2026-01-01T00:00:00.100Z"),
            Some("2026-01-01T00:00:01.350Z")
        ),
        Some(1250)
    );
}

#[test]
fn ignores_negative_latency_from_rfc3339_timestamps() {
    assert_eq!(
        latency_ms(Some("2026-01-01T00:00:01Z"), Some("2026-01-01T00:00:00Z")),
        None
    );
}
