use super::shared::{
    extract_text, lookup_service_query, mcp_service_allowed, MetaToolError, StructuredToolError,
};

fn make_call_result(texts: &[&str]) -> rmcp::model::CallToolResult {
    use rmcp::model::CallToolResult;

    let content = texts
        .iter()
        .map(|t| rmcp::model::Content::text(*t))
        .collect();

    CallToolResult::success(content)
}

#[test]
fn lookup_service_query_prefers_latest_online_row() {
    let query = lookup_service_query("x-data");
    assert!(query.contains(r#"service_id: { _eq: "x-data" }"#));
    assert!(query.contains(r#"status: { _eq: "online" }"#));
    assert!(query.contains(r#"order: { updated_at: DESC }"#));
    assert!(query.contains("limit: 1"));
}

#[test]
fn lookup_service_query_escapes_service_id() {
    let query = lookup_service_query("x\"data");
    assert!(query.contains(r#"service_id: { _eq: "x\"data" }"#));
}

#[test]
fn empty_mcp_allowlist_denies_every_service() {
    assert!(!mcp_service_allowed(&[], "x-data"));
    assert!(!mcp_service_allowed(&[], "observability-mcp"));
}

#[test]
fn mcp_allowlist_matches_service_id_exactly() {
    let allowlist = vec!["x-data".to_string(), "hf-data".to_string()];

    assert!(mcp_service_allowed(&allowlist, "x-data"));
    assert!(!mcp_service_allowed(&allowlist, "observability-mcp"));
}

#[test]
fn blocked_mcp_service_returns_tool_not_allowed_error() {
    let error = StructuredToolError::tool_not_allowed(
        "observability-mcp",
        "query_metrics",
        vec!["x-data".to_string()],
    );

    assert_eq!(error.failure_class, "tool_not_allowed");
    assert_eq!(error.path, "/service_id");
    assert!(!error.retryable);
    assert_eq!(error.service_id, "observability-mcp");
    assert_eq!(
        error.allowed_mcp_service_ids,
        Some(vec!["x-data".to_string()])
    );
}

#[test]
fn structured_meta_tool_error_becomes_a_typed_dispatch_failure() {
    let error = StructuredToolError::invalid_tool_arguments(
        "x-data",
        "query",
        "/arguments/limit",
        "limit must be an integer",
    );
    let dispatch_error = MetaToolError::structured(error).into_dispatch_error();
    let outcome =
        crate::tool_call_lifecycle::ToolOutcome::from_dispatch("call_tool", Err(dispatch_error));

    match outcome {
        crate::tool_call_lifecycle::ToolOutcome::Failed { class, text, .. } => {
            assert_eq!(
                class,
                crate::tool_call_lifecycle::FailureClass::ArgumentInvalid
            );
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["ok"], false);
            assert_eq!(value["failure_class"], "invalid_tool_arguments");
        }
        other => panic!("structured meta-tool error must be typed failed, got {other:?}"),
    }
}

#[test]
fn extract_text_empty_content() {
    let result = make_call_result(&[]);
    assert_eq!(extract_text(&result), "");
}

#[test]
fn extract_text_single_item() {
    let result = make_call_result(&["hello world"]);
    assert_eq!(extract_text(&result), "hello world");
}

#[test]
fn extract_text_multiple_items_joined_with_newline() {
    let result = make_call_result(&["first", "second", "third"]);
    assert_eq!(extract_text(&result), "first\nsecond\nthird");
}

#[test]
fn meta_tool_error_display_includes_context_chain() {
    let error = anyhow::anyhow!("missing field 'host'").context("MCP call_tool");
    let display = MetaToolError::from(error).to_string();

    assert!(display.contains("MCP call_tool"), "{display}");
    assert!(display.contains("missing field 'host'"), "{display}");
}
