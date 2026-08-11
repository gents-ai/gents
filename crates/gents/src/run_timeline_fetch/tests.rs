use super::*;

use super::*;

fn bindings() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("doc-root".to_string(), "req-root".to_string()),
        ("doc-child".to_string(), "req-child".to_string()),
    ])
}

#[test]
fn physical_request_edge_rejects_forged_logical_join() {
    let error = validate_required_request_binding(
        &bindings(),
        "AgentResponse",
        "response-1",
        "req-root",
        Some("doc-child"),
    )
    .expect_err("mismatched physical edge must fail closed");
    assert!(error.to_string().contains("belongs to req-child"));
}

#[test]
fn genuinely_unbound_context_row_is_permitted_but_half_binding_is_not() {
    validate_optional_request_binding(
        &bindings(),
        "AgentMessage",
        "message-context",
        None,
        None,
    )
    .expect("unbound context message");
    let error = validate_optional_request_binding(
        &bindings(),
        "AgentMessage",
        "message-forged",
        Some("req-root"),
        None,
    )
    .expect_err("partial binding must fail closed");
    assert!(error.to_string().contains("incomplete request lineage"));
}

#[test]
fn rendered_request_requires_the_same_physical_binding_as_other_timeline_rows() {
    let rendered = TimelineRenderedRequestRow {
        doc_id: Some("rendered-1".to_string()),
        capture_key: "capture-1".to_string(),
        request_id: Some("req-root".to_string()),
        request_doc_id: Some("doc-child".to_string()),
        ..Default::default()
    };
    let error = validate_request_scoped_rows(
        &bindings(),
        &[],
        &[],
        &[],
        &[],
        &[],
        &[rendered],
    )
    .expect_err("rendered request must not forge a logical/physical request pair");
    assert!(error.to_string().contains("RenderedRequest rendered-1"));
}

#[test]
fn session_rows_for_nested_requests_are_out_of_scope_without_hiding_forged_root_edges() {
    let bindings = bindings();
    assert!(!request_scoped_row_is_in_timeline(
        &bindings,
        Some("req-grandchild"),
        Some("doc-grandchild")
    ));
    assert!(request_scoped_row_is_in_timeline(
        &bindings,
        Some("req-root"),
        Some("doc-grandchild")
    ));
    assert!(!request_scoped_row_is_in_timeline(
        &bindings,
        Some("req-root"),
        None
    ));
    assert!(request_scoped_row_is_in_timeline(
        &bindings,
        None,
        Some("doc-root")
    ));
    assert!(!request_scoped_row_is_in_timeline(
        &bindings,
        Some("req-grandchild"),
        None
    ));
    assert!(request_scoped_row_is_in_timeline(&bindings, None, None));
}

#[test]
fn child_bridge_requires_the_exact_parent_tool_document() {
    let root = TimelineRequestRow {
        doc_id: Some("doc-root".to_string()),
        request_id: "req-root".to_string(),
        ..Default::default()
    };
    let child = TimelineRequestRow {
        doc_id: Some("doc-child".to_string()),
        request_id: "req-child".to_string(),
        caused_by_parent_request_id: Some("req-root".to_string()),
        caused_by_parent_request_doc_id: Some("doc-root".to_string()),
        caused_by_parent_tool_call_id: Some("call-parent".to_string()),
        caused_by_parent_tool_call_doc_id: Some("doc-forged-tool".to_string()),
        ..Default::default()
    };
    let tool = TimelineToolCallRow {
        doc_id: Some("doc-real-tool".to_string()),
        request_id: Some("req-root".to_string()),
        request_doc_id: Some("doc-root".to_string()),
        tool_call_id: "call-parent".to_string(),
        child_request_id: Some("req-child".to_string()),
        ..Default::default()
    };

    let error = validate_child_tool_bridges(&root, &[root.clone(), child], &[tool])
        .expect_err("forged tool document edge must fail closed");
    assert!(error.to_string().contains("missing AgentToolCall"));
}

#[test]
fn direct_child_without_tool_lineage_is_valid_but_half_bridge_is_rejected() {
    let root = TimelineRequestRow {
        doc_id: Some("doc-root".to_string()),
        request_id: "req-root".to_string(),
        ..Default::default()
    };
    let direct_child = TimelineRequestRow {
        doc_id: Some("doc-direct".to_string()),
        request_id: "req-direct".to_string(),
        caused_by_parent_request_id: Some("req-root".to_string()),
        caused_by_parent_request_doc_id: Some("doc-root".to_string()),
        ..Default::default()
    };
    validate_child_tool_bridges(&root, &[root.clone(), direct_child], &[])
        .expect("direct parent lineage does not fabricate a tool delegation");

    let half_bridge = TimelineRequestRow {
        doc_id: Some("doc-half".to_string()),
        request_id: "req-half".to_string(),
        caused_by_parent_request_id: Some("req-root".to_string()),
        caused_by_parent_request_doc_id: Some("doc-root".to_string()),
        caused_by_parent_tool_call_id: Some("call-only".to_string()),
        ..Default::default()
    };
    let error = validate_child_tool_bridges(&root, &[root.clone(), half_bridge], &[])
        .expect_err("half tool bridge must fail closed");
    assert!(error.to_string().contains("incomplete parent tool lineage"));
}
