//! Bucket 3 — runtime integration tests for the subagent extensions to
//! ToolCallLifecycle. Spins up a real EmbeddedNode via test_db() and exercises
//! every new transition end-to-end. Mirrors R1's
//! tool_call_lifecycle_conformance.rs structure.
//!
//! Tests 22-26 (in subsequent tasks) build on the two helpers defined here:
//!   - `make_completed_request`: creates a child AgentRequest in `.completed`
//!     state via direct DB writes, bypassing the normal request lifecycle.
//!   - `make_terminal_request`: same but for any non-completed terminal state
//!     ("failed", "dead", "interrupted", "superseded").

mod support;

// These imports are used by the helpers defined here and by the integration
// tests that Tasks 22-26 add to this file. Allow unused-import lint until the
// remainder of Bucket 3 is filled in.
#[allow(unused_imports)]
use defra_agent::tool_call_lifecycle::{
    AwaitMode, CancelPolicy, CascadeIntent, ChildTerminal, FailureClass,
    IllegalToolCallTransition, ToolCallLifecycle,
    create_subagent_request, MAX_SUBAGENT_DEPTH,
};
use support::test_db;

// ---------------------------------------------------------------------------
// Internal test helpers
// ---------------------------------------------------------------------------

/// Test helper: directly constructs a child AgentRequest in `.completed`
/// state via low-level DB writes, bypassing the normal request lifecycle.
/// Used by bridge_complete tests to set up "the child has finished" state
/// without R3's SubagentSource.
///
/// Uses the same full required-field set as `support::create_request` so
/// that DefraDB schema validation passes. Parent linkage fields are only
/// written when `Some`.
async fn make_completed_request(
    node: &defra_agent::defra_node::EmbeddedNode,
    request_id: &str,
    parent_request_id: Option<&str>,
    parent_tool_call_id: Option<&str>,
    final_message: &str,
) -> anyhow::Result<()> {
    let depth: u32 = if parent_request_id.is_some() { 1 } else { 0 };
    let parent_req_field = parent_request_id
        .map(|id| {
            let escaped = defra_agent::graphql::escape_graphql_string(id);
            format!(r#"caused_by_parent_request_id: "{escaped}","#)
        })
        .unwrap_or_default();
    let parent_tc_field = parent_tool_call_id
        .map(|id| {
            let escaped = defra_agent::graphql::escape_graphql_string(id);
            format!(r#"caused_by_parent_tool_call_id: "{escaped}","#)
        })
        .unwrap_or_default();
    let rid = defra_agent::graphql::escape_graphql_string(request_id);
    let content = defra_agent::graphql::escape_graphql_string(final_message);
    let now = chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let now_escaped = defra_agent::graphql::escape_graphql_string(&now);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{rid}",
                agent_did: "{agent_did}",
                behavior_id: "test",
                session_id: "{rid}",
                retry_parent_request: "",
                retry_root_request: "{rid}",
                superseded_by_request: "",
                content: "{content}",
                status: "completed",
                lifecycle_state: "completed",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{now_escaped}",
                retry_count: 0,
                max_retries: {max_retries},
                subagent_depth: {depth},
                {prf}
                {ptc}
            }}) {{ _docID }}
        }}"#,
        agent_did = support::AGENT_DID,
        max_retries = defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
        prf = parent_req_field,
        ptc = parent_tc_field,
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "make_completed_request failed: {:?}",
        resp.errors
    );
    Ok(())
}

/// Test helper: same as `make_completed_request` but for non-completed terminal
/// states: "failed", "dead", "interrupted", "superseded".
async fn make_terminal_request(
    node: &defra_agent::defra_node::EmbeddedNode,
    request_id: &str,
    parent_request_id: Option<&str>,
    parent_tool_call_id: Option<&str>,
    state: &str,
) -> anyhow::Result<()> {
    let depth: u32 = if parent_request_id.is_some() { 1 } else { 0 };
    let parent_req_field = parent_request_id
        .map(|id| {
            let escaped = defra_agent::graphql::escape_graphql_string(id);
            format!(r#"caused_by_parent_request_id: "{escaped}","#)
        })
        .unwrap_or_default();
    let parent_tc_field = parent_tool_call_id
        .map(|id| {
            let escaped = defra_agent::graphql::escape_graphql_string(id);
            format!(r#"caused_by_parent_tool_call_id: "{escaped}","#)
        })
        .unwrap_or_default();
    let rid = defra_agent::graphql::escape_graphql_string(request_id);
    let state_escaped = defra_agent::graphql::escape_graphql_string(state);
    let now = chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let now_escaped = defra_agent::graphql::escape_graphql_string(&now);
    // "dead"/"interrupted"/"superseded" are not valid `status` values in the
    // existing schema — the `status` field uses the legacy R1 vocabulary
    // ("pending", "processing", "completed", "error", "superseded") while
    // `lifecycle_state` carries the full R2 vocabulary. We normalise `status`
    // to the closest valid legacy value for schema compliance.
    let legacy_status = match state {
        "completed" => "completed",
        "superseded" => "superseded",
        "failed" | "dead" | "interrupted" => "error",
        other => other,
    };
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{rid}",
                agent_did: "{agent_did}",
                behavior_id: "test",
                session_id: "{rid}",
                retry_parent_request: "",
                retry_root_request: "{rid}",
                superseded_by_request: "",
                content: "",
                status: "{legacy_status}",
                lifecycle_state: "{state_escaped}",
                backend_id: "",
                execution_origin: "interactive",
                failure_reason: "",
                created_at: "{now_escaped}",
                retry_count: 0,
                max_retries: {max_retries},
                subagent_depth: {depth},
                {prf}
                {ptc}
            }}) {{ _docID }}
        }}"#,
        agent_did = support::AGENT_DID,
        max_retries = defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
        prf = parent_req_field,
        ptc = parent_tc_field,
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "make_terminal_request({state}) failed: {:?}",
        resp.errors
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Integration: background → foreground round-trip persists await_mode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn integration_background_then_foreground_persists_round_trip() {
    let db = test_db("tc-sa-int-1").await;

    let mut lc = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        "sess-int-1".to_string(),
        "tc-int-1".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-req-int-1".to_string(),
    );
    lc.start_running().await.unwrap();

    // Flip to background and verify persisted await_mode.
    lc.background().await.unwrap();
    let resp = db.node.execute(
        r#"{ AgentToolCall(filter: { tool_call_id: { _eq: "tc-int-1" } }) { await_mode } }"#
    ).await;
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    let data = resp.data.expect("data");
    let rows = data["AgentToolCall"].as_array().expect("array");
    assert_eq!(rows.len(), 1, "expected one row");
    assert_eq!(
        rows[0]["await_mode"].as_str(),
        Some("background"),
        "await_mode should be persisted as 'background' after background()"
    );

    // Flip back to foreground and verify persisted await_mode.
    lc.foreground().await.unwrap();
    let resp = db.node.execute(
        r#"{ AgentToolCall(filter: { tool_call_id: { _eq: "tc-int-1" } }) { await_mode } }"#
    ).await;
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    let data = resp.data.expect("data");
    let rows = data["AgentToolCall"].as_array().expect("array");
    assert_eq!(rows.len(), 1, "expected one row");
    assert_eq!(
        rows[0]["await_mode"].as_str(),
        Some("foreground"),
        "await_mode should be persisted as 'foreground' after foreground()"
    );

    // Calling foreground() again returns ModeAlreadyForeground.
    let err = lc.foreground().await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::ModeAlreadyForeground)
        ),
        "expected ModeAlreadyForeground on second foreground() call, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Integration: detach one-way persists cancel_policy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn integration_detach_one_way_persists() {
    let db = test_db("tc-sa-det-1").await;

    let mut lc = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        "sess-det-1".to_string(),
        "tc-det-1".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-req-det-1".to_string(),
    );
    lc.start_running().await.unwrap();

    lc.detach().await.unwrap();

    // Verify cancel_policy is persisted as "detach".
    let resp = db.node.execute(
        r#"{ AgentToolCall(filter: { tool_call_id: { _eq: "tc-det-1" } }) { cancel_policy } }"#
    ).await;
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);
    let data = resp.data.expect("data");
    let rows = data["AgentToolCall"].as_array().expect("array");
    assert_eq!(rows.len(), 1, "expected one row");
    assert_eq!(
        rows[0]["cancel_policy"].as_str(),
        Some("detach"),
        "cancel_policy should be persisted as 'detach' after detach()"
    );

    // detach again errors.
    let err = lc.detach().await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::PolicyAlreadyDetach)
        ),
        "expected PolicyAlreadyDetach on second detach() call, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Sanity test: make_completed_request creates a row with the expected state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_make_completed_request_creates_row() {
    let db = test_db("tc-sa-sanity-1").await;

    make_completed_request(
        &db.node,
        "req-sanity-1",
        None,
        None,
        "all done",
    )
    .await
    .unwrap();

    // Verify a row exists with lifecycle_state == "completed".
    let query = r#"{
        AgentRequest(filter: { request_id: { _eq: "req-sanity-1" } }) {
            request_id
            lifecycle_state
            subagent_depth
        }
    }"#;
    let resp = db.node.execute(query).await;
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);

    let data = resp.data.expect("data");
    let rows = data["AgentRequest"]
        .as_array()
        .expect("AgentRequest array");
    assert_eq!(rows.len(), 1, "expected exactly one AgentRequest row");

    let row = &rows[0];
    assert_eq!(
        row["lifecycle_state"].as_str(),
        Some("completed"),
        "expected lifecycle_state to be 'completed', got: {:?}",
        row["lifecycle_state"]
    );
    assert_eq!(
        row["subagent_depth"].as_i64(),
        Some(0),
        "top-level request should have subagent_depth 0"
    );
}

// ---------------------------------------------------------------------------
// Sanity test: make_completed_request with parent linkage sets depth=1
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_make_completed_request_child_sets_depth_and_parent_fields() {
    let db = test_db("tc-sa-sanity-2").await;

    make_completed_request(
        &db.node,
        "req-sanity-child-1",
        Some("req-parent-1"),
        Some("tc-parent-1"),
        "child done",
    )
    .await
    .unwrap();

    let query = r#"{
        AgentRequest(filter: { request_id: { _eq: "req-sanity-child-1" } }) {
            request_id
            lifecycle_state
            subagent_depth
            caused_by_parent_request_id
            caused_by_parent_tool_call_id
        }
    }"#;
    let resp = db.node.execute(query).await;
    assert!(!resp.has_errors(), "query failed: {:?}", resp.errors);

    let data = resp.data.expect("data");
    let rows = data["AgentRequest"]
        .as_array()
        .expect("AgentRequest array");
    assert_eq!(rows.len(), 1, "expected one row");

    let row = &rows[0];
    assert_eq!(
        row["lifecycle_state"].as_str(),
        Some("completed"),
        "lifecycle_state should be completed"
    );
    assert_eq!(
        row["subagent_depth"].as_i64(),
        Some(1),
        "child request should have subagent_depth 1"
    );
    assert_eq!(
        row["caused_by_parent_request_id"].as_str(),
        Some("req-parent-1"),
        "parent request id should be set"
    );
    assert_eq!(
        row["caused_by_parent_tool_call_id"].as_str(),
        Some("tc-parent-1"),
        "parent tool call id should be set"
    );
}

// ---------------------------------------------------------------------------
// Sanity test: make_terminal_request creates rows in each terminal state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_make_terminal_request_all_states() {
    let db = test_db("tc-sa-sanity-3").await;

    for (idx, state) in ChildTerminal::ALL_KIND.iter().enumerate() {
        let rid = format!("req-terminal-{idx}");
        make_terminal_request(&db.node, &rid, Some("req-parent-x"), Some("tc-parent-x"), state)
            .await
            .unwrap_or_else(|e| panic!("make_terminal_request({state}) failed: {e}"));

        let rid_escaped = defra_agent::graphql::escape_graphql_string(&rid);
        let query = format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{rid_escaped}" }} }}) {{
                    lifecycle_state
                }}
            }}"#
        );
        let resp = db.node.execute(&query).await;
        assert!(!resp.has_errors(), "query failed for state {state}: {:?}", resp.errors);

        let data = resp.data.expect("data");
        let rows = data["AgentRequest"].as_array().expect("array");
        assert_eq!(rows.len(), 1, "expected one row for state {state}");
        assert_eq!(
            rows[0]["lifecycle_state"].as_str(),
            Some(*state),
            "expected lifecycle_state={state}"
        );
    }
}
