use std::time::Duration;

use gents::Gents;
use gents_loop::completion_retry::{CompletionRetryPolicy, CompletionRetryProfileFields};
use gents_loop::execution_origin::ExecutionOrigin;
use gents_observe::{
    observe::run_timeline::build::build_run_timeline, observe::run_timeline::events::RunTimeline,
    observe::run_timeline::rows::RunTimelineRows,
    observe::run_timeline::rows::TimelineInferenceCallRow,
    observe::run_timeline::rows::TimelineRequestRow,
};
use gents_protocol::output::TerminalOutput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_store::graphql::escape_graphql_string;
use gents_store::test_support::{STALL_CEILING, UNREACHED_DEADLINE};
use gents_tools::surface::ToolCeiling;
use serde_json::Value;

use crate::support::agent_requests::RequestActors;
use crate::support::builder_runtime::{
    builder_actors, fixture_agent, runtime_builder, spawn_runtime, submit_request,
};
use crate::support::interrupt::{wait_for_request_terminal_state, BootedAgent};
use crate::support::snapshots::fetch_request_snapshot;
use crate::support::{first_row, trial_store};
use gents_inference::test_support::scripted_backend::{
    ScriptedBackend, StreamChunk, StreamPlan, StreamResponse, StreamScript,
};
use gents_store::test_support::rows_when;

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
    let backend = ScriptedBackend::start_with_plans(RETRY_MODEL, plans).unwrap();
    let db = trial_store("completion-retry-cluster").await;
    let (actors, agent) =
        boot_retry_agent(&db, backend.endpoint(), 3, UNREACHED_DEADLINE.as_secs()).await;

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
        let terminal_state = wait_for_request_terminal_state(&actors, doc_id).await;
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

/// An execution whose lease runs out while it backs off a failed provider call
/// dispatches no retry, and recovery fails the request (`tla/RetryBackoffLease`).
/// The backoff is unreached, so the expiry lands during it however long the
/// fixture takes (`WallClock`), and the expiry write matches only the observed
/// generation, so a renewal between its read and its write cannot make it miss
/// (`DeadlineCas`).
#[tokio::test]
async fn retry_backoff_cannot_renew_an_expired_execution_lease() {
    let marker = "backoff-vs-liveness";
    let backend = ScriptedBackend::start_with_plans(
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
    let db = trial_store("completion-retry-backoff-liveness").await;
    let unreached_backoff_ms = i64::try_from(UNREACHED_DEADLINE.as_millis()).unwrap();
    let (actors, runtime) = build_retry_agent_with_liveness(
        &db,
        backend.endpoint(),
        1,
        2 * UNREACHED_DEADLINE.as_secs(),
        Some(30),
        CompletionRetryProfileFields {
            retry_backoff_ms: Some(vec![unreached_backoff_ms]),
            ..Default::default()
        },
    )
    .await;
    let identities = runtime
        .agents()
        .iter()
        .map(|agent| agent.agent.agent_identity.clone())
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
    let mut changes =
        gents_store::subscription_source::subscribe_collections(&actors.node, ["InferenceCall"])
            .expect("subscribe to provider attempt changes");
    tokio::time::timeout(STALL_CEILING, async {
        loop {
            let calls = fetch_inference_calls(&actors, request_id).await;
            if call_states(&calls) == vec!["failed"] {
                break;
            }
            assert!(
                calls.len() <= 1,
                "retry started before the first failed-call observation: {calls:?}"
            );
            changes
                .recv()
                .await
                .expect("provider attempt subscription closed");
        }
    })
    .await
    .expect("first provider failure must be durably observed");
    assert_eq!(backend.observed_requests(marker), 1);
    let escaped_doc_id = escape_graphql_string(&doc_id);
    let tuple = actors
        .query(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }} ) {{
        lifecycle_state execution_generation
    }} }}"#
        ))
        .await;
    assert!(!tuple.has_errors(), "{:?}", tuple.errors);
    let observed = &tuple.data.as_ref().unwrap()["AgentRequest"][0];
    assert_eq!(observed["lifecycle_state"], "processing");
    let generation = escape_graphql_string(observed["execution_generation"].as_str().unwrap());
    let expired = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let mutation = format!(
        r#"mutation {{ update_AgentRequest(
        filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }}, lifecycle_state: {{ _eq: "processing" }},
            execution_generation: {{ _eq: "{generation}" }} }},
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
            .is_some_and(gents_store::graphql::response_has_documents),
        "observed execution lease must be expired by the fixture"
    );

    let terminal_state = wait_for_request_terminal_state(&actors, &doc_id).await;
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
    let repeated =
        gents_session::request::lifecycle::RequestLifecycle::recover_all_with_identities(
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

/// A request whose deadline passes is failed by its executor without a second
/// provider call, and every inference call it started ends `failed`, though
/// possibly after the request's terminal.
#[tokio::test]
async fn deadline_tight_fails_cleanly() {
    let marker = "deadline-tight";
    let backend = ScriptedBackend::start_with_plans(
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
    let db = trial_store("completion-retry-deadline-tight").await;
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

    let terminal_state = wait_for_request_terminal_state(&actors, &request_doc_id).await;
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
    let calls = wait_for_ended_inference_calls(&actors, request_id).await;
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

/// The interactive budget is the transport ladder `CompletionRetryPolicy::resolve`
/// gives an interactive request (Lean `RuntimeCompletionConfig.resolve`), not
/// wall time: the request deadline is unreached (`WallClockBound`), so the
/// in-loop stop can only be the exhausted ladder, and the scheduled ladder
/// would instead have recovered on the provider's next answer without parking.
#[tokio::test]
async fn interactive_budget_is_quick() {
    let in_loop_attempts = interactive_in_loop_attempts();

    let marker = "interactive-budget";
    let mut responses = (0..in_loop_attempts)
        .map(|index| {
            StreamResponse::service_unavailable(format!(
                "HTTP status 503 interactive failure {index}"
            ))
        })
        .collect::<Vec<_>>();
    responses.push(StreamResponse::completes(
        marker,
        ["answered after the outage"],
    ));
    let backend =
        ScriptedBackend::start_with_plans(RETRY_MODEL, vec![StreamPlan::new(marker, responses)])
            .unwrap();
    let db = trial_store("completion-retry-interactive-budget").await;
    let (actors, agent) =
        boot_retry_agent(&db, backend.endpoint(), 1, UNREACHED_DEADLINE.as_secs()).await;

    let request_id = "req-interactive-budget";
    let request_doc_id = submit_request(
        &actors,
        request_id,
        "session-interactive-budget",
        "interactive",
        &format!("interactive request {marker}"),
    )
    .await;

    let terminal_state = wait_for_request_terminal_state(&actors, &request_doc_id).await;
    assert_eq!(
        terminal_state,
        RequestLifecycleState::Completed,
        "an exhausted interactive retry parks the request for its backend, which then answers"
    );
    assert_eq!(backend.observed_requests(marker), in_loop_attempts + 1);
    let calls = fetch_inference_calls(&actors, request_id).await;
    assert_parked_once_then_answered(&actors, &request_doc_id, &calls, in_loop_attempts).await;
    assert_eq!(
        calls.iter().map(|call| call.call_seq).collect::<Vec<_>>(),
        (1..=i64::try_from(in_loop_attempts + 1).unwrap()).collect::<Vec<_>>(),
        "the resumed execution continues the request's call sequence: {calls:?}"
    );
    let timeline = build_timeline(&actors, request_id).await;
    assert_eq!(
        timeline.request.retry_summary.retry_count,
        u32::try_from(in_loop_attempts).unwrap(),
        "every failed attempt before and after the park is a retry: {:?}",
        timeline.request.retry_summary
    );
    assert!(timeline.request.retry_summary.recovered);

    agent.shutdown().await;
}

/// Provider attempts one interactive execution makes before it parks: the
/// first call and one retry per step of the ladder `CompletionRetryPolicy::resolve`
/// gives the interactive origin. The scheduled ladder must be longer, or a
/// tape could not tell the two apart without a clock.
fn interactive_in_loop_attempts() -> usize {
    let fields = CompletionRetryProfileFields::default();
    let interactive = CompletionRetryPolicy::resolve(&fields, ExecutionOrigin::Interactive);
    let scheduled = CompletionRetryPolicy::resolve(&fields, ExecutionOrigin::Scheduled);
    let in_loop_attempts = interactive.transport_backoff.len() + 1;
    assert!(
        scheduled.transport_backoff.len() >= in_loop_attempts,
        "the tape must distinguish the interactive ladder from the scheduled one: \
         interactive={interactive:?} scheduled={scheduled:?}"
    );
    in_loop_attempts
}

/// The first execution spends `in_loop_attempts` failed provider calls, parks
/// once for its backend, and the resumed execution's one call completes.
async fn assert_parked_once_then_answered(
    actors: &RequestActors,
    request_doc_id: &str,
    calls: &[TimelineInferenceCallRow],
    in_loop_attempts: usize,
) {
    let wait = fetch_backend_wait(actors, request_doc_id).await;
    assert_eq!(wait.attempts, 1, "the request parks once: {wait:?}");
    let parked_at = rfc3339(Some(&wait.wait_started_at));
    let (parked, resumed): (Vec<_>, Vec<_>) = calls
        .iter()
        .partition(|call| rfc3339(call.started_at.as_deref()) < parked_at);
    assert_eq!(
        call_states_of(&parked),
        vec!["failed"; in_loop_attempts],
        "the interactive ladder spends its retries and then parks: {calls:?}"
    );
    assert!(
        parked
            .iter()
            .all(|call| rfc3339(call.ended_at.as_deref()) <= parked_at),
        "every in-loop attempt ends before the request parks: {calls:?}; {wait:?}"
    );
    assert_eq!(
        call_states_of(&resumed),
        vec!["completed"],
        "the parked request resumes once and the backend answers: {calls:?}"
    );
}

#[derive(Debug, serde::Deserialize)]
struct BackendWaitRow {
    attempts: i64,
    wait_started_at: String,
}

async fn fetch_backend_wait(actors: &RequestActors, request_doc_id: &str) -> BackendWaitRow {
    let request_doc_id = escape_graphql_string(request_doc_id);
    let response = actors
        .query(&format!(
            r#"{{ AgentRequestBackendWait(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}, limit: 1) {{
                attempts wait_started_at
            }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "fetch backend wait failed: {:?}",
        response.errors
    );
    first_row::<BackendWaitRow>(&response, "AgentRequestBackendWait")
}

fn rfc3339(value: Option<&str>) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(value.expect("recorded timestamp"))
        .expect("recorded timestamps are RFC 3339")
        .with_timezone(&chrono::Utc)
}

fn call_states_of<'a>(calls: &[&'a TimelineInferenceCallRow]) -> Vec<&'a str> {
    calls.iter().map(|call| call.call_state.as_str()).collect()
}

async fn start_stall_tape(
    marker: &str,
    responses: Vec<StreamResponse>,
    execution_origin: &str,
) -> (
    ScriptedBackend,
    gents_store::test_support::TestStore,
    RequestActors,
    BootedAgent,
    String,
) {
    let backend =
        ScriptedBackend::start_with_plans(RETRY_MODEL, vec![StreamPlan::new(marker, responses)])
            .unwrap();
    let test_name = format!("completion-retry-{marker}");
    let db = trial_store(&test_name).await;
    let actors = builder_actors(&db, RETRY_BACKEND_ID, backend.endpoint(), RETRY_MODEL, 1).await;
    let runtime = fixture_agent(
        runtime_builder(&db, &actors).tool_ceiling(ToolCeiling::meta_only()),
        &actors,
        "retry-tape",
    )
    .backend_id(RETRY_BACKEND_ID)
    .model_name(RETRY_MODEL)
    .stream_batch_ms(0)
    .deadline_duration_secs(UNREACHED_DEADLINE.as_secs())
    .provider_idle_timeout_secs(PROVIDER_IDLE_SECS)
    .done()
    .build()
    .await
    .expect("build stall tape agent");
    let agent = spawn_runtime(&db, runtime).await;
    let doc_id = submit_request(
        &actors,
        &format!("req-{marker}"),
        &format!("session-{marker}"),
        execution_origin,
        &format!("stall the provider {marker}"),
    )
    .await;
    (backend, db, actors, agent, doc_id)
}

async fn assert_stall_recovered(marker: &str, responses: Vec<StreamResponse>) {
    let (backend, _db, actors, agent, doc_id) =
        start_stall_tape(marker, responses, "interactive").await;
    wait_for_request_terminal_state(&actors, &doc_id).await;
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
    agent.shutdown().await;
}

/// The stall tapes' provider idle timeout. Every stalled attempt must be cut
/// by this timer, which its recorded failure reason names, rather than by the
/// stream liveness timeout or the request deadline.
const PROVIDER_IDLE_SECS: u64 = 1;

fn assert_stall_reason(call: &TimelineInferenceCallRow) {
    let idle = format!(
        "provider stream stalled: no transport activity for {:?}",
        Duration::from_secs(PROVIDER_IDLE_SECS)
    );
    assert!(
        call.failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains(&idle)),
        "stalled call must record the provider idle timeout ({idle}), not a generic drop: {call:?}"
    );
}

#[tokio::test]
async fn provider_headers_that_never_arrive_fail_the_attempt_and_retry() {
    let marker = "stall-headers";
    assert_stall_recovered(
        marker,
        vec![
            StreamResponse::completes(marker, ["never released"]).after(marker),
            StreamResponse::completes(marker, ["recovered after header stall"]),
        ],
    )
    .await;
}

#[tokio::test]
async fn stalled_call_is_terminal_before_the_retry_backoff_ends() {
    let marker = "stall-releases-permit";
    let (backend, _db, actors, agent, doc_id) = start_stall_tape(
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
    assert_eq!(
        wait_for_request_terminal_state(&actors, &doc_id).await,
        RequestLifecycleState::Completed
    );
    assert_eq!(backend.observed_requests(marker), 2);
    let calls = fetch_inference_calls(&actors, &format!("req-{marker}")).await;
    assert_retry_recovered(&calls, 1);
    assert_stall_reason(&calls[0]);
    assert!(
        rfc3339(calls[0].ended_at.as_deref()) <= rfc3339(calls[1].started_at.as_deref()),
        "the stalled call must be terminal before its retry starts: {calls:?}"
    );
    agent.shutdown().await;
}

#[tokio::test]
async fn repeated_provider_silence_parks_the_request_until_the_provider_answers() {
    let marker = "stall-exhausts-retry";
    let in_loop_attempts = interactive_in_loop_attempts();
    let mut responses = (0..in_loop_attempts)
        .map(|index| {
            StreamResponse::Stream(if index == 0 {
                StreamScript::paused_before(marker, vec![StreamChunk::text("never released")])
            } else {
                StreamScript::paused(marker, ["partial before stall"])
            })
        })
        .collect::<Vec<_>>();
    responses.push(StreamResponse::completes(
        marker,
        ["answered after the stalls"],
    ));
    let (backend, _db, actors, agent, doc_id) =
        start_stall_tape(marker, responses, "interactive").await;
    wait_for_request_terminal_state(&actors, &doc_id).await;
    let request = fetch_terminal_request(&actors, &doc_id).await;
    let calls = fetch_inference_calls(&actors, &format!("req-{marker}")).await;
    assert_eq!(request.lifecycle_state, RequestLifecycleState::Completed);
    assert_eq!(backend.observed_requests(marker), in_loop_attempts + 1);
    assert_parked_once_then_answered(&actors, &doc_id, &calls, in_loop_attempts).await;
    calls
        .iter()
        .filter(|call| call.call_state == "failed")
        .for_each(assert_stall_reason);
    agent.shutdown().await;
}

#[tokio::test]
async fn deterministic_400_tape() {
    let marker = "deterministic-400";
    let backend = ScriptedBackend::start_with_plans(
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
    let db = trial_store("completion-retry-deterministic-400").await;
    let (actors, agent) =
        build_retry_agent(&db, backend.endpoint(), 1, UNREACHED_DEADLINE.as_secs()).await;
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
    let terminal_state = wait_for_request_terminal_state(&actors, &request_doc_id).await;

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
    db: &gents_store::test_support::TestStore,
    endpoint: &str,
    max_concurrent: usize,
    deadline_duration_secs: u64,
) -> (RequestActors, BootedAgent) {
    let (actors, runtime) = build_retry_agent_with_liveness(
        db,
        endpoint,
        max_concurrent,
        deadline_duration_secs,
        None,
        CompletionRetryProfileFields::default(),
    )
    .await;
    let agent = spawn_runtime(db, runtime).await;
    (actors, agent)
}

async fn build_retry_agent(
    db: &gents_store::test_support::TestStore,
    endpoint: &str,
    max_concurrent: usize,
    deadline_duration_secs: u64,
) -> (RequestActors, Gents) {
    build_retry_agent_with_liveness(
        db,
        endpoint,
        max_concurrent,
        deadline_duration_secs,
        None,
        CompletionRetryProfileFields::default(),
    )
    .await
}

async fn build_retry_agent_with_liveness(
    db: &gents_store::test_support::TestStore,
    endpoint: &str,
    max_concurrent: usize,
    deadline_duration_secs: u64,
    stream_liveness_timeout_secs: Option<u64>,
    completion_retry: CompletionRetryProfileFields,
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
    .deadline_duration_secs(deadline_duration_secs)
    .completion_retry(completion_retry);
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

/// Reads the request's inference calls once none is live, re-reading on each
/// `InferenceCall` change. A call whose stream the request deadline cut is
/// ended by its dropped permit's detached cleanup, which nothing orders before
/// the request's own terminal (`tla/DroppedPermitCleanup.tla`:
/// `RequestChanges` holds, `RequestRead` fails).
async fn wait_for_ended_inference_calls(
    actors: &RequestActors,
    request_id: &str,
) -> Vec<TimelineInferenceCallRow> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{ InferenceCall(filter: {{ request_id: {{ _eq: "{escaped}" }}, call_kind: {{ _eq: "inference" }} }}) {{ call_state }} }}"#
    );
    rows_when(actors, "InferenceCall", &query, |data| {
        data["InferenceCall"].as_array().is_some_and(|rows| {
            rows.iter()
                .all(|row| !matches!(row["call_state"].as_str(), Some("queued" | "running")))
        })
    })
    .await;
    fetch_inference_calls(actors, request_id).await
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
