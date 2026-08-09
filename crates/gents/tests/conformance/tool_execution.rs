use crate::support::snapshots::fetch_tool_call_snapshots_for_session;
use gents::tool_call_lifecycle::{CancelCause, FailureClass, ToolCallLifecycle};

use crate::lean_vocab_test::{lean_tool_preflight_case, lean_tool_retry_case};

fn test_deadline() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + chrono::Duration::minutes(5)
}

async fn test_db(name: &str) -> crate::support::TestDb {
    crate::signed_materializer_test_db(name).await
}

#[tokio::test]
async fn lifecycle_pending_to_running_to_completed_persists_correctly() {
    let db = test_db("tc-lc-1").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-1".into(),
        "test-session-1".into(),
        "did:test:test".to_string(),
        "tool-call-1".into(),
        0,
        "test_tool".into(),
        r#"{"x":1}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, "test-session-1").await;
    assert_eq!(snapshots.len(), 1, "one row after start_running");
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("running"),
        "lifecycle_state should be running after start_running"
    );
    assert_eq!(snapshots[0].request_id.as_deref(), Some("request-1"));
    assert!(
        snapshots[0]
            .deadline_at
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        "deadline_at should persist on running tool calls"
    );

    lc.complete("ok").await.unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, "test-session-1").await;
    assert_eq!(snapshots.len(), 1, "still one row after complete");
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("completed"),
        "lifecycle_state should be completed after complete()"
    );
    assert_exact_output_fact_bound(&db.node, &snapshots[0], "ok").await;
}

#[tokio::test]
async fn durable_output_reader_reconstructs_the_exact_bound_result() {
    let db = test_db("tc-exact-output-reader").await;
    let session_id = "exact-output-reader-session";
    let tool_call_id = "exact-output-reader-call";
    let mut lifecycle = ToolCallLifecycle::new(
        db.node.clone(),
        "exact-output-reader-request".into(),
        session_id.into(),
        "did:test:test".to_string(),
        tool_call_id.into(),
        0,
        "test_tool".into(),
        r#"{"x":1}"#.into(),
        test_deadline(),
    );

    lifecycle.start_running().await.unwrap();
    lifecycle.complete("durable exact output").await.unwrap();

    let output = gents::session::load_tool_call_result(&db.node, session_id, tool_call_id)
        .await
        .expect("the exact signed result edge should reconstruct output");
    assert_eq!(output, "durable exact output");
}

#[tokio::test]
async fn durable_output_reader_refuses_a_terminal_omission() {
    let db = test_db("tc-exact-output-omission-reader").await;
    let session_id = "exact-output-omission-session";
    let tool_call_id = "exact-output-omission-call";
    let mut lifecycle = ToolCallLifecycle::new(
        db.node.clone(),
        "exact-output-omission-request".into(),
        session_id.into(),
        "did:test:test".to_string(),
        tool_call_id.into(),
        0,
        "test_tool".into(),
        r#"{"x":1}"#.into(),
        test_deadline(),
    );

    lifecycle.start_running().await.unwrap();
    lifecycle.timeout().await.unwrap();

    let error = gents::session::load_tool_call_result(&db.node, session_id, tool_call_id)
        .await
        .expect_err("an omission fact must never be projected as tool output");
    assert!(
        error.to_string().contains("has no output (timedOut)"),
        "unexpected omission read error: {error:#}"
    );
}

async fn publish_unbound_omission_proposal(
    node: &gents::defra_node::EmbeddedNode,
    call: &crate::support::snapshots::ToolCallSnapshot,
    source_phase: &str,
    terminal_phase: &str,
    reason: &str,
    detail: &str,
) {
    let commits = node
        .execute(&format!(
            r#"{{ _commits(docID: ["{}"], filter: {{ fieldName: {{ _eq: "_C" }} }}) {{ cid heads {{ cid fieldName }} }} }}"#,
            gents::graphql::escape_graphql_string(&call.doc_id),
        ))
        .await;
    assert!(!commits.has_errors(), "commit query: {:?}", commits.errors);
    let rows = commits
        .data
        .as_ref()
        .and_then(|data| data.get("_commits"))
        .and_then(serde_json::Value::as_array)
        .expect("composite commit rows");
    let nested = rows
        .iter()
        .flat_map(|row| {
            row.get("heads")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|head| head.get("fieldName").and_then(serde_json::Value::as_str) == Some("_C"))
        .filter_map(|head| head.get("cid").and_then(serde_json::Value::as_str))
        .collect::<std::collections::HashSet<_>>();
    let current = rows
        .iter()
        .filter_map(|row| row.get("cid").and_then(serde_json::Value::as_str))
        .find(|cid| !nested.contains(cid))
        .expect("sole current composite CID");
    let signer = node
        .verified_block_signer_did(current)
        .await
        .expect("verified current call signer");
    let mutation = format!(
        r#"mutation {{ create_AgentToolOutputOmission(input: {{
            omission_key: "{cid}"
            tool_call_key: "{key}"
            tool_call_doc_id: "{doc_id}"
            tool_call_composite_commit_cid: "{cid}"
            tool_call_signer_did: "{signer}"
            agent_did: "did:test:test"
            session_id: "{session_id}"
            source_phase: "{source_phase}"
            terminal_phase: "{terminal_phase}"
            reason: "{reason}"
            detail: "{detail}"
            created_at: "{}"
        }}) {{ _docID }} }}"#,
        chrono::Utc::now().to_rfc3339(),
        cid = gents::graphql::escape_graphql_string(current),
        key = gents::graphql::escape_graphql_string(&format!(
            "{}:{}",
            call.session_id, call.tool_call_id
        )),
        doc_id = gents::graphql::escape_graphql_string(&call.doc_id),
        signer = gents::graphql::escape_graphql_string(&signer),
        session_id = gents::graphql::escape_graphql_string(&call.session_id),
        detail = gents::graphql::escape_graphql_string(detail),
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "publishing unbound omission proposal: {:?}",
        response.errors
    );
}

#[tokio::test]
async fn bound_result_remains_authoritative_with_an_unbound_omission_proposal() {
    let db = test_db("tc-bound-result-with-orphan-proposal").await;
    let session_id = "bound-result-with-orphan-session";
    let tool_call_id = "bound-result-with-orphan-call";
    let mut lifecycle = ToolCallLifecycle::new(
        db.node.clone(),
        "bound-result-with-orphan-request".into(),
        session_id.into(),
        "did:test:test".to_string(),
        tool_call_id.into(),
        0,
        "test_tool".into(),
        "{}".into(),
        test_deadline(),
    );
    lifecycle.start_running().await.unwrap();
    let call = fetch_tool_call_snapshots_for_session(&db.node, session_id)
        .await
        .remove(0);
    publish_unbound_omission_proposal(
        &db.node,
        &call,
        "running",
        "cancelled",
        "cancelled",
        "losing concurrent cancellation",
    )
    .await;

    lifecycle.complete("winning exact output").await.unwrap();
    assert_eq!(
        gents::session::load_tool_call_result(&db.node, session_id, tool_call_id)
            .await
            .unwrap(),
        "winning exact output"
    );
}

#[tokio::test]
async fn conflicting_unbound_omission_does_not_wedge_terminal_recovery() {
    let db = test_db("tc-conflicting-unbound-omission").await;
    let session_id = "conflicting-unbound-omission-session";
    let tool_call_id = "conflicting-unbound-omission-call";
    let mut lifecycle = ToolCallLifecycle::new(
        db.node.clone(),
        "conflicting-unbound-omission-request".into(),
        session_id.into(),
        "did:test:test".to_string(),
        tool_call_id.into(),
        0,
        "test_tool".into(),
        "{}".into(),
        test_deadline(),
    );
    lifecycle.start_running().await.unwrap();
    let call = fetch_tool_call_snapshots_for_session(&db.node, session_id)
        .await
        .remove(0);
    publish_unbound_omission_proposal(
        &db.node,
        &call,
        "running",
        "cancelled",
        "cancelled",
        "proposal abandoned before binding",
    )
    .await;

    lifecycle.timeout().await.unwrap();
    let terminal = fetch_tool_call_snapshots_for_session(&db.node, session_id)
        .await
        .remove(0);
    assert_exact_omission_fact_bound(&db.node, &terminal, "running", "timedOut", "timedOut").await;
}

#[tokio::test]
async fn lifecycle_running_to_failed_persists_failure_class() {
    let db = test_db("tc-lc-2").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-2".into(),
        "test-session-2".into(),
        "did:test:test".to_string(),
        "tool-call-2".into(),
        0,
        "test_tool".into(),
        r#"{"x":1}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();
    lc.fail("error message", FailureClass::ToolReturnedError)
        .await
        .unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, "test-session-2").await;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("failed"),
        "lifecycle_state should be failed after fail()"
    );
    assert_eq!(
        snapshots[0].tool_failure_class.as_deref(),
        Some("toolReturnedError"),
        "tool_failure_class should be toolReturnedError"
    );
    assert_exact_output_fact_bound(&db.node, &snapshots[0], "error message").await;
}

async fn assert_exact_output_fact_bound(
    node: &gents::defra_node::EmbeddedNode,
    call: &crate::support::snapshots::ToolCallSnapshot,
    expected_output: &str,
) {
    let result_doc_id = call
        .result_doc_id
        .as_deref()
        .expect("terminal tool execution must bind an output fact _docID");
    assert!(call
        .result_composite_commit_cid
        .as_deref()
        .is_some_and(|cid| !cid.is_empty()));
    assert!(call
        .result_signer_did
        .as_deref()
        .is_some_and(|did| !did.is_empty()));

    let query = format!(
        r#"{{ AgentToolResult(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ tool_call_doc_id output_text }} }}"#,
        gents::graphql::escape_graphql_string(result_doc_id),
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "exact AgentToolResult query failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolResult"))
        .and_then(serde_json::Value::as_array)
        .expect("AgentToolResult rows");
    let [result] = rows.as_slice() else {
        panic!("expected one exact AgentToolResult, got {}", rows.len());
    };
    assert_eq!(
        result
            .get("tool_call_doc_id")
            .and_then(serde_json::Value::as_str),
        Some(call.doc_id.as_str())
    );
    assert_eq!(
        result
            .get("output_text")
            .and_then(serde_json::Value::as_str),
        Some(expected_output)
    );
    assert!(
        call.omission_doc_id.is_none(),
        "output and omission are exclusive"
    );
}

async fn assert_exact_omission_fact_bound(
    node: &gents::defra_node::EmbeddedNode,
    call: &crate::support::snapshots::ToolCallSnapshot,
    expected_source: &str,
    expected_terminal: &str,
    expected_reason: &str,
) {
    assert!(
        call.result_doc_id.is_none(),
        "output and omission are exclusive"
    );
    let omission_doc_id = call
        .omission_doc_id
        .as_deref()
        .expect("terminal tool execution without output must bind omission _docID");
    assert!(call
        .omission_composite_commit_cid
        .as_deref()
        .is_some_and(|cid| !cid.is_empty()));
    assert!(call
        .omission_signer_did
        .as_deref()
        .is_some_and(|did| !did.is_empty()));
    let query = format!(
        r#"{{ AgentToolOutputOmission(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
            tool_call_doc_id source_phase terminal_phase reason
        }} }}"#,
        gents::graphql::escape_graphql_string(omission_doc_id),
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "omission query: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolOutputOmission"))
        .and_then(serde_json::Value::as_array)
        .expect("omission rows");
    let [omission] = rows.as_slice() else {
        panic!("expected one exact omission, got {}", rows.len());
    };
    assert_eq!(omission["tool_call_doc_id"], call.doc_id);
    assert_eq!(omission["source_phase"], expected_source);
    assert_eq!(omission["terminal_phase"], expected_terminal);
    assert_eq!(omission["reason"], expected_reason);
}

#[tokio::test]
async fn startup_pending_recovery_preserves_live_dispatch_and_closes_expired_orphan() {
    let db = crate::signed_materializer_test_db("tc-lc-recover-pending").await;
    let agent_did = crate::signed_materializer_agent_did(&db).to_string();
    crate::support::create_request_for_agent(
        db.node.as_ref(),
        "request-recover-pending-live",
        "session-recover-pending",
        &agent_did,
        "processing",
        "2026-08-08T00:00:00Z",
    )
    .await;
    let active_deadline = test_deadline().to_rfc3339();
    let expired_deadline = (chrono::Utc::now() - chrono::Duration::seconds(5)).to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            live: create_AgentToolCall(input: {{
                tool_call_key: "session-recover-pending:call-recover-pending-live"
                request_id: "request-recover-pending-live"
                session_id: "session-recover-pending"
                agent_did: "{}"
                message_sequence: 0
                tool_name: "test_tool"
                tool_call_id: "call-recover-pending-live"
                args: "{{}}"
                result: ""
                status: "called"
                lifecycle_state: "pending"
                deadline_at: "{}"
                await_mode: "foreground"
                cancel_policy: "cascade"
            }}) {{ _docID }}
            orphan: create_AgentToolCall(input: {{
                tool_call_key: "session-recover-pending:call-recover-pending-orphan"
                request_id: "request-recover-pending-missing"
                session_id: "session-recover-pending"
                agent_did: "{}"
                message_sequence: 1
                tool_name: "test_tool"
                tool_call_id: "call-recover-pending-orphan"
                args: "{{}}"
                result: ""
                status: "called"
                lifecycle_state: "pending"
                deadline_at: "{}"
                await_mode: "foreground"
                cancel_policy: "cascade"
            }}) {{ _docID }}
        }}"#,
        gents::graphql::escape_graphql_string(&agent_did),
        active_deadline,
        gents::graphql::escape_graphql_string(&agent_did),
        expired_deadline,
    );
    let response = db.node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "seed pending execution: {:?}",
        response.errors
    );

    let report = ToolCallLifecycle::recover_all(&db.node, &agent_did)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);
    let snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "session-recover-pending").await;
    let [live, orphan] = snapshots.as_slice() else {
        panic!("expected live and orphan pending executions");
    };
    assert_eq!(live.tool_call_id, "call-recover-pending-live");
    assert_eq!(live.lifecycle_state.as_deref(), Some("pending"));
    assert!(live.omission_doc_id.is_none());
    assert_eq!(orphan.tool_call_id, "call-recover-pending-orphan");
    assert_eq!(orphan.lifecycle_state.as_deref(), Some("failed"));
    assert_exact_omission_fact_bound(&db.node, orphan, "pending", "failed", "preDispatchFailure")
        .await;

    let replay = ToolCallLifecycle::recover_all(&db.node, &agent_did)
        .await
        .unwrap();
    assert_eq!(replay.tool_calls_recovered, 0);
}

#[tokio::test]
async fn startup_recovery_cancels_expired_staged_fork_rows_with_exact_omissions() {
    let db = crate::signed_materializer_test_db("tc-lc-recover-staged-fork").await;
    let agent_did = crate::signed_materializer_agent_did(&db).to_string();
    let session_id = "session-recover-staged-fork";
    let now = chrono::Utc::now();
    let expired = (now - chrono::Duration::minutes(5)).to_rfc3339();
    let active_lease = (now + chrono::Duration::minutes(5)).to_rfc3339();
    let started = (now - chrono::Duration::minutes(10)).to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            pending: create_AgentToolCall(input: {{
                tool_call_key: "{session_id}:staged-pending"
                request_id: "request-staged-pending"
                session_id: "{session_id}"
                agent_did: "{agent_did}"
                message_sequence: 0
                tool_name: "test_tool"
                tool_call_id: "staged-pending"
                args: "{{}}"
                result: ""
                status: "forkStaging"
                lifecycle_state: "pending"
                deadline_at: "{expired}"
                await_mode: "foreground"
                cancel_policy: "cascade"
                fork_source_doc_id: "source-pending-doc"
                fork_source_composite_commit_cid: "source-pending-cid"
                fork_source_signer_did: "{agent_did}"
            }}) {{ _docID }}
            running: create_AgentToolCall(input: {{
                tool_call_key: "{session_id}:staged-running"
                request_id: "request-staged-running"
                session_id: "{session_id}"
                agent_did: "{agent_did}"
                message_sequence: 1
                tool_name: "test_tool"
                tool_call_id: "staged-running"
                args: "{{}}"
                result: ""
                status: "forkStaging"
                lifecycle_state: "running"
                started_at: "{started}"
                deadline_at: "{expired}"
                await_mode: "foreground"
                cancel_policy: "cascade"
                fork_source_doc_id: "source-running-doc"
                fork_source_composite_commit_cid: "source-running-cid"
                fork_source_signer_did: "{agent_did}"
            }}) {{ _docID }}
            active: create_AgentToolCall(input: {{
                tool_call_key: "{session_id}:staged-active"
                request_id: "request-staged-active"
                session_id: "{session_id}"
                agent_did: "{agent_did}"
                message_sequence: 2
                tool_name: "test_tool"
                tool_call_id: "staged-active"
                args: "{{}}"
                result: ""
                status: "forkStaging"
                lifecycle_state: "pending"
                deadline_at: "{active_lease}"
                await_mode: "foreground"
                cancel_policy: "cascade"
                fork_source_doc_id: "source-active-doc"
                fork_source_composite_commit_cid: "source-active-cid"
                fork_source_signer_did: "{agent_did}"
            }}) {{ _docID }}
            unleased: create_AgentToolCall(input: {{
                tool_call_key: "{session_id}:staged-unleased"
                request_id: "request-staged-unleased"
                session_id: "{session_id}"
                agent_did: "{agent_did}"
                message_sequence: 3
                tool_name: "test_tool"
                tool_call_id: "staged-unleased"
                args: "{{}}"
                result: ""
                status: "forkStaging"
                lifecycle_state: "pending"
                await_mode: "foreground"
                cancel_policy: "cascade"
                fork_source_doc_id: "source-unleased-doc"
                fork_source_composite_commit_cid: "source-unleased-cid"
                fork_source_signer_did: "{agent_did}"
            }}) {{ _docID }}
        }}"#,
        session_id = gents::graphql::escape_graphql_string(session_id),
        agent_did = gents::graphql::escape_graphql_string(&agent_did),
    );
    let response = db.node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "seed staged fork recovery rows: {:?}",
        response.errors
    );

    let report = ToolCallLifecycle::recover_all(&db.node, &agent_did)
        .await
        .unwrap();
    assert_eq!(
        report.tool_calls_recovered, 2,
        "expired incomplete fork staging must converge to explicit terminal evidence"
    );
    assert_eq!(report.notifications_repaired, 0);

    let rows = fetch_tool_call_snapshots_for_session(&db.node, session_id).await;
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].tool_call_id, "staged-pending");
    assert_eq!(rows[0].lifecycle_state.as_deref(), Some("cancelled"));
    assert!(rows[0].omission_doc_id.is_some());
    assert_eq!(rows[1].tool_call_id, "staged-running");
    assert_eq!(rows[1].lifecycle_state.as_deref(), Some("cancelled"));
    assert!(rows[1].result_doc_id.is_none());
    assert!(rows[1].omission_doc_id.is_some());
    assert_eq!(rows[2].tool_call_id, "staged-active");
    assert_eq!(rows[2].lifecycle_state.as_deref(), Some("pending"));
    assert!(rows[2].omission_doc_id.is_none());
    assert_eq!(rows[3].tool_call_id, "staged-unleased");
    assert_eq!(rows[3].lifecycle_state.as_deref(), Some("pending"));
    assert!(rows[3].omission_doc_id.is_none());
}

#[tokio::test]
async fn lifecycle_terminal_irreversibility() {
    let db = test_db("tc-lc-3").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-3".into(),
        "test-session-3".into(),
        "did:test:test".to_string(),
        "tool-call-3".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();
    lc.complete("done").await.unwrap();

    let err = lc
        .fail("late error", FailureClass::External)
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("illegal tool call transition"),
        "expected guard error, got: {msg}"
    );
}

#[tokio::test]
async fn lifecycle_idempotent_start_running() {
    let db = test_db("tc-lc-4").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-4".into(),
        "test-session-4".into(),
        "did:test:test".to_string(),
        "tool-call-4".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();
    lc.start_running().await.unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, "test-session-4").await;
    assert_eq!(
        snapshots.len(),
        1,
        "exactly one row should exist after duplicate start_running"
    );
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("running"),
        "state should still be running"
    );
}

#[tokio::test]
async fn lifecycle_load_returns_persisted_state() {
    let db = test_db("tc-lc-5").await;

    {
        let mut lc = ToolCallLifecycle::new(
            db.node.clone(),
            "request-5".into(),
            "test-session-5".into(),
            "did:test:test".to_string(),
            "tool-call-5".into(),
            0,
            "test_tool".into(),
            r#"{}"#.into(),
            test_deadline(),
        );
        lc.start_running().await.unwrap();
        lc.fail("oops", FailureClass::Transport).await.unwrap();
    }

    let loaded = ToolCallLifecycle::load(db.node.clone(), "test-session-5", "tool-call-5")
        .await
        .unwrap()
        .expect("row should exist after start_running + fail");

    drop(loaded);

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, "test-session-5").await;
    assert_eq!(snapshots.len(), 1, "exactly one row");
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("failed"),
        "persisted lifecycle_state should be failed"
    );
    assert_eq!(
        snapshots[0].tool_failure_class.as_deref(),
        Some("transport"),
        "persisted tool_failure_class should be transport"
    );
}

#[tokio::test]
async fn lifecycle_load_preserves_deadline_for_terminal_update() {
    let db = test_db("tc-lc-6").await;
    let deadline = chrono::DateTime::parse_from_rfc3339("2026-05-08T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    {
        let mut lc = ToolCallLifecycle::new(
            db.node.clone(),
            "request-6".into(),
            "test-session-6".into(),
            "did:test:test".to_string(),
            "tool-call-6".into(),
            0,
            "test_tool".into(),
            r#"{}"#.into(),
            deadline,
        );
        lc.start_running().await.unwrap();
    }

    let mut loaded = ToolCallLifecycle::load(db.node.clone(), "test-session-6", "tool-call-6")
        .await
        .unwrap()
        .expect("row should exist after start_running");
    loaded.timeout().await.unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, "test-session-6").await;
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("timedOut"),
        "loaded lifecycle should be able to terminalize as timedOut"
    );
    let observed_deadline =
        chrono::DateTime::parse_from_rfc3339(snapshots[0].deadline_at.as_deref().unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc);
    assert_eq!(
        observed_deadline, deadline,
        "deadline_at should survive load and terminal update"
    );
    assert_eq!(
        snapshots[0].cancel_cause.as_deref(),
        Some("deadline"),
        "timeout() should persist cancel_cause=deadline"
    );
    assert_exact_omission_fact_bound(&db.node, &snapshots[0], "running", "timedOut", "timedOut")
        .await;
}

#[tokio::test]
async fn lifecycle_cancel_during_run_persists_cancel_cause() {
    let db = test_db("tc-lc-cancel-cause-run").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-cancel-run".into(),
        "test-session-cancel-run".into(),
        "did:test:test".to_string(),
        "tool-call-cancel-run".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();
    lc.cancel_during_run(CancelCause::UserCancelled)
        .await
        .unwrap();

    let snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "test-session-cancel-run").await;
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(snapshots[0].cancel_cause.as_deref(), Some("userCancelled"));
    assert_exact_omission_fact_bound(&db.node, &snapshots[0], "running", "cancelled", "cancelled")
        .await;
}

#[tokio::test]
async fn lifecycle_load_with_null_cancel_cause_can_persist_cancel_cause() {
    let db = test_db("tc-lc-cancel-cause-load").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-cancel-load".into(),
        "test-session-cancel-load".into(),
        "did:test:test".to_string(),
        "tool-call-cancel-load".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();
    let snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "test-session-cancel-load").await;
    assert_eq!(snapshots[0].cancel_cause, None);

    let mut loaded = ToolCallLifecycle::load(
        db.node.clone(),
        "test-session-cancel-load",
        "tool-call-cancel-load",
    )
    .await
    .unwrap()
    .expect("tool call lifecycle should load");
    loaded
        .cancel_during_run(CancelCause::Interrupted)
        .await
        .unwrap();

    let snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "test-session-cancel-load").await;
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(snapshots[0].cancel_cause.as_deref(), Some("interrupted"));
    assert_exact_omission_fact_bound(&db.node, &snapshots[0], "running", "cancelled", "cancelled")
        .await;
}

#[tokio::test]
async fn lifecycle_cancel_before_dispatch_persists_cancel_cause() {
    let db = test_db("tc-lc-cancel-cause-pending").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-cancel-pending".into(),
        "test-session-cancel-pending".into(),
        "did:test:test".to_string(),
        "tool-call-cancel-pending".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
        test_deadline(),
    );

    lc.cancel_before_dispatch(CancelCause::Interrupted)
        .await
        .unwrap();

    let snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "test-session-cancel-pending").await;
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(snapshots[0].cancel_cause.as_deref(), Some("interrupted"));
    assert_exact_omission_fact_bound(&db.node, &snapshots[0], "pending", "cancelled", "cancelled")
        .await;
}

pub(super) fn generated_tool_execution_cases_cover_preflight_and_retry_contracts() {
    let unreachable =
        lean_tool_preflight_case("preflight_unreachable_valid_blocks_serviceUnavailable");
    assert_eq!(unreachable.decision, "block");
    assert_eq!(
        unreachable.failure_class.as_deref(),
        Some("serviceUnavailable")
    );

    let invalid = lean_tool_preflight_case("preflight_healthy_invalid_blocks_argumentInvalid");
    assert_eq!(invalid.decision, "block");
    assert_eq!(invalid.failure_class.as_deref(), Some("argumentInvalid"));

    for name in [
        "preflight_healthy_valid_dispatch",
        "preflight_stale_valid_dispatch",
    ] {
        let case = lean_tool_preflight_case(name);
        assert_eq!(case.decision, "dispatch", "{name}");
        assert_eq!(case.failure_class, None, "{name}");
    }

    let safe_read = lean_tool_retry_case("retry_mcpListTools_unknown_transport_retrySafeRead");
    assert_eq!(safe_read.disposition, "retrySafeRead");

    let idempotent =
        lean_tool_retry_case("retry_mcpCall_idempotent_transport_retryIdempotentToolCall");
    assert_eq!(idempotent.disposition, "retryIdempotentToolCall");

    for name in [
        "retry_mcpCall_unknown_transport_doNotRetry",
        "retry_mcpCall_nonIdempotent_transport_doNotRetry",
        "retry_nativeCommand_idempotent_transport_doNotRetry",
    ] {
        let case = lean_tool_retry_case(name);
        assert_eq!(case.disposition, "doNotRetry", "{name}");
    }
}

/// Issue #1002 defect 1: `timeout()` must CAS on `running` like every sibling
/// terminal transition (`complete`, `fail`, `cancel_during_run`,
/// `bridge_complete`/`bridge_failure`, `recover_tool_call_row`).
///
/// The documented race, driven through the real writers: a native tool is
/// running past its deadline while its parent request is interrupted. The
/// periodic `reconcile_terminal_parent_owned_tools` sweep terminalizes the row
/// first (`cancelled` / cause `interrupted`), and only then does the deadline
/// wrapper's `timeout()` fire on the stale in-memory handle. Lean's
/// `ToolExecution.Transition.timeout` requires `pre.state = .running`, and
/// terminal irreversibility requires state AND recorded cause to survive — so
/// the straggler must lose the compare and adopt the durable terminal instead
/// of overwriting it with `timedOut`.
#[tokio::test]
async fn timeout_adopts_terminal_written_by_terminal_parent_sweep() {
    let db = test_db("tc-timeout-cas").await;

    let request_id = "timeout-cas-request";
    let session_id = "timeout-cas-session";
    let created_at = chrono::Utc::now().to_rfc3339();
    // Parent request already interrupted while the tool ran past its deadline.
    crate::support::create_request(&db.node, request_id, session_id, "interrupted", &created_at)
        .await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        request_id.into(),
        session_id.into(),
        crate::support::AGENT_DID.to_string(),
        "timeout-cas-tool-call".into(),
        0,
        "test_tool".into(),
        "{}".into(),
        // Already expired: the timeout writer is enabled the moment the
        // sweep loses interest in the row.
        chrono::Utc::now() - chrono::Duration::seconds(5),
    );
    lc.start_running().await.unwrap();

    // Actor A: the sweep terminalizes the running tool under its terminal
    // parent (interrupted parent => cancelled / cause interrupted).
    let report = ToolCallLifecycle::reconcile_terminal_parent_owned_tools(
        &db.node,
        crate::support::AGENT_DID,
    )
    .await
    .unwrap();
    assert_eq!(
        report.tool_calls_terminalized, 1,
        "sweep should terminalize the running tool under its interrupted parent"
    );

    // Actor B: the straggler timeout path fires on the stale handle. It must
    // lose the running-state compare and adopt the durable terminal.
    let won = lc.timeout().await.unwrap();
    assert!(
        !won,
        "timeout() must report a lost compare when the row was already terminal"
    );

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, session_id).await;
    assert_eq!(snapshots.len(), 1, "exactly one persisted tool-call row");
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("cancelled"),
        "timeout() must not overwrite a terminal another actor already recorded"
    );
    assert_eq!(
        snapshots[0].cancel_cause.as_deref(),
        Some("interrupted"),
        "the recorded cancellation cause must be preserved"
    );
    assert_eq!(
        snapshots[0].tool_failure_class, None,
        "the sweep's cancelled terminal carries no failure class; timeout() must not stamp one"
    );
}

/// Output publication has the same pre-CAS race window as omission
/// publication: another writer may terminalize the running execution before
/// the late completer can bind an AgentToolResult. The loser must adopt the
/// durable cancellation and treat completion as a no-op.
#[tokio::test]
async fn complete_adopts_terminal_written_before_output_publication() {
    let db = test_db("tc-complete-evidence-race").await;
    let session_id = "complete-evidence-race-session";
    let tool_call_id = "complete-evidence-race-tool-call";
    let mut late_complete = ToolCallLifecycle::new(
        db.node.clone(),
        "complete-evidence-race-request".into(),
        session_id.into(),
        "did:test:test".to_string(),
        tool_call_id.into(),
        0,
        "test_tool".into(),
        "{}".into(),
        test_deadline(),
    );
    late_complete.start_running().await.unwrap();

    let mut winner = ToolCallLifecycle::load(db.node.clone(), session_id, tool_call_id)
        .await
        .unwrap()
        .expect("competing writer should reload the running call");
    assert!(
        winner
            .cancel_during_run(CancelCause::UserCancelled)
            .await
            .unwrap(),
        "competing cancellation should win"
    );

    late_complete.complete("late output").await.unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, session_id).await;
    assert_eq!(snapshots.len(), 1, "exactly one persisted tool-call row");
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(
        snapshots[0].cancel_cause.as_deref(),
        Some("userCancelled"),
        "late output publication must preserve the winner's terminal cause"
    );
    assert_eq!(
        snapshots[0].result.as_str(),
        "tool call cancelled",
        "late completion must not replace the winner's terminal payload"
    );
}

/// Held calls can move terminal before a competing omission fact is
/// published. The stale held writer must report a lost compare and preserve
/// the exact timeout omission already bound by the winner.
#[tokio::test]
async fn cancel_while_held_adopts_terminal_written_before_omission_publication() {
    let db = test_db("tc-held-evidence-race").await;
    let session_id = "held-evidence-race-session";
    let tool_call_id = "held-evidence-race-tool-call";
    let mut late_cancel = ToolCallLifecycle::new(
        db.node.clone(),
        "held-evidence-race-request".into(),
        session_id.into(),
        "did:test:test".to_string(),
        tool_call_id.into(),
        0,
        "test_tool".into(),
        "{}".into(),
        test_deadline(),
    );
    late_cancel.hold_for_approval().await.unwrap();

    let mut winner = ToolCallLifecycle::load(db.node.clone(), session_id, tool_call_id)
        .await
        .unwrap()
        .expect("competing writer should reload the held call");
    assert!(
        winner.timeout_while_held().await.unwrap(),
        "competing held timeout should win"
    );

    let won = late_cancel
        .cancel_while_held(CancelCause::UserCancelled)
        .await
        .unwrap();
    assert!(
        !won,
        "held cancellation must report a lost compare when timeout is already durable"
    );

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, session_id).await;
    assert_eq!(snapshots.len(), 1, "exactly one persisted tool-call row");
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("timedOut"));
    assert_eq!(snapshots[0].cancel_cause.as_deref(), Some("deadline"));
    assert!(
        snapshots[0].omission_doc_id.is_some(),
        "the winning held timeout omission must remain exactly bound"
    );
}
