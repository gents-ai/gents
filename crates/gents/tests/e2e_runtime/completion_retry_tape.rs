use std::sync::Arc;
use std::time::{Duration, Instant};

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{
    build_run_timeline, AgentIdentity, Gents, RunTimeline, RunTimelineRows,
    TimelineInferenceCallRow, TimelineRequestRow, ToolCeiling,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::Value;

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{
    create_runtime_request, create_runtime_request_with_execution_origin,
    wait_for_request_lifecycle_state, wait_for_response_doc_id, wait_for_runtime_ready,
    BootedAgent,
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
        let terminal_state = wait_for_request_terminal_state(db.node.as_ref(), doc_id).await;
        if terminal_state != RequestLifecycleState::Completed {
            let snapshot = fetch_request_snapshot(db.node.as_ref(), doc_id).await;
            let calls = fetch_inference_calls(db.node.as_ref(), request_id).await;
            let all_calls = fetch_call_diagnostics(db.node.as_ref(), request_id).await;
            let response = fetch_response_diagnostic(db.node.as_ref(), request_id).await;
            let request_counts = request_doc_ids
                .iter()
                .map(|(_, request_id, marker)| {
                    (request_id.as_str(), backend.observed_requests(marker))
                })
                .collect::<Vec<_>>();
            panic!(
                "request {request_id} unexpectedly reached {terminal_state}; \
                 failure_reason={:?}; inference_calls={calls:?}; \
                 all_calls={all_calls:?}; response={response:?}; \
                 backend_request_counts={request_counts:?}",
                snapshot.failure_reason
            );
        }

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
        RequestLifecycleState::Completed,
    )
    .await;
    assert_eq!(failed_count, 0, "all clustered restart requests recover");

    agent.shutdown().await;
}

#[tokio::test]
async fn retry_backoff_cannot_renew_an_expired_execution_lease() {
    let marker = "backoff-vs-liveness";
    let backend = MockStreamingBackend::start_with_plans(
        RETRY_MODEL,
        vec![StreamPlan::new(
            marker,
            vec![
                StreamResponse::service_unavailable(
                    "HTTP status 503 forcing a backoff longer than the liveness timeout",
                ),
                StreamResponse::completes(marker, ["retry must not execute after lease expiry"]),
            ],
        )],
    )
    .unwrap();
    let db = test_db("completion-retry-backoff-liveness").await;
    let agent = boot_retry_agent_with_liveness(
        &db,
        "completion-retry-backoff-liveness",
        backend.endpoint(),
        1,
        60,
        Some(30),
    )
    .await;

    let request_id = "req-backoff-vs-liveness";
    let doc_id = create_runtime_request_with_execution_origin(
        db.node.as_ref(),
        &agent.agent_did,
        RETRY_BEHAVIOR_ID,
        request_id,
        "session-backoff-vs-liveness",
        "scheduled",
        &format!("recover across a long backoff {marker}"),
    )
    .await;
    // Allow setup and the first real HTTP failure to finish before expiring
    // the lease. Scheduled retry pacing leaves a backoff window; it must not
    // count as semantic progress or grant the next provider call ownership.
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let calls = fetch_inference_calls(db.node.as_ref(), request_id).await;
            if call_states(&calls) == vec!["failed"] {
                break;
            }
            assert!(
                calls.len() <= 1,
                "retry started before the first failed-call observation: {calls:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first provider failure must be durably observed");
    assert_eq!(backend.observed_requests(marker), 1);
    let escaped_doc_id = escape_graphql_string(&doc_id);
    let tuple = db
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }} ) {{
        lifecycle_state execution_generation execution_lease_expires_at execution_progress_seq
    }} }}"#
        ))
        .await;
    assert!(!tuple.has_errors(), "{:?}", tuple.errors);
    let observed = &tuple.data.as_ref().unwrap()["AgentRequest"][0];
    assert_eq!(observed["lifecycle_state"], "processing");
    let generation = escape_graphql_string(observed["execution_generation"].as_str().unwrap());
    let expiry = escape_graphql_string(observed["execution_lease_expires_at"].as_str().unwrap());
    let progress = observed["execution_progress_seq"].as_i64().unwrap();
    assert_eq!(
        progress, 0,
        "failed provider dispatch and backoff are not durable output progress"
    );
    let expired = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let mutation = format!(
        r#"mutation {{ update_AgentRequest(
        filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }}, lifecycle_state: {{ _eq: "processing" }},
            execution_generation: {{ _eq: "{generation}" }}, execution_lease_expires_at: {{ _eq: "{expiry}" }},
            execution_progress_seq: {{ _eq: {progress} }} }},
        input: {{ execution_lease_expires_at: "{}" }}
    ) {{ _docID }} }}"#,
        escape_graphql_string(&expired)
    );
    let expired = db.node.execute(&mutation).await;
    assert!(!expired.has_errors(), "{:?}", expired.errors);
    assert!(
        expired
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentRequest"))
            .is_some_and(gents::graphql::response_has_documents),
        "observed execution lease must be expired by the fixture"
    );

    wait_for_request_lifecycle_state(db.node.as_ref(), &doc_id, "failed").await;
    let escaped_request_id = escape_graphql_string(request_id);
    let rows = db.node.execute(&format!(r#"{{
        AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}) {{ lifecycle_state execution_progress_seq }}
        AgentResponse(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}) {{ status content }}
    }}"#)).await;
    assert!(!rows.has_errors(), "{:?}", rows.errors);
    let rows = rows.data.as_ref().unwrap();
    assert_eq!(rows["AgentRequest"].as_array().unwrap().len(), 1);
    assert_eq!(rows["AgentResponse"].as_array().unwrap().len(), 1);
    assert_eq!(
        rows["AgentRequest"][0]["execution_progress_seq"].as_i64(),
        Some(progress)
    );
    assert_eq!(rows["AgentResponse"][0]["status"], "error");
    assert!(!rows["AgentResponse"][0]["content"]
        .as_str()
        .unwrap_or_default()
        .contains("retry must not execute after lease expiry"));
    let calls = fetch_inference_calls(db.node.as_ref(), request_id).await;
    assert_eq!(call_states(&calls), vec!["failed"]);
    let timeline = build_timeline(db.node.as_ref(), request_id).await;
    assert!(!timeline.request.retry_summary.recovered);
    let repeated = gents::RequestLifecycle::recover_all(db.node.as_ref(), &agent.agent_did)
        .await
        .unwrap();
    assert_eq!(repeated.requests_recovered, 0);
    assert_eq!(repeated.responses_recovered, 0);
    agent.shutdown().await;
    assert_eq!(
        backend.observed_requests(marker),
        1,
        "expired execution must not dispatch the planned successful retry"
    );
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
    let request_doc_id = create_runtime_request_with_execution_origin(
        db.node.as_ref(),
        &agent_did,
        RETRY_BEHAVIOR_ID,
        request_id,
        "session-deterministic-400",
        "scheduled",
        &format!("trigger deterministic parse retry {marker}"),
    )
    .await;

    let agent = spawn_agent(db.node.as_ref(), agent, agent_did).await;
    let terminal_state = wait_for_request_terminal_state(db.node.as_ref(), &request_doc_id).await;

    let calls = fetch_inference_calls(db.node.as_ref(), request_id).await;
    let response_doc_id = wait_for_response_doc_id(db.node.as_ref(), request_id).await;
    let error_message = fetch_response_error_message(db.node.as_ref(), &response_doc_id).await;
    assert_eq!(
        terminal_state,
        RequestLifecycleState::Completed,
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
    boot_retry_agent_with_liveness(
        db,
        test_name,
        endpoint,
        max_concurrent,
        deadline_duration_secs,
        None,
    )
    .await
}

async fn boot_retry_agent_with_liveness(
    db: &crate::support::TestDb,
    test_name: &str,
    endpoint: &str,
    max_concurrent: i64,
    deadline_duration_secs: u64,
    stream_liveness_timeout_secs: Option<u64>,
) -> BootedAgent {
    let agent = build_retry_agent_with_liveness(
        db,
        test_name,
        endpoint,
        max_concurrent,
        deadline_duration_secs,
        stream_liveness_timeout_secs,
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
) -> Gents {
    build_retry_agent_with_liveness(
        db,
        test_name,
        endpoint,
        max_concurrent,
        deadline_duration_secs,
        None,
    )
    .await
}

async fn build_retry_agent_with_liveness(
    db: &crate::support::TestDb,
    test_name: &str,
    endpoint: &str,
    max_concurrent: i64,
    deadline_duration_secs: u64,
    stream_liveness_timeout_secs: Option<u64>,
) -> Gents {
    upsert_retry_backend(db.node.as_ref(), endpoint, max_concurrent).await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity(test_name));
    let mut behavior = Gents::builder()
        .node(db.node.clone())
        .identity(identity)
        .default_behavior_id(RETRY_BEHAVIOR_ID)
        .tool_ceiling(ToolCeiling::meta_only())
        .behavior(RETRY_BEHAVIOR_ID)
        .backend_id(RETRY_BACKEND_ID)
        .model_name(RETRY_MODEL)
        .stream_batch_ms(0)
        .deadline_duration_secs(deadline_duration_secs);
    if let Some(secs) = stream_liveness_timeout_secs {
        behavior = behavior.stream_liveness_timeout_secs(secs);
    }
    behavior
        .done()
        .build()
        .await
        .expect("build completion retry tape agent")
}

async fn spawn_agent(node: &EmbeddedNode, agent: Gents, agent_did: String) -> BootedAgent {
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

async fn wait_for_request_terminal_state(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> RequestLifecycleState {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let snapshot = fetch_request_snapshot(node, request_doc_id).await;
        if snapshot.lifecycle_state.is_terminal() {
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

async fn fetch_response_diagnostic(node: &EmbeddedNode, request_id: &str) -> Option<Value> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentResponse(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                status
                error_message
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "fetch response diagnostic failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
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

async fn fetch_call_diagnostics(node: &EmbeddedNode, request_id: &str) -> Vec<Value> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ call_seq: ASC }}
            ) {{
                call_id
                call_seq
                attempt
                call_kind
                call_state
                failure_reason
                queued_at
                started_at
                ended_at
                backend_id
                controller_generation
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "fetch call diagnostics failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
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
    expected_state: RequestLifecycleState,
) -> usize {
    let mut count = 0;
    for request_id in request_ids {
        let row = fetch_timeline_request(node, request_id).await;
        if row.lifecycle_state != Some(expected_state) {
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
