use std::time::{Duration, Instant};

use gents::store::graphql::escape_graphql_string;
use gents::{
    build_run_timeline, Gents, RunTimeline, RunTimelineRows, TimelineInferenceCallRow,
    TimelineRequestRow, ToolCeiling,
};
use gents_protocol::output::TerminalOutput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::Value;

use crate::support::agent_requests::RequestActors;
use crate::support::builder_runtime::{
    builder_actors, fixture_agent, runtime_builder, spawn_runtime, submit_request,
};
use crate::support::interrupt::{
    wait_for_request_terminal_state, BootedAgent, REQUEST_TERMINAL_PUBLICATION_MARGIN,
};
use crate::support::snapshots::fetch_request_snapshot;
use crate::support::streaming_backend::{
    MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use crate::support::{first_row, test_db};

const RETRY_MODEL: &str = "retry-tape-model";
const RETRY_BACKEND_ID: &str = "retry-tape-backend";
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
    let (actors, agent) = boot_retry_agent(&db, backend.endpoint(), 3, 30).await;

    let mut request_doc_ids = Vec::new();
    for index in 0..3 {
        let request_id = format!("req-cluster-restart-{index}");
        let session_id = format!("session-cluster-restart-{index}");
        let marker = format!("cluster-restart-{index}");
        let doc_id = submit_request(
            &actors,
            &request_id,
            &session_id,
            "interactive",
            &format!("please recover {marker}"),
        )
        .await;
        request_doc_ids.push((doc_id, request_id, marker));
    }

    for (doc_id, request_id, marker) in &request_doc_ids {
        let terminal_state =
            wait_for_request_terminal_state(&actors, doc_id, Duration::from_secs(60)).await;
        if terminal_state != RequestLifecycleState::Completed {
            let snapshot = fetch_request_snapshot(&actors, doc_id).await;
            let calls = fetch_inference_calls(&actors, request_id).await;
            let all_calls = fetch_call_diagnostics(&actors, request_id).await;
            let request_counts = request_doc_ids
                .iter()
                .map(|(_, request_id, marker)| {
                    (request_id.as_str(), backend.observed_requests(marker))
                })
                .collect::<Vec<_>>();
            panic!(
                "request {request_id} unexpectedly reached {terminal_state}; \
                 failure_reason={:?}; inference_calls={calls:?}; \
                 all_calls={all_calls:?}; \
                 backend_request_counts={request_counts:?}",
                snapshot.failure_reason
            );
        }

        let calls = fetch_inference_calls(&actors, request_id).await;
        assert_retry_recovered(&calls, 1);
        assert_eq!(
            backend.observed_requests(marker),
            2,
            "request {request_id} should issue one failed provider call and one recovery call"
        );

        let timeline = build_timeline(&actors, request_id).await;
        assert_eq!(timeline.request.retry_summary.retry_count, 1);
        assert!(timeline.request.retry_summary.recovered);
    }

    let failed_count = count_requests_not_in_state(
        &actors,
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
    let deadline_duration_secs = 60u64;
    let (actors, runtime) = build_retry_agent_with_liveness(
        &db,
        backend.endpoint(),
        1,
        deadline_duration_secs,
        Some(30),
    )
    .await;
    let identities = runtime
        .agents()
        .iter()
        .map(|agent| agent.agent_identity.clone())
        .collect::<Vec<_>>();
    let agent = spawn_runtime(&db, runtime).await;

    let request_id = "req-backoff-vs-liveness";
    let doc_id = submit_request(
        &actors,
        request_id,
        "session-backoff-vs-liveness",
        "scheduled",
        &format!("recover across a long backoff {marker}"),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let calls = fetch_inference_calls(&actors, request_id).await;
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
    let tuple = actors
        .query(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }} ) {{
        lifecycle_state execution_generation execution_lease_expires_at
    }} }}"#
        ))
        .await;
    assert!(!tuple.has_errors(), "{:?}", tuple.errors);
    let observed = &tuple.data.as_ref().unwrap()["AgentRequest"][0];
    assert_eq!(observed["lifecycle_state"], "processing");
    let generation = escape_graphql_string(observed["execution_generation"].as_str().unwrap());
    let expiry = escape_graphql_string(observed["execution_lease_expires_at"].as_str().unwrap());
    let expired = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let mutation = format!(
        r#"mutation {{ update_AgentRequest(
        filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }}, lifecycle_state: {{ _eq: "processing" }},
            execution_generation: {{ _eq: "{generation}" }}, execution_lease_expires_at: {{ _eq: "{expiry}" }} }},
        input: {{ execution_lease_expires_at: "{}" }}
    ) {{ _docID }} }}"#,
        escape_graphql_string(&expired)
    );
    // The working Agent owns its execution lease; the fixture stands in for
    // the lease running out by writing the elapsed expiry as that Agent.
    let expired = actors
        .agent_access
        .write("test.retry_tape.expire_lease", &mutation)
        .await
        .expect("expire the observed execution lease as the working Agent");
    assert!(
        expired
            .pointer("/data/update_AgentRequest")
            .is_some_and(gents::store::graphql::response_has_documents),
        "observed execution lease must be expired by the fixture"
    );

    let terminal_state = wait_for_request_terminal_state(
        &actors,
        &doc_id,
        Duration::from_secs(deadline_duration_secs) + REQUEST_TERMINAL_PUBLICATION_MARGIN,
    )
    .await;
    assert_eq!(
        terminal_state,
        RequestLifecycleState::Failed,
        "an expired execution lease must fail the request, not run the planned retry"
    );
    let rows = actors
        .query(&format!(
            r#"{{
        AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{
            lifecycle_state failure_reason terminal_output
        }}
        AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{escaped_doc_id}" }} }}) {{ _docID source payload }}
    }}"#
        ))
        .await;
    assert!(!rows.has_errors(), "{:?}", rows.errors);
    let rows = rows.data.as_ref().unwrap();
    assert_eq!(rows["AgentRequest"].as_array().unwrap().len(), 1);
    assert_eq!(rows["AgentRequest"][0]["lifecycle_state"], "failed");
    assert_eq!(
        serde_json::from_value::<TerminalOutput>(
            rows["AgentRequest"][0]["terminal_output"].clone()
        )
        .unwrap(),
        TerminalOutput::NoMessage,
        "a provider failure before accepted publication has no terminal message"
    );
    assert!(rows["AgentRequest"][0]["failure_reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("execution lease expired")));
    for segment in rows["AgentOutputSegment"].as_array().unwrap() {
        let source: gents_protocol::output::OutputSource =
            serde_json::from_value(segment["source"].clone()).unwrap();
        if matches!(
            source,
            gents_protocol::output::OutputSource::ProviderTurn { .. }
        ) {
            assert!(
                segment["payload"].is_null() || segment["payload"].as_str() == Some(""),
                "terminal-only provider records must contain no payload: {segment:?}"
            );
        }
    }
    let calls = fetch_inference_calls(&actors, request_id).await;
    assert_eq!(call_states(&calls), vec!["failed"]);
    let timeline = build_timeline(&actors, request_id).await;
    assert!(!timeline.request.retry_summary.recovered);
    let repeated = gents::RequestLifecycle::recover_all_with_identities(
        &db.node,
        &agent.node_did,
        &identities,
    )
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
    let deadline_duration_secs = 1u64;
    let (actors, agent) =
        boot_retry_agent(&db, backend.endpoint(), 1, deadline_duration_secs).await;

    let request_id = "req-deadline-tight";
    let request_doc_id = submit_request(
        &actors,
        request_id,
        "session-deadline-tight",
        "interactive",
        &format!("tight deadline {marker}"),
    )
    .await;

    let terminal_state = wait_for_request_terminal_state(
        &actors,
        &request_doc_id,
        Duration::from_secs(deadline_duration_secs) + REQUEST_TERMINAL_PUBLICATION_MARGIN,
    )
    .await;
    assert_eq!(
        terminal_state,
        RequestLifecycleState::Failed,
        "a tight deadline must fail the request, not complete it"
    );
    let observed_requests = backend.observed_requests(marker);
    assert!(
        observed_requests <= 1,
        "deadline overshoot should prevent a second provider call"
    );
    let calls = fetch_inference_calls(&actors, request_id).await;
    assert!(calls.len() <= 1, "deadline must prevent a retry: {calls:?}");
    if observed_requests == 1 {
        assert_eq!(
            calls.len(),
            1,
            "a dispatched provider request must retain its durable inference call"
        );
    }
    assert!(
        call_states(&calls).iter().all(|state| *state == "failed"),
        "deadline must fail every started inference call: {calls:?}"
    );
    let request = fetch_terminal_request(&actors, &request_doc_id).await;
    assert_eq!(request.lifecycle_state, RequestLifecycleState::Failed);
    assert_eq!(request.terminal_output, TerminalOutput::NoMessage);
    assert!(
        request.failure_reason.contains("request deadline"),
        "deadline failure should surface request deadline context, got {:?}",
        request.failure_reason
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
    let deadline_duration_secs = 30u64;
    let (actors, agent) =
        boot_retry_agent(&db, backend.endpoint(), 1, deadline_duration_secs).await;

    let started = Instant::now();
    let request_id = "req-interactive-budget";
    let request_doc_id = submit_request(
        &actors,
        request_id,
        "session-interactive-budget",
        "interactive",
        &format!("interactive request {marker}"),
    )
    .await;

    let terminal_state = wait_for_request_terminal_state(
        &actors,
        &request_doc_id,
        Duration::from_secs(deadline_duration_secs) + REQUEST_TERMINAL_PUBLICATION_MARGIN,
    )
    .await;
    assert_eq!(
        terminal_state,
        RequestLifecycleState::Failed,
        "the interactive default should spend its one retry and fail, not complete"
    );
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

    let calls = fetch_inference_calls(&actors, request_id).await;
    assert_eq!(call_states(&calls), vec!["failed", "failed"]);
    let timeline = build_timeline(&actors, request_id).await;
    assert_eq!(timeline.request.retry_summary.retry_count, 1);
    assert!(!timeline.request.retry_summary.recovered);

    agent.shutdown().await;
}

async fn start_stall_tape(
    marker: &str,
    responses: Vec<StreamResponse>,
    execution_origin: &str,
) -> (
    MockStreamingBackend,
    crate::support::TestDb,
    RequestActors,
    BootedAgent,
    String,
    Instant,
) {
    let backend = MockStreamingBackend::start_with_plans(
        RETRY_MODEL,
        vec![StreamPlan::new(marker, responses)],
    )
    .unwrap();
    let test_name = format!("completion-retry-{marker}");
    let db = test_db(&test_name).await;
    let actors = builder_actors(&db, RETRY_BACKEND_ID, backend.endpoint(), RETRY_MODEL, 1).await;
    let runtime = fixture_agent(
        runtime_builder(&db, &actors).tool_ceiling(ToolCeiling::meta_only()),
        &actors,
        "retry-tape",
    )
    .backend_id(RETRY_BACKEND_ID)
    .model_name(RETRY_MODEL)
    .stream_batch_ms(0)
    .deadline_duration_secs(60)
    .provider_idle_timeout_secs(1)
    .done()
    .build()
    .await
    .expect("build stall tape agent");
    let agent = spawn_runtime(&db, runtime).await;
    let started = Instant::now();
    let doc_id = submit_request(
        &actors,
        &format!("req-{marker}"),
        &format!("session-{marker}"),
        execution_origin,
        &format!("stall the provider {marker}"),
    )
    .await;
    (backend, db, actors, agent, doc_id, started)
}

async fn assert_stall_recovered(marker: &str, responses: Vec<StreamResponse>) {
    let (backend, _db, actors, agent, doc_id, started) =
        start_stall_tape(marker, responses, "interactive").await;
    wait_for_request_terminal_state(&actors, &doc_id, Duration::from_secs(60)).await;
    let elapsed = started.elapsed();
    let request = fetch_terminal_request(&actors, &doc_id).await;
    let calls = fetch_inference_calls(&actors, &format!("req-{marker}")).await;
    assert_eq!(
        request.lifecycle_state,
        RequestLifecycleState::Completed,
        "failure_reason={:?}; calls={calls:?}",
        request.failure_reason
    );
    assert!(matches!(
        request.terminal_output,
        TerminalOutput::Message { .. }
    ));
    assert_eq!(backend.observed_requests(marker), 2);
    assert_retry_recovered(&calls, 1);
    assert_stall_reason(&calls[0]);
    assert!(elapsed < Duration::from_secs(20), "elapsed={elapsed:?}");
    agent.shutdown().await;
}

fn assert_stall_reason(call: &TimelineInferenceCallRow) {
    assert!(
        call.failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("provider stream stalled")),
        "stalled call must record the stall, not a generic drop: {call:?}"
    );
}

#[tokio::test]
async fn provider_headers_that_never_arrive_fail_the_attempt_and_retry() {
    let marker = "stall-headers";
    assert_stall_recovered(
        marker,
        vec![
            StreamResponse::HoldHeaders(StreamScript::completes(marker, ["never released"])),
            StreamResponse::completes(marker, ["recovered after header stall"]),
        ],
    )
    .await;
}

#[tokio::test]
async fn provider_silent_after_headers_fails_the_attempt_and_retries() {
    let marker = "stall-first-item";
    assert_stall_recovered(
        marker,
        vec![
            StreamResponse::Stream(StreamScript::paused_before(
                marker,
                vec![StreamChunk::text("never released")],
            )),
            StreamResponse::completes(marker, ["recovered after first-item stall"]),
        ],
    )
    .await;
}

#[tokio::test]
async fn provider_silent_between_chunks_retracts_and_retries() {
    let marker = "stall-between-chunks";
    assert_stall_recovered(
        marker,
        vec![
            StreamResponse::Stream(StreamScript::paused(marker, ["partial before stall"])),
            StreamResponse::completes(marker, ["recovered after inter-chunk stall"]),
        ],
    )
    .await;
}

#[tokio::test]
async fn stalled_call_is_terminal_before_the_retry_backoff_ends() {
    let marker = "stall-releases-permit";
    let (backend, _db, actors, agent, doc_id, _) = start_stall_tape(
        marker,
        vec![
            StreamResponse::Stream(StreamScript::paused_before(
                marker,
                vec![StreamChunk::text("never released")],
            )),
            StreamResponse::completes(marker, ["recovered after backoff"]),
        ],
        "scheduled",
    )
    .await;
    let request_id = format!("req-{marker}");
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let calls = fetch_inference_calls(&actors, &request_id).await;
            if calls
                .first()
                .is_some_and(|call| call.call_state == "failed")
            {
                assert_eq!(
                    backend.observed_requests(marker),
                    1,
                    "the retry must still be backing off: {calls:?}"
                );
                assert_stall_reason(&calls[0]);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("stalled call must reach a terminal state during backoff");
    assert_eq!(
        wait_for_request_terminal_state(&actors, &doc_id, Duration::from_secs(60)).await,
        RequestLifecycleState::Completed
    );
    assert_eq!(backend.observed_requests(marker), 2);
    agent.shutdown().await;
}

#[tokio::test]
async fn repeated_provider_silence_fails_the_request_through_the_retry_owner() {
    let marker = "stall-exhausts-retry";
    let (backend, _db, actors, agent, doc_id, started) = start_stall_tape(
        marker,
        vec![
            StreamResponse::Stream(StreamScript::paused_before(
                marker,
                vec![StreamChunk::text("never released")],
            )),
            StreamResponse::Stream(StreamScript::paused(marker, ["partial before stall"])),
            StreamResponse::completes(marker, ["should not be reached"]),
        ],
        "interactive",
    )
    .await;
    wait_for_request_terminal_state(&actors, &doc_id, Duration::from_secs(60)).await;
    let elapsed = started.elapsed();
    let request = fetch_terminal_request(&actors, &doc_id).await;
    let calls = fetch_inference_calls(&actors, &format!("req-{marker}")).await;
    assert_eq!(request.lifecycle_state, RequestLifecycleState::Failed);
    assert!(
        request.failure_reason.contains("retry budget exhausted"),
        "failure_reason={:?}",
        request.failure_reason
    );
    assert_eq!(backend.observed_requests(marker), 2);
    assert_eq!(call_states(&calls), vec!["failed", "failed"]);
    calls.iter().for_each(assert_stall_reason);
    assert!(elapsed < Duration::from_secs(20), "elapsed={elapsed:?}");
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
    let (actors, agent) = build_retry_agent(&db, backend.endpoint(), 1, 30).await;
    let request_id = "req-deterministic-400";
    let request_doc_id = submit_request(
        &actors,
        request_id,
        "session-deterministic-400",
        "scheduled",
        &format!("trigger deterministic parse retry {marker}"),
    )
    .await;

    let agent = spawn_runtime(&db, agent).await;
    let terminal_state =
        wait_for_request_terminal_state(&actors, &request_doc_id, Duration::from_secs(60)).await;

    let calls = fetch_inference_calls(&actors, request_id).await;
    assert_eq!(
        terminal_state,
        RequestLifecycleState::Completed,
        "deterministic parse-400 should recover; calls={calls:?}"
    );
    let request = fetch_terminal_request(&actors, &request_doc_id).await;
    assert!(matches!(
        request.terminal_output,
        TerminalOutput::Message { .. }
    ));
    assert_retry_recovered(&calls, 2);
    let timeline = build_timeline(&actors, request_id).await;
    assert_eq!(timeline.request.retry_summary.retry_count, 2);
    assert!(timeline.request.retry_summary.recovered);

    agent.shutdown().await;
}

async fn boot_retry_agent(
    db: &crate::support::TestDb,
    endpoint: &str,
    max_concurrent: usize,
    deadline_duration_secs: u64,
) -> (RequestActors, BootedAgent) {
    let (actors, runtime) =
        build_retry_agent_with_liveness(db, endpoint, max_concurrent, deadline_duration_secs, None)
            .await;
    let agent = spawn_runtime(db, runtime).await;
    (actors, agent)
}

async fn build_retry_agent(
    db: &crate::support::TestDb,
    endpoint: &str,
    max_concurrent: usize,
    deadline_duration_secs: u64,
) -> (RequestActors, Gents) {
    build_retry_agent_with_liveness(db, endpoint, max_concurrent, deadline_duration_secs, None)
        .await
}

async fn build_retry_agent_with_liveness(
    db: &crate::support::TestDb,
    endpoint: &str,
    max_concurrent: usize,
    deadline_duration_secs: u64,
    stream_liveness_timeout_secs: Option<u64>,
) -> (RequestActors, Gents) {
    let actors = builder_actors(db, RETRY_BACKEND_ID, endpoint, RETRY_MODEL, max_concurrent).await;
    let mut behavior = fixture_agent(
        runtime_builder(db, &actors).tool_ceiling(ToolCeiling::meta_only()),
        &actors,
        "retry-tape",
    )
    .backend_id(RETRY_BACKEND_ID)
    .model_name(RETRY_MODEL)
    .stream_batch_ms(0)
    .deadline_duration_secs(deadline_duration_secs);
    if let Some(secs) = stream_liveness_timeout_secs {
        behavior = behavior.stream_liveness_timeout_secs(secs);
    }
    let runtime = behavior
        .done()
        .build()
        .await
        .expect("build completion retry tape agent");
    (actors, runtime)
}

async fn fetch_timeline_request(actors: &RequestActors, request_id: &str) -> TimelineRequestRow {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                _docID
                request_id
                node_did
                agent_did
                session_id
                content
                input
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
    let response = actors.query(&query).await;
    assert!(
        !response.has_errors(),
        "fetch timeline request failed: {:?}",
        response.errors
    );
    first_row::<TimelineRequestRow>(&response, "AgentRequest")
}

#[derive(Debug, serde::Deserialize)]
struct TerminalRequest {
    lifecycle_state: RequestLifecycleState,
    failure_reason: String,
    terminal_output: TerminalOutput,
}

async fn fetch_terminal_request(actors: &RequestActors, request_doc_id: &str) -> TerminalRequest {
    let request_doc_id = escape_graphql_string(request_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{request_doc_id}" }} }}, limit: 1) {{
                lifecycle_state
                failure_reason
                terminal_output
            }}
        }}"#
    );
    let response = actors.query(&query).await;
    assert!(
        !response.has_errors(),
        "fetch terminal request failed: {:?}",
        response.errors
    );
    first_row::<TerminalRequest>(&response, "AgentRequest")
}

async fn fetch_inference_calls(
    actors: &RequestActors,
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
    let response = actors.query(&query).await;
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

async fn fetch_call_diagnostics(actors: &RequestActors, request_id: &str) -> Vec<Value> {
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
    let response = actors.query(&query).await;
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

async fn build_timeline(actors: &RequestActors, request_id: &str) -> RunTimeline {
    let request = fetch_timeline_request(actors, request_id).await;
    let inference_calls = fetch_inference_calls(actors, request_id).await;
    build_run_timeline(RunTimelineRows {
        request,
        inference_calls,
        ..Default::default()
    })
}

async fn count_requests_not_in_state(
    actors: &RequestActors,
    request_ids: &[&str],
    expected_state: RequestLifecycleState,
) -> usize {
    let mut count = 0;
    for request_id in request_ids {
        let row = fetch_timeline_request(actors, request_id).await;
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
