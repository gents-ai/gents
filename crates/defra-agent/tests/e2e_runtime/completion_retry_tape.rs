use std::sync::Arc;
use std::time::{Duration, Instant};

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    build_run_timeline, AgentIdentity, DefraAgent, RunTimeline, RunTimelineRows,
    TimelineInferenceCallRow, TimelineRequestRow, ToolCeiling,
};
use serde_json::Value;

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{
    create_runtime_request, wait_for_request_lifecycle_state, wait_for_response_doc_id,
    wait_for_runtime_ready, BootedAgent,
};
use crate::support::snapshots::{fetch_request_snapshot, fetch_response_snapshot};
use crate::support::streaming_backend::{MockStreamingBackend, StreamPlan, StreamResponse};
use crate::support::{first_row, test_db};

const RETRY_MODEL: &str = "retry-tape-model";
const RETRY_BACKEND_ID: &str = "retry-tape-backend";
const RETRY_BEHAVIOR_ID: &str = "retry-tape";
const PROD_PARSE_400_BODY: &str = r#"{"object":"error","message":"BadRequestError: Error in processing prompt inputs: Expecting value: line 1 column 28 (char 27)","type":"BadRequestError","code":400}"#;

#[tokio::test]
async fn backend_restart_cluster_recovers() {
    let plans = (0..3)
        .map(|index| {
            let marker = format!("cluster-restart-{index}");
            StreamPlan::new(
                marker.clone(),
                vec![
                    StreamResponse::service_unavailable(format!(
                        "HTTP status 503 while backend restart is in progress for {marker}"
                    )),
                    StreamResponse::completes(marker, ["recovered after restart"]),
                ],
            )
        })
        .collect::<Vec<_>>();
    let backend = MockStreamingBackend::start_with_plans(RETRY_MODEL, plans).unwrap();
    let db = test_db("completion-retry-cluster").await;
    let agent = boot_retry_agent(&db, "completion-retry-cluster", backend.endpoint(), 3, 30).await;

    let mut request_doc_ids = Vec::new();
    for index in 0..3 {
        let request_id = format!("req-cluster-restart-{index}");
        let session_id = format!("session-cluster-restart-{index}");
        let marker = format!("cluster-restart-{index}");
        let doc_id = create_runtime_request(
            db.node.as_ref(),
            &agent.agent_did,
            RETRY_BEHAVIOR_ID,
            &request_id,
            &session_id,
            &format!("please recover {marker}"),
        )
        .await;
        request_doc_ids.push((doc_id, request_id, marker));
    }

    for (doc_id, request_id, marker) in &request_doc_ids {
        wait_for_request_lifecycle_state(db.node.as_ref(), doc_id, "completed").await;

        let calls = fetch_inference_calls(db.node.as_ref(), request_id).await;
        assert_retry_recovered(&calls, 1);
        assert_eq!(
            backend.observed_requests(marker),
            2,
            "request {request_id} should issue one failed provider call and one recovery call"
        );

        let timeline = build_timeline(db.node.as_ref(), request_id).await;
        assert_eq!(timeline.request.retry_summary.retry_count, 1);
        assert!(timeline.request.retry_summary.recovered);
    }

    let failed_count = count_requests_not_in_state(
        db.node.as_ref(),
        &request_doc_ids
            .iter()
            .map(|(_, request_id, _)| request_id.as_str())
            .collect::<Vec<_>>(),
        "completed",
    )
    .await;
    assert_eq!(failed_count, 0, "all clustered restart requests recover");

    agent.shutdown().await;
}

#[tokio::test]
async fn deadline_tight_fails_cleanly() {
    let marker = "deadline-tight";
    let backend = MockStreamingBackend::start_with_plans(
        RETRY_MODEL,
        vec![StreamPlan::new(
            marker,
            vec![
                StreamResponse::service_unavailable("HTTP status 503 before tight deadline"),
                StreamResponse::completes(marker, ["should not be reached"]),
            ],
        )],
    )
    .unwrap();
    let db = test_db("completion-retry-deadline-tight").await;
    let agent = boot_retry_agent(
        &db,
        "completion-retry-deadline-tight",
        backend.endpoint(),
        1,
        1,
    )
    .await;

    let started = Instant::now();
    let request_id = "req-deadline-tight";
    let request_doc_id = create_runtime_request(
        db.node.as_ref(),
        &agent.agent_did,
        RETRY_BEHAVIOR_ID,
        request_id,
        "session-deadline-tight",
        &format!("tight deadline {marker}"),
    )
    .await;

    wait_for_request_lifecycle_state(db.node.as_ref(), &request_doc_id, "failed").await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "deadline fail-fast should not wait for the retry ladder; elapsed={:?}",
        started.elapsed()
    );
    assert_eq!(
        backend.observed_requests(marker),
        1,
        "deadline overshoot should prevent a second provider call"
    );
    let calls = fetch_inference_calls(db.node.as_ref(), request_id).await;
    assert_eq!(call_states(&calls), vec!["failed"]);
    let response_doc_id = wait_for_response_doc_id(db.node.as_ref(), request_id).await;
    let response = fetch_response_snapshot(db.node.as_ref(), &response_doc_id).await;
    assert_eq!(response.status, "error");
    let error_message = fetch_response_error_message(db.node.as_ref(), &response_doc_id).await;
    assert!(
        error_message
            .as_deref()
            .unwrap_or_default()
            .contains("request deadline"),
        "deadline failure should surface request deadline context, got {error_message:?}"
    );

    agent.shutdown().await;
}

#[tokio::test]
async fn interactive_budget_is_quick() {
    let marker = "interactive-budget";
    let backend = MockStreamingBackend::start_with_plans(
        RETRY_MODEL,
        vec![StreamPlan::new(
            marker,
            vec![
                StreamResponse::service_unavailable("HTTP status 503 first interactive failure"),
                StreamResponse::service_unavailable("HTTP status 503 second interactive failure"),
                StreamResponse::completes(marker, ["should not be reached"]),
            ],
        )],
    )
    .unwrap();
    let db = test_db("completion-retry-interactive-budget").await;
    let agent = boot_retry_agent(
        &db,
        "completion-retry-interactive-budget",
        backend.endpoint(),
        1,
        30,
    )
    .await;

    let started = Instant::now();
    let request_id = "req-interactive-budget";
    let request_doc_id = create_runtime_request(
        db.node.as_ref(),
        &agent.agent_did,
        RETRY_BEHAVIOR_ID,
        request_id,
        "session-interactive-budget",
        &format!("interactive request {marker}"),
    )
    .await;

    wait_for_request_lifecycle_state(db.node.as_ref(), &request_doc_id, "failed").await;
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "interactive default should spend one short retry, not the scheduled ladder; elapsed={:?}",
        started.elapsed()
    );
    assert_eq!(
        backend.observed_requests(marker),
        2,
        "interactive default allows exactly one retry"
    );

    let calls = fetch_inference_calls(db.node.as_ref(), request_id).await;
    assert_eq!(call_states(&calls), vec!["failed", "failed"]);
    let timeline = build_timeline(db.node.as_ref(), request_id).await;
    assert_eq!(timeline.request.retry_summary.retry_count, 1);
    assert!(!timeline.request.retry_summary.recovered);

    agent.shutdown().await;
}

#[tokio::test]
async fn deterministic_400_tape() {
    let marker = "deterministic-400";
    let backend = MockStreamingBackend::start_with_plans(
        RETRY_MODEL,
        vec![StreamPlan::new(
            marker,
            vec![
                StreamResponse::bad_request(PROD_PARSE_400_BODY),
                StreamResponse::bad_request(PROD_PARSE_400_BODY),
                StreamResponse::completes(marker, ["repaired deterministic 400"]),
            ],
        )],
    )
    .unwrap();
    let db = test_db("completion-retry-deterministic-400").await;
    let agent = build_retry_agent(
        &db,
        "completion-retry-deterministic-400",
        backend.endpoint(),
        1,
        30,
    )
    .await;
    let agent_did = agent.agent_did().to_string();
    let request_id = "req-deterministic-400";
    let request_doc_id = create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        RETRY_BEHAVIOR_ID,
        request_id,
        "session-deterministic-400",
        &format!("trigger deterministic parse retry {marker}"),
    )
    .await;
    set_request_execution_origin(db.node.as_ref(), &request_doc_id, "scheduled").await;

    let agent = spawn_agent(db.node.as_ref(), agent, agent_did).await;
    let terminal_state = wait_for_request_terminal_state(db.node.as_ref(), &request_doc_id).await;

    let calls = fetch_inference_calls(db.node.as_ref(), request_id).await;
    let response_doc_id = wait_for_response_doc_id(db.node.as_ref(), request_id).await;
    let error_message = fetch_response_error_message(db.node.as_ref(), &response_doc_id).await;
    assert_eq!(
        terminal_state, "completed",
        "deterministic parse-400 should recover; calls={calls:?}; error={error_message:?}"
    );
    assert_retry_recovered(&calls, 2);
    let timeline = build_timeline(db.node.as_ref(), request_id).await;
    assert_eq!(timeline.request.retry_summary.retry_count, 2);
    assert!(timeline.request.retry_summary.recovered);

    agent.shutdown().await;
}

async fn boot_retry_agent(
    db: &crate::support::TestDb,
    test_name: &str,
    endpoint: &str,
    max_concurrent: i64,
    deadline_duration_secs: u64,
) -> BootedAgent {
    let agent = build_retry_agent(
        db,
        test_name,
        endpoint,
        max_concurrent,
        deadline_duration_secs,
    )
    .await;
    let agent_did = agent.agent_did().to_string();
    spawn_agent(db.node.as_ref(), agent, agent_did).await
}

async fn build_retry_agent(
    db: &crate::support::TestDb,
    test_name: &str,
    endpoint: &str,
    max_concurrent: i64,
    deadline_duration_secs: u64,
) -> DefraAgent {
    upsert_retry_backend(db.node.as_ref(), endpoint, max_concurrent).await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity(test_name));
    DefraAgent::builder()
        .node(db.node.clone())
        .identity(identity)
        .default_behavior_id(RETRY_BEHAVIOR_ID)
        .tool_ceiling(ToolCeiling::meta_only())
        .behavior(RETRY_BEHAVIOR_ID)
        .backend_id(RETRY_BACKEND_ID)
        .model_name(RETRY_MODEL)
        .stream_batch_ms(0)
        .deadline_duration_secs(deadline_duration_secs)
        .done()
        .build()
        .await
        .expect("build completion retry tape agent")
}

async fn spawn_agent(node: &EmbeddedNode, agent: DefraAgent, agent_did: String) -> BootedAgent {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(node, &agent_did).await;
    BootedAgent::new(shutdown_tx, handle, agent_did)
}

async fn upsert_retry_backend(node: &EmbeddedNode, endpoint: &str, max_concurrent: i64) {
    let escaped_backend_id = escape_graphql_string(RETRY_BACKEND_ID);
    let escaped_endpoint = escape_graphql_string(endpoint);
    let escaped_model = escape_graphql_string(RETRY_MODEL);
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{escaped_endpoint}",
                    api_key: "",
                    api_key_env_var: "",
                    max_concurrent: {max_concurrent},
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model}"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{escaped_endpoint}",
                    max_concurrent: {max_concurrent},
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model}"],
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert retry tape backend failed: {:?}",
        response.errors
    );
}

async fn set_request_execution_origin(node: &EmbeddedNode, request_doc_id: &str, origin: &str) {
    let doc_id = escape_graphql_string(request_doc_id);
    let origin = escape_graphql_string(origin);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{ execution_origin: "{origin}" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "set request execution origin failed: {:?}",
        response.errors
    );
}

async fn fetch_timeline_request(node: &EmbeddedNode, request_id: &str) -> TimelineRequestRow {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                _docID
                request_id
                agent_did
                behavior_id
                session_id
                content
                metadata
                status
                lifecycle_state
                backend_id
                failure_reason
                created_at
                retry_count
                interrupt_requested_at
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "fetch timeline request failed: {:?}",
        response.errors
    );
    first_row::<TimelineRequestRow>(&response, "AgentRequest")
}

async fn wait_for_request_terminal_state(node: &EmbeddedNode, request_doc_id: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let snapshot = fetch_request_snapshot(node, request_doc_id).await;
        if matches!(
            snapshot.lifecycle_state.as_str(),
            "completed" | "failed" | "interrupted" | "superseded" | "dead"
        ) {
            return snapshot.lifecycle_state;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for request {request_doc_id} terminal state; last={}",
            snapshot.lifecycle_state
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn fetch_response_error_message(
    node: &EmbeddedNode,
    response_doc_id: &str,
) -> Option<String> {
    let response_doc_id = escape_graphql_string(response_doc_id);
    let query = format!(
        r#"{{
            AgentResponse(filter: {{ _docID: {{ _eq: "{response_doc_id}" }} }}, limit: 1) {{
                error_message
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "fetch response error_message failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("error_message"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

async fn fetch_inference_calls(
    node: &EmbeddedNode,
    request_id: &str,
) -> Vec<TimelineInferenceCallRow> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{
                    request_id: {{ _eq: "{request_id}" }},
                    call_kind: {{ _eq: "inference" }}
                }},
                order: {{ call_seq: ASC }}
            ) {{
                _docID
                call_id
                request_id
                call_seq
                attempt
                call_state
                failure_reason
                queued_at
                started_at
                ended_at
                backend_id
                call_kind
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "fetch inference calls failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("decode TimelineInferenceCallRow"))
        .collect()
}

async fn build_timeline(node: &EmbeddedNode, request_id: &str) -> RunTimeline {
    let request = fetch_timeline_request(node, request_id).await;
    let inference_calls = fetch_inference_calls(node, request_id).await;
    build_run_timeline(RunTimelineRows {
        request,
        inference_calls,
        ..Default::default()
    })
}

async fn count_requests_not_in_state(
    node: &EmbeddedNode,
    request_ids: &[&str],
    expected_state: &str,
) -> usize {
    let mut count = 0;
    for request_id in request_ids {
        let row = fetch_timeline_request(node, request_id).await;
        if row.lifecycle_state.as_deref() != Some(expected_state) {
            count += 1;
        }
    }
    count
}

fn assert_retry_recovered(calls: &[TimelineInferenceCallRow], retry_count: usize) {
    assert_eq!(
        calls.len(),
        retry_count + 1,
        "expected {retry_count} failed attempt row(s) followed by a completed row: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .take(retry_count)
            .all(|call| call.call_state == "failed"),
        "retried attempts should be failed rows: {calls:?}"
    );
    assert_eq!(
        calls.last().map(|call| call.call_state.as_str()),
        Some("completed"),
        "last attempt should complete: {calls:?}"
    );
    assert!(
        calls
            .windows(2)
            .all(|pair| pair[0].call_seq < pair[1].call_seq),
        "call_seq should be strictly ascending: {calls:?}"
    );
}

fn call_states(calls: &[TimelineInferenceCallRow]) -> Vec<&str> {
    calls
        .iter()
        .map(|call| call.call_state.as_str())
        .collect::<Vec<_>>()
}
