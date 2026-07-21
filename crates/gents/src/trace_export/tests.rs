use super::*;
use crate::llm::message::{Text, ToolCall, ToolFunction};

#[test]
fn canonical_failure_class_parser_preserves_approval_denied() {
    assert_eq!(
        failure_class_from_str("approvalDenied"),
        Some(ToolFailureClass::ApprovalDenied)
    );
}

#[test]
fn successful_completed_result_has_no_failure_class() {
    let analysis = analyze_tool_call("read", r#"{"path":"README.md"}"#, "contents", "completed");

    assert!(analysis.tool_result_ok);
    assert_eq!(analysis.tool_failure_class, None);
    assert_eq!(
        analysis.argument_parse_result,
        ArgumentParseResult::ValidObject
    );
}

#[test]
fn compact_contract_text_that_mentions_unknown_fields_is_successful() {
    let result = "## coding_overview\n\nInput contract:\n- Unknown top-level fields: rejected (`additionalProperties: false`)\n\nCommon mistakes:\n- Do not add unlisted top-level fields; this schema rejects unknown arguments.";
    let result = format!("{result}\n- Use the exact field names and JSON types shown above.");
    let analysis = analyze_tool_call(
        "describe_tool",
        r#"{"service_id":"coding-session-store","tool_name":"coding_overview"}"#,
        &result,
        "completed",
    );

    assert!(analysis.tool_result_ok);
    assert_eq!(analysis.tool_failure_class, None);
}

#[test]
fn completed_nonzero_command_exit_is_not_a_successful_outcome() {
    let result = "cwd: /tmp\ncommand: grep -P foo README.md\nexit_code: 2\nstdout:\n(empty)\nstderr:\ngrep: invalid option -- P";
    let analysis = analyze_tool_call("bash", r#"{"command":"grep"}"#, result, "completed");

    assert!(!analysis.tool_result_ok);
    assert_eq!(
        analysis.tool_failure_class,
        Some(ToolFailureClass::ToolReturnedError)
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
fn native_bash_output_metadata_drives_success_classification() {
    let result = concat!(
        "gents_exec: {\"ok\":true,\"status\":\"success\",\"command\":\"printf ok\",",
        "\"argv\":[\"printf\",\"ok\"],\"cwd\":\".\",\"exit_code\":0,",
        "\"timed_out\":false,\"duration_ms\":4,\"timeout_ms\":10000,",
        "\"execution_mode\":\"read_only\",\"network_mode\":\"inherit\",",
        "\"sandbox\":\"policy_read_only\",\"stdout_truncation\":{\"returned_bytes\":2,",
        "\"total_bytes\":2,\"max_bytes\":16000,\"truncated\":false},",
        "\"stderr_truncation\":{\"returned_bytes\":0,\"total_bytes\":0,",
        "\"max_bytes\":16000,\"truncated\":false}}\n",
        "stdout:\n",
        "ok\n",
        "stderr:\n",
        "(empty)"
    );
    let analysis = analyze_tool_call(
        "bash",
        r#"{"command":"printf","args":["ok"]}"#,
        result,
        "completed",
    );

    assert!(analysis.tool_result_ok);
    assert_eq!(analysis.tool_failure_class, None);
    let native = analysis.native_tool_output.expect("native output");
    assert!(native.ok);
    assert_eq!(native.command.as_deref(), Some("printf ok"));
    assert_eq!(native.exit_code, Some(0));
    assert_eq!(native.timed_out, Some(false));
    assert_eq!(native.execution_mode.as_deref(), Some("read_only"));
    assert_eq!(native.network_mode.as_deref(), Some("inherit"));
    assert_eq!(native.sandbox.as_deref(), Some("policy_read_only"));
}

#[test]
fn native_bash_nonzero_metadata_is_not_successful() {
    let result = concat!(
        "gents_exec: {\"ok\":false,\"status\":\"exit_nonzero\",",
        "\"command\":\"grep -P foo README.md\",\"argv\":[\"grep\",\"-P\",\"foo\",\"README.md\"],",
        "\"cwd\":\".\",\"exit_code\":2,\"timed_out\":false,\"duration_ms\":4,",
        "\"timeout_ms\":10000,\"execution_mode\":\"read_only\",",
        "\"network_mode\":\"inherit\",\"sandbox\":\"policy_read_only\",",
        "\"stdout_truncation\":{\"returned_bytes\":0,\"total_bytes\":0,",
        "\"max_bytes\":16000,\"truncated\":false},",
        "\"stderr_truncation\":{\"returned_bytes\":24,\"total_bytes\":24,",
        "\"max_bytes\":16000,\"truncated\":false}}\n",
        "stdout:\n",
        "(empty)\n",
        "stderr:\n",
        "grep: invalid option -- P"
    );
    let analysis = analyze_tool_call("bash", r#"{"command":"grep"}"#, result, "completed");

    assert!(!analysis.tool_result_ok);
    assert_eq!(
        analysis.tool_failure_class,
        Some(ToolFailureClass::ToolReturnedError)
    );
    let native = analysis.native_tool_output.expect("native output");
    assert!(!native.ok);
    assert_eq!(native.status.as_deref(), Some("exit_nonzero"));
    assert_eq!(native.exit_code, Some(2));
}

#[test]
fn native_bash_timeout_metadata_is_retryable_timeout() {
    let result = concat!(
        "gents_exec: {\"ok\":false,\"status\":\"timeout\",\"command\":\"sleep 2\",",
        "\"argv\":[\"sleep\",\"2\"],\"cwd\":\".\",\"exit_code\":null,",
        "\"timed_out\":true,\"duration_ms\":1000,\"timeout_ms\":1000,",
        "\"execution_mode\":\"read_only\",\"network_mode\":\"inherit\",",
        "\"sandbox\":\"policy_read_only\",\"stdout_truncation\":{\"returned_bytes\":0,",
        "\"total_bytes\":0,\"max_bytes\":16000,\"truncated\":false},",
        "\"stderr_truncation\":{\"returned_bytes\":0,\"total_bytes\":0,",
        "\"max_bytes\":16000,\"truncated\":false}}\n",
        "stdout:\n",
        "(empty)\n",
        "stderr:\n",
        "(empty)"
    );
    let analysis = analyze_tool_call("bash", r#"{"command":"sleep"}"#, result, "completed");

    assert!(!analysis.tool_result_ok);
    assert_eq!(
        analysis.tool_failure_class,
        Some(ToolFailureClass::External)
    );
    assert_eq!(
        analysis
            .tool_error
            .as_ref()
            .and_then(|error| error.retryable),
        Some(true)
    );
    assert_eq!(
        analysis
            .native_tool_output
            .as_ref()
            .and_then(|native| native.timed_out),
        Some(true)
    );
}

#[test]
fn native_file_output_metadata_prevents_false_positive_text_classification() {
    let result = concat!(
        "gents_fs: {\"ok\":true,\"status\":\"success\",\"tool\":\"read_file\",",
        "\"path\":\"notes.txt\",\"returned_count\":1,\"total_count\":1,",
        "\"truncated\":false}\n",
        "content:\n",
        "L1: no such file or directory is just file content"
    );
    let analysis = analyze_tool_call("read_file", r#"{"path":"notes.txt"}"#, result, "completed");

    assert!(analysis.tool_result_ok);
    assert_eq!(analysis.tool_failure_class, None);
    let native = analysis.native_tool_output.expect("native output");
    assert!(native.ok);
    assert_eq!(native.tool.as_deref(), Some("read_file"));
    assert_eq!(native.path.as_deref(), Some("notes.txt"));
    assert_eq!(native.returned_count, Some(1));
    assert_eq!(native.total_count, Some(1));
}

#[test]
fn native_file_raw_json_output_is_parsed_for_trace_export() {
    let result = json!({
        "ok": true,
        "status": "success",
        "tool": "grep",
        "path": ".",
        "pattern": "println",
        "returned_count": 1,
        "total_count": 1,
        "truncated": false,
        "default_ignored": [".git", "target"],
        "matches": [{
            "path": "src/main.rs",
            "line_number": 2,
            "preview": "println!(\"hello\")"
        }]
    })
    .to_string();
    let analysis = analyze_tool_call(
        "grep",
        r#"{"pattern":"println","path":"."}"#,
        &result,
        "completed",
    );

    assert!(analysis.tool_result_ok);
    let native = analysis.native_tool_output.expect("native output");
    assert_eq!(native.tool.as_deref(), Some("grep"));
    assert_eq!(native.pattern.as_deref(), Some("println"));
    assert_eq!(
        native.default_ignored,
        Some(vec![".git".into(), "target".into()])
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
        Some(ToolFailureClass::ServiceUnavailable)
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
        Some(ToolFailureClass::ArgumentInvalid)
    );
    assert_eq!(
        analysis.schema_validation_result,
        SchemaValidationResult::Failed
    );
    assert_eq!(analysis.validation_errors[0].path, "/arguments/query");
    let error = analysis.tool_error.as_ref().expect("tool error");
    assert_eq!(error.failure_class, ToolFailureClass::ArgumentInvalid);
    assert_eq!(error.service_id.as_deref(), Some("x-data"));
    assert_eq!(error.tool_name.as_deref(), Some("search_bookmarks"));
    assert_eq!(error.path.as_deref(), Some("/arguments/query"));
    assert_eq!(error.retryable, Some(true));
}

#[test]
fn structured_describe_tool_missing_tool_preserves_available_tools() {
    let result = json!({
            "ok": false,
            "failure_class": "tool_not_found",
            "path": "/tool_name",
            "message": "tool 'search_post' was not found on service 'x-data'; available tools: search_posts",
            "retryable": true,
            "service_id": "x-data",
            "tool_name": "search_post",
            "requested_tool_name": "search_post",
            "available_tools": ["search_posts"]
        })
        .to_string();
    let analysis = analyze_tool_call(
        "describe_tool",
        r#"{"service_id":"x-data","tool_name":"search_post"}"#,
        &result,
        "completed",
    );

    assert!(!analysis.tool_result_ok);
    assert_eq!(
        analysis.tool_failure_class,
        Some(ToolFailureClass::ServiceUnavailable)
    );
    assert_eq!(analysis.validation_errors[0].code, "serviceUnavailable");
    assert_eq!(analysis.validation_errors[0].path, "/tool_name");
    let error = analysis.tool_error.as_ref().expect("tool error");
    assert_eq!(error.failure_class, ToolFailureClass::ServiceUnavailable);
    assert_eq!(error.service_id.as_deref(), Some("x-data"));
    assert_eq!(error.tool_name.as_deref(), Some("search_post"));
    assert_eq!(error.requested_tool_name.as_deref(), Some("search_post"));
    assert_eq!(error.retryable, Some(true));
    assert_eq!(
        error.available_tools,
        Some(vec!["search_posts".to_string()])
    );
}

#[test]
fn structured_tool_not_allowed_is_non_retryable() {
    let result = json!({
        "ok": false,
        "failure_class": "tool_not_allowed",
        "path": "/service_id",
        "message": "service 'observability-mcp' is not allowed for this behavior",
        "retryable": false,
        "service_id": "observability-mcp",
        "tool_name": "query_metrics",
        "requested_tool_name": "query_metrics",
        "allowed_mcp_service_ids": ["x-data"]
    })
    .to_string();
    let analysis = analyze_tool_call(
        "call_tool",
        r#"{"service_id":"observability-mcp","tool_name":"query_metrics","arguments":{}}"#,
        &result,
        "completed",
    );

    assert!(!analysis.tool_result_ok);
    // The failure class still rebuckets to ServiceUnavailable per the
    // 5-variant collapse, but trace consumers see the raw "tool_not_allowed"
    // string preserved as the policy-block reason via
    // structured_tool_error_code_from_result.
    assert_eq!(
        analysis.tool_failure_class,
        Some(ToolFailureClass::ServiceUnavailable)
    );
    assert_eq!(analysis.validation_errors[0].code, "tool_not_allowed");
    assert_eq!(analysis.validation_errors[0].path, "/service_id");
    assert_eq!(analysis.validation_errors[0].retryable, false);
    let error = analysis.tool_error.as_ref().expect("tool error");
    assert_eq!(error.failure_class, ToolFailureClass::ServiceUnavailable);
    assert_eq!(error.service_id.as_deref(), Some("observability-mcp"));
    assert_eq!(error.tool_name.as_deref(), Some("query_metrics"));
    assert_eq!(error.retryable, Some(false));
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
        Some(ToolFailureClass::ArgumentInvalid)
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
        Some(ToolFailureClass::ArgumentInvalid)
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
        Some(ToolFailureClass::ServiceUnavailable)
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
        Some(ToolFailureClass::ServiceUnavailable)
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
        Some(ToolFailureClass::ToolReturnedError)
    );
    let error = analysis.tool_error.as_ref().expect("tool error");
    assert_eq!(error.failure_class, ToolFailureClass::ToolReturnedError);
    assert_eq!(error.retryable, Some(false));
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
        Some(ToolFailureClass::External)
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
        Some(ToolFailureClass::ArgumentInvalid)
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
        Some(ToolFailureClass::ArgumentInvalid)
    );
}

#[test]
fn classifies_stringified_call_tool_arguments_that_are_not_json() {
    let analysis = analyze_tool_call(
        "call_tool",
        r#"{"service_id":"x-data","tool_name":"search","arguments":"query=gents"}"#,
        "",
        "called",
    );

    assert_eq!(
        analysis.argument_parse_result,
        ArgumentParseResult::InvalidJson
    );
    assert_eq!(
        analysis.tool_failure_class,
        Some(ToolFailureClass::ArgumentInvalid)
    );
    assert_eq!(analysis.validation_errors[0].path, "/arguments");
    assert_eq!(
        analysis.final_arguments_sent,
        Some(json!({ "input": "query=gents" }))
    );
}

#[test]
fn normalizes_stringified_call_tool_object_arguments() {
    let analysis = analyze_tool_call(
        "call_tool",
        r#"{"service_id":"x-data","tool_name":"search","arguments":"{\"query\":\"gents\"}"}"#,
        "ok",
        "completed",
    );

    assert!(analysis.tool_result_ok);
    assert_eq!(analysis.tool_failure_class, None);
    assert_eq!(
        analysis.final_arguments_sent,
        Some(json!({ "query": "gents" }))
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
        Some(ToolFailureClass::External)
    );
}

#[test]
fn classifies_uncompleted_tool_call_without_tool_error_as_unclassified() {
    let analysis = analyze_tool_call("read", r#"{"path":"README.md"}"#, "", "called");

    assert!(!analysis.tool_result_ok);
    assert_eq!(
        analysis.tool_failure_class,
        Some(ToolFailureClass::ToolReturnedError)
    );
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

#[test]
fn ignores_negative_latency_from_rfc3339_timestamps() {
    assert_eq!(
        latency_ms(Some("2026-05-04T12:00:01Z"), Some("2026-05-04T12:00:00Z")),
        None
    );
}
