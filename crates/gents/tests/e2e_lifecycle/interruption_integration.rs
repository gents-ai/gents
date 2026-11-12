use gents_protocol::output::{OutputOutcome, TerminalOutput};
use gents_protocol::row::AgentRequestRow;
use gents_protocol::transcript::present_message;
use gents_session::request::interrupt::interrupt_request;
use gents_store::graphql::escape_graphql_string;

use crate::support::agent_requests::{canonical_now, RequestActors, RequestSeed};
use crate::support::interrupt::{
    managed_runtime_builder, spawn_runtime, wait_for_inference_call_state,
    wait_for_provider_output_contains, wait_for_request_lifecycle_state, BootedAgent,
};
use crate::support::streaming_backend::{MockStreamingBackend, StreamScript};
use crate::support::{first_row, test_db};

const STREAM_MODEL: &str = "default";
const STREAM_BACKEND_ID: &str = "backend-stream";
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
        let (header, native) = gents_session::session::load_canonical_message(
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
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{
        read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    let owner = db.node_identity.did();
    fixture
        .access
        .transact_write("test.interruption.backend_concurrency", |txn| {
            Box::pin(async move {
                let (_, mut backend) = read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::InferenceBackend,
                    owner,
                    STREAM_BACKEND_ID,
                )
                .await?
                .expect("streaming backend is configured");
                backend["max_concurrent"] = max_concurrent.into();
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: gents_store::collection::Collection::InferenceBackend,
                    add: backend.clone(),
                    update: backend,
                }])?;
                apply_desired_state_plan(
                    txn,
                    &plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .expect("set streaming backend concurrency as the User");
}
