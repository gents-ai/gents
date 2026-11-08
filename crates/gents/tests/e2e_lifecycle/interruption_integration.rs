use gents::interrupt_request;
use gents::request::lifecycle::{ClaimOutcome, ExecutionOrigin};
use gents::store::graphql::escape_graphql_string;
use gents_protocol::output::{OutputOutcome, SourceClose, TerminalOutput};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::transcript::present_message;

use crate::support::agent_requests::{canonical_now, RequestActors, RequestSeed};
use crate::support::interrupt::{
    fetch_output_segments_for_request, managed_runtime_builder, spawn_runtime,
    wait_for_inference_call_state, wait_for_provider_output_contains,
    wait_for_request_lifecycle_state, BootedAgent,
};
use crate::support::snapshots::fetch_request_snapshot;
use crate::support::streaming_backend::{MockStreamingBackend, StreamScript};
use crate::support::{first_row, test_db, BACKEND_ID, DEADLINE_SECS, NODE_DID};

const STREAM_MODEL: &str = "default";
const STREAM_BACKEND_ID: &str = "backend-stream";
const PRIMARY_AGENT: &str = "general";
const TARGET_MARKER: &str = "interrupt-target";
const TARGET_PARTIAL: &str = "partial response content ";
const SURVIVOR_MARKER: &str = "survivor-target";
const SURVIVOR_PARTIAL: &str = "survivor partial content ";

fn run_on_production_runtime(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(16 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("build e2e lifecycle runtime")
        .block_on(future);
}

async fn fetch_terminal_request(actors: &RequestActors, request_doc_id: &str) -> AgentRequestRow {
    let request_doc_id = escape_graphql_string(request_doc_id);
    let response = actors
        .query(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{request_doc_id}" }} }}, limit: 1) {{
                request_id requester_did lifecycle_state interrupt_requested_at terminal_output
            }} }}"#
        ))
        .await;
    first_row(&response, "AgentRequest")
}

/// Submits an interactive request as the Agent in its own titled session.
async fn submit_request(
    actors: &RequestActors,
    request_id: &str,
    session_id: &str,
    content: &str,
) -> String {
    let created_at = canonical_now();
    actors.session(session_id, &created_at).await;
    actors
        .set_session_title(
            session_id,
            &gents_protocol::session::SessionTitle {
                text: "generated-title".into(),
                source: gents_protocol::session::SessionTitleSource::Generated,
            },
        )
        .await;
    let mut seed = RequestSeed::new(request_id, session_id, &created_at);
    seed.content = Some(content);
    actors.create_request(seed).await
}

#[tokio::test]
async fn offline_replay_of_stale_requests_does_not_call_backend() {
    let db = test_db("offline-replay-stale").await;
    let actors = crate::support::agent_requests::RequestActors::new(&db).await;
    let past = (chrono::Utc::now() - chrono::Duration::seconds(10))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let created_at = crate::support::agent_requests::canonical_now();

    const BATCH: usize = 20;
    let mut request_doc_ids = Vec::with_capacity(BATCH);
    for _ in 0..BATCH {
        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let doc_id = actors
            .create_request(
                crate::support::agent_requests::RequestSeed::new(
                    &request_id,
                    &session_id,
                    &created_at,
                )
                .state("pending")
                .valid_until(Some(&past)),
            )
            .await;
        request_doc_ids.push((doc_id, request_id, session_id));
    }

    for (doc_id, request_id, session_id) in request_doc_ids.clone() {
        let request = actors.build_request(doc_id, request_id, session_id, created_at.clone());
        let mut lifecycle = actors.lifecycle(
            request,
            DEADLINE_SECS,
            ExecutionOrigin::Interactive,
            BACKEND_ID,
        );
        assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Expired);
    }

    for (doc_id, _, _) in &request_doc_ids {
        let snap = fetch_request_snapshot(&actors, doc_id).await;
        assert_eq!(snap.lifecycle_state, RequestLifecycleState::Dead);
        assert_eq!(snap.failure_reason, "Stale");
        assert_eq!(
            snap.backend_id, "",
            "stale request must not be bound to a backend"
        );
        assert!(
            !snap.claimed_at_present,
            "stale request must not be claimed"
        );
    }
}

#[tokio::test]
async fn resend_from_stale_populates_retry_chain() {
    let db = test_db("resend-chain").await;
    let actors = crate::support::agent_requests::RequestActors::new(&db).await;

    let created_at = crate::support::agent_requests::canonical_now();
    let past = (chrono::Utc::now() - chrono::Duration::seconds(10))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let original_request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let original_doc_id = actors
        .create_request(
            crate::support::agent_requests::RequestSeed::new(
                &original_request_id,
                &session_id,
                &created_at,
            )
            .state("pending")
            .valid_until(Some(&past)),
        )
        .await;

    let request = actors.build_request(
        original_doc_id.clone(),
        original_request_id.clone(),
        session_id.clone(),
        created_at.clone(),
    );
    let mut lifecycle = actors.lifecycle(
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Expired);

    let resend_1_id = uuid::Uuid::new_v4().to_string();
    let resend_1_created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let resend_1_doc_id = actors
        .create_request(crate::support::agent_requests::RequestSeed {
            retry_parent_request: Some(&original_request_id),
            retry_root_request: Some(&original_request_id),
            content: Some("hello"),
            ..crate::support::agent_requests::RequestSeed::new(
                &resend_1_id,
                &session_id,
                &resend_1_created_at,
            )
            .valid_until(Some(&past))
        })
        .await;

    let snap_1 = fetch_request_snapshot(&actors, &resend_1_doc_id).await;
    assert_eq!(snap_1.retry_parent_request, original_request_id);
    assert_eq!(snap_1.retry_root_request, original_request_id);

    let request_1 = actors.build_request(
        resend_1_doc_id.clone(),
        resend_1_id.clone(),
        session_id.clone(),
        resend_1_created_at.clone(),
    );
    let mut lifecycle_1 = actors.lifecycle(
        request_1,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(lifecycle_1.claim().await.unwrap(), ClaimOutcome::Expired);

    let resend_2_id = uuid::Uuid::new_v4().to_string();
    let resend_2_created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let resend_2_doc_id = actors
        .create_request(crate::support::agent_requests::RequestSeed {
            retry_parent_request: Some(&resend_1_id),
            retry_root_request: Some(&original_request_id),
            content: Some("hello"),
            ..crate::support::agent_requests::RequestSeed::new(
                &resend_2_id,
                &session_id,
                &resend_2_created_at,
            )
            .valid_until(None)
        })
        .await;

    let snap_2 = fetch_request_snapshot(&actors, &resend_2_doc_id).await;
    assert_eq!(snap_2.retry_parent_request, resend_1_id);
    assert_eq!(
        snap_2.retry_root_request, original_request_id,
        "retry_root_request must be stable across the chain"
    );
}

#[tokio::test]
async fn inference_call_wait_observes_latest_attempt() {
    let db = test_db("inference-call-wait-latest").await;
    let request_id = "req-inference-call-wait-latest";

    insert_inference_call(
        db.node.as_ref(),
        request_id,
        1,
        "failed",
        Some("ProviderError: transient connect failure"),
    )
    .await;
    insert_inference_call(db.node.as_ref(), request_id, 2, "running", None).await;

    let call = wait_for_inference_call_state(db.node.as_ref(), request_id, "running").await;
    assert_eq!(call.call_seq, 2);
    assert_eq!(call.call_state, "running");
}

#[test]
fn interrupt_mid_stream_preserves_partial_and_cancels_inference_call() {
    run_on_production_runtime(async {
        let db = test_db("daemon-interrupt-mid-stream").await;
        let backend = MockStreamingBackend::start(
            STREAM_MODEL,
            vec![StreamScript::paused(TARGET_MARKER, [TARGET_PARTIAL])],
        )
        .unwrap();
        let actors = RequestActors::new(&db).await;
        let agent = boot_streaming_agent(&db, backend.endpoint(), &[&actors], 2).await;

        let request_id = "req-daemon-interrupt-mid-stream";
        let session_id = "session-daemon-interrupt-mid-stream";
        let request_doc_id = submit_request(&actors, request_id, session_id, TARGET_MARKER).await;

        backend.wait_for_chunks(TARGET_MARKER, 1).await;
        wait_for_provider_output_contains(&actors, &request_doc_id, TARGET_PARTIAL).await;

        interrupt_request(&db.node, actors.agent().as_ref(), request_id)
            .await
            .expect("interrupt_request should latch interrupt_requested_at");

        wait_for_request_lifecycle_state(&actors, &request_doc_id, "interrupted").await;
        let call = wait_for_inference_call_state(&actors, request_id, "cancelled").await;
        assert_eq!(call.failure_reason.as_deref(), Some("Cancelled"));

        let terminal = fetch_terminal_request(&actors, &request_doc_id).await;
        assert!(
            terminal
                .interrupt_requested_at
                .as_deref()
                .is_some_and(|at| !at.is_empty()),
            "daemon interrupt must retain the accepted interrupt intent timestamp"
        );
        let TerminalOutput::Message { message_doc_id } = terminal
            .terminal_output
            .expect("interrupted request selects retained partial output")
        else {
            panic!("interrupted request must select its retained partial message")
        };
        let (header, native) = gents::session::load_canonical_message(
            &actors.agent_access,
            &message_doc_id,
            agent.node_did.as_str(),
            terminal.requester_did.as_deref(),
        )
        .await
        .expect("reconstruct selected partial message");
        assert_eq!(
            header.request_doc_id.as_deref(),
            Some(request_doc_id.as_str())
        );
        assert_eq!(header.outcome, OutputOutcome::Partial);
        assert_eq!(
            present_message(&native).body_markdown,
            TARGET_PARTIAL.trim()
        );

        let segments = fetch_output_segments_for_request(&actors, &request_doc_id).await;
        assert!(
            segments.iter().any(|row| {
                matches!(
                    &row.segment.close,
                    Some(SourceClose::Closed {
                        outcome: OutputOutcome::Partial,
                        ..
                    })
                )
            }),
            "interrupted provider source must be sealed Partial"
        );

        agent.shutdown().await;
    });
}

#[test]
fn interrupting_one_request_does_not_affect_another() {
    run_on_production_runtime(async {
        let db = test_db("daemon-interrupt-isolation").await;
        let backend = MockStreamingBackend::start(
            STREAM_MODEL,
            vec![
                StreamScript::paused(TARGET_MARKER, [TARGET_PARTIAL]),
                StreamScript::paused(SURVIVOR_MARKER, [SURVIVOR_PARTIAL]),
            ],
        )
        .unwrap();
        let target = RequestActors::new(&db).await;
        let survivor =
            RequestActors::from_fixture(&db, crate::support::managed_agent_fixture(&db).await)
                .await;
        let agent = boot_streaming_agent(&db, backend.endpoint(), &[&target, &survivor], 4).await;

        let target_request_id = "req-daemon-interrupt-target";
        let target_session_id = "session-daemon-interrupt-target";
        let target_doc_id =
            submit_request(&target, target_request_id, target_session_id, TARGET_MARKER).await;

        let survivor_request_id = "req-daemon-survivor";
        let survivor_session_id = "session-daemon-survivor";
        let survivor_doc_id = submit_request(
            &survivor,
            survivor_request_id,
            survivor_session_id,
            SURVIVOR_MARKER,
        )
        .await;

        backend.wait_for_chunks(TARGET_MARKER, 1).await;
        backend.wait_for_chunks(SURVIVOR_MARKER, 1).await;

        wait_for_provider_output_contains(&target, &target_doc_id, TARGET_PARTIAL).await;
        wait_for_provider_output_contains(&survivor, &survivor_doc_id, SURVIVOR_PARTIAL).await;

        interrupt_request(&db.node, target.agent().as_ref(), target_request_id)
            .await
            .expect("interrupt_request should latch interrupt_requested_at");

        wait_for_request_lifecycle_state(&target, &target_doc_id, "interrupted").await;
        let target_call =
            wait_for_inference_call_state(&target, target_request_id, "cancelled").await;
        assert_eq!(target_call.failure_reason.as_deref(), Some("Cancelled"));

        let survivor_running =
            wait_for_inference_call_state(&survivor, survivor_request_id, "running").await;
        assert_eq!(
            survivor_running.call_state, "running",
            "unrelated concurrent inference call must remain live after target interrupt"
        );

        backend.release(SURVIVOR_MARKER);
        wait_for_request_lifecycle_state(&survivor, &survivor_doc_id, "completed").await;
        let survivor_call =
            wait_for_inference_call_state(&survivor, survivor_request_id, "completed").await;
        assert_eq!(survivor_call.failure_reason.as_deref(), None);

        let survivor_row = fetch_terminal_request(&survivor, &survivor_doc_id).await;
        assert!(
            survivor_row.interrupt_requested_at.is_none(),
            "unrelated request must not acquire interrupt intent"
        );
        let TerminalOutput::Message { message_doc_id } = survivor_row
            .terminal_output
            .expect("completed survivor selects canonical output")
        else {
            panic!("completed survivor must select its canonical message")
        };
        let (header, native) = gents::session::load_canonical_message(
            &survivor.agent_access,
            &message_doc_id,
            agent.node_did.as_str(),
            survivor_row.requester_did.as_deref(),
        )
        .await
        .expect("reconstruct survivor message");
        assert_eq!(header.outcome, OutputOutcome::Complete);
        assert_eq!(
            present_message(&native).body_markdown,
            SURVIVOR_PARTIAL.trim()
        );

        agent.shutdown().await;
    });
}

/// Boots the production runtime for the installed Agents, the first as the
/// Node default, all on one streaming backend admitting `max_concurrent` calls.
async fn boot_streaming_agent(
    db: &crate::support::TestDb,
    endpoint: &str,
    agents: &[&RequestActors],
    max_concurrent: i64,
) -> BootedAgent {
    for (index, actors) in agents.iter().enumerate() {
        crate::support::fixtures::bind_selected_managed_agent_backend(
            db,
            &actors.fixture,
            actors.agent_did(),
            index == 0,
            STREAM_BACKEND_ID,
            endpoint,
            STREAM_MODEL,
        )
        .await;
    }
    set_backend_max_concurrent(db, &agents[0].fixture, max_concurrent).await;

    let mut builder = managed_runtime_builder(db, agents[0].agent_did());
    for actors in agents {
        builder = builder
            .agent(actors.agent_did())
            .identity_binding(actors.fixture.binding.clone())
            .backend_id(STREAM_BACKEND_ID)
            .model_name(STREAM_MODEL)
            .stream_batch_ms(0)
            .done();
    }
    spawn_runtime(db, builder.build().await.unwrap()).await
}

/// Raises the backend's admission bound as the Node's configuring User.
async fn set_backend_max_concurrent(
    db: &crate::support::TestDb,
    fixture: &crate::support::ManagedAgentFixture,
    max_concurrent: i64,
) {
    use gents::store::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
        DesiredStateApplyPlan,
    };
    let owner = db.node_identity.did();
    fixture
        .access
        .transact_write("test.interruption.backend_concurrency", |txn| {
            Box::pin(async move {
                let (_, mut backend) = read_desired_state_record_in_txn(
                    txn,
                    gents::Collection::InferenceBackend,
                    owner,
                    STREAM_BACKEND_ID,
                )
                .await?
                .expect("streaming backend is configured");
                backend["max_concurrent"] = max_concurrent.into();
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: gents::Collection::InferenceBackend,
                    add: backend.clone(),
                    update: backend,
                }])?;
                apply_desired_state_plan(txn, &plan).await
            })
        })
        .await
        .expect("set streaming backend concurrency as the User");
}

async fn insert_inference_call(
    node: &gents::defra_node::Node,
    request_id: &str,
    call_seq: i64,
    call_state: &str,
    failure_reason: Option<&str>,
) {
    let call_id = format!("call-{request_id}-{call_seq}");
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_call_id = escape_graphql_string(&call_id);
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_call_state = escape_graphql_string(call_state);
    let escaped_now = escape_graphql_string(&now);
    let failure_reason_field = failure_reason
        .map(|reason| format!(r#"failure_reason: "{}","#, escape_graphql_string(reason)))
        .unwrap_or_default();
    let ended_at_field = if matches!(call_state, "failed" | "completed" | "cancelled") {
        format!(r#"ended_at: "{escaped_now}","#)
    } else {
        String::new()
    };

    let mutation = format!(
        r#"mutation {{
            add_InferenceCall(input: {{
                call_id: "{escaped_call_id}",
                runtime_instance_id: "runtime-test",
                request_id: "{escaped_request_id}",
                call_seq: {call_seq},
                backend_id: "{STREAM_BACKEND_ID}",
                agent_did: "{PRIMARY_AGENT}",
                node_did: "{NODE_DID}",
                call_kind: "inference",
                attempt: {call_seq},
                call_state: "{escaped_call_state}",
                {failure_reason_field}
                queued_at: "{escaped_now}",
                started_at: "{escaped_now}",
                {ended_at_field}
                priority: 0,
                queue_depth_at_enqueue: 0,
                controller_generation: 0,
                backend_config_fingerprint: "test"
            }}) {{ _docID }}
        }}"#,
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "insert inference call failed: {:?}",
        response.errors
    );
}
