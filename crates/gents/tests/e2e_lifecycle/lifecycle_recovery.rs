use gents::tools::call::lifecycle::ToolCallLifecycle;
use gents::RequestLifecycle;
use gents_protocol::output::rows::{
    decode_output_segment_row, decode_transcript_message_row, output_segment_create_variables,
    transcript_message_create_variables, OutputSegmentRow, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS, CREATE_AGENT_MESSAGE_MUTATION,
    CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use std::sync::Arc;

use crate::support::agent_requests::{RequestActors, RequestSeed};
use crate::support::fixtures::bind_managed_agent_backend;
use crate::support::fixtures::configure_agent_tools;
use crate::support::snapshots::{
    fetch_message_snapshots_for_session_as, fetch_tool_call_snapshots_for_session,
};
use crate::support::{
    accepted_turn::{
        boot_prepared_accepted_turn, prepare_managed_accepted_turn, AcceptedTurnRuntime,
        AcceptedTurnSpec, PreparedAcceptedTurn,
    },
    first_row,
    streaming_backend::{MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse},
    test_db, BACKEND_ID,
};

type StatusRow = AgentRequestRow;

#[derive(Debug, Clone, Deserialize)]
struct NotificationDeliveryRow {
    completion_notification_delivered_at: Option<String>,
}

async fn grant_recovery_agent(
    db: &crate::support::TestDb,
    collection: &str,
    doc_id: &str,
    agent_did: &str,
) {
    // The runtime root authored the recovery fixture rows and relates their Agent.
    runtime_access(db)
        .add_actor_relationship(
            collection,
            doc_id,
            "working_agent",
            &identity::Did::new(agent_did).unwrap(),
        )
        .await
        .expect("grant recovery fixture to selected Agent");
}

fn runtime_access(db: &crate::support::TestDb) -> gents::ConfigAccess {
    gents::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    )
}

async fn create_recovery_session(
    db: &crate::support::TestDb,
    agent_identity: &Arc<dyn gents::NodeIdentity>,
    session_id: &str,
    created_at: &str,
) {
    let mut session = crate::support::session_document_in_scope(
        db.node_identity.did(),
        session_id,
        agent_identity.did(),
        created_at,
    );
    session.requester_did = Some(db.node_identity.did().to_owned());
    let input =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(session).unwrap())
            .unwrap();
    let response = gents::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    )
    .write(
        "test.lifecycle_recovery_session",
        &format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
    )
    .await
    .expect("create recovery session");
    let doc_id = gents::store::graphql::created_doc_id(&response, "AgentSession").unwrap();
    grant_recovery_agent(db, "AgentSession", &doc_id, agent_identity.did()).await;
}

async fn create_recovery_request(
    db: &crate::support::TestDb,
    agent_identity: &Arc<dyn gents::NodeIdentity>,
    request_id: &str,
    session_id: &str,
    status: &str,
    created_at: &str,
) -> String {
    let input = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
        "request_id": request_id,
        "purpose": "normal",
        "node_did": db.node_identity.did(),
        "requester_did": db.node_identity.did(),
        "agent_did": agent_identity.did(),
        "session_id": session_id,
        "retry_parent_request": "",
        "retry_root_request": request_id,
        "superseded_by_request": "",
        "content": "hello",
        "lifecycle_state": status,
        "backend_id": "",
        "execution_origin": "interactive",
        "created_at": created_at,
        "retry_count": 0,
        "max_retries": gents::request::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
        "request_hop": 0,
    }))
    .unwrap();
    let response = gents::ConfigAccess::local(db.node.clone())
        .write(
            "test.lifecycle_recovery_request",
            &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
        )
        .await
        .expect("create recovery request");
    let doc_id = gents::store::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    grant_recovery_agent(db, "AgentRequest", &doc_id, agent_identity.did()).await;
    doc_id
}

/// The working Agent's own signed access, the actor every recovery owner and
/// assertion reads its rows through.
fn agent_access(
    db: &crate::support::TestDb,
    fixture: &crate::support::ManagedAgentFixture,
) -> gents::ConfigAccess {
    gents::ConfigAccess::local_managed_agent(
        db.node.clone(),
        fixture.agent.did().parse().expect("valid Agent DID"),
        fixture.agent.as_ref(),
    )
    .expect("managed recovery Agent access")
}

fn read_only_bash_tools(
    db: &crate::support::TestDb,
    agent_did: &str,
    tools: &str,
    command: &str,
    background_enabled: bool,
) -> gents::documents::Tools {
    gents::documents::Tools {
        tools_id: format!("{agent_did}:{tools}"),
        node_did: db.node_identity.did().to_string(),
        host: Some(gents::documents::HostTools {
            bash: Some(gents::documents::BashTools {
                mode: gents::BashMode::ReadOnly,
                read_only_commands: Some(vec![command.into()]),
                background_enabled,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The Node's runtime built from its documents, with the read-only ceiling
/// the recovery tools need.
async fn recovery_runtime(db: &crate::support::TestDb) -> gents::Gents {
    let identity: Arc<dyn gents::NodeIdentity> = db.node_identity.clone();
    gents::Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: gents::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .expect("build lifecycle recovery runtime")
}

/// The User's client opens a session with its Agent, whose accepted turn
/// starts a long-running bash call.
async fn boot_running_recovery_bash(
    db: &crate::support::TestDb,
    fixture: &crate::support::ManagedAgentFixture,
    access: &gents::ConfigAccess,
    name: &str,
    call_id: &str,
) -> (AcceptedTurnRuntime, String, String, String) {
    let agent_did = fixture.binding.agent_did.as_str();
    let session_id = format!("{name}-session");
    let request_id = format!("{name}-request");
    let prompt = format!("{name}-prompt");
    let prepared = prepare_managed_accepted_turn(
        db,
        fixture,
        AcceptedTurnSpec {
            backend_id: "lifecycle-recovery-backend",
            model: "lifecycle-recovery-model",
            parent_agent_did: agent_did,
            configured_agent_dids: &[agent_did],
            request_id: &request_id,
            session_id: &session_id,
            prompt: &prompt,
            accepted_chunks: vec![StreamChunk::tool_call(
                call_id,
                "bash",
                r#"{"command":"sleep","args":["60"]}"#,
            )],
            child_plans: Vec::new(),
            valid_until: None,
            request_hop: None,
            request_setup: None,
        },
    )
    .await;
    configure_agent_tools(
        db,
        db.node_identity.did(),
        agent_did,
        None,
        read_only_bash_tools(db, agent_did, "recovery-tools", "sleep", false),
        Vec::new(),
    )
    .await;
    let runtime = boot_prepared_accepted_turn(db, prepared, recovery_runtime(db).await).await;
    let tool_call_doc_id = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(tool_call_doc_id) =
                accepted_tool_call_doc_id(access, &session_id, call_id).await
            {
                let rows = fetch_tool_call_snapshots_for_session(access, &session_id).await;
                if rows.iter().any(|row| {
                    row.doc_id == tool_call_doc_id
                        && row.lifecycle_state.as_deref() == Some("running")
                }) {
                    break tool_call_doc_id;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("canonical bash call did not reach running");
    (runtime, request_id, session_id, tool_call_doc_id)
}

async fn boot_two_running_recovery_bashes(
    db: &crate::support::TestDb,
    fixture: &crate::support::ManagedAgentFixture,
    access: &gents::ConfigAccess,
) -> ([AcceptedTurnRuntime; 2], [(String, String, String); 2]) {
    let first =
        boot_running_recovery_bash(db, fixture, access, "tool-cancel", "tool-cancel-call").await;
    let second =
        boot_running_recovery_bash(db, fixture, access, "tool-other", "tool-other-call").await;
    let (first_runtime, first_request, first_session, first_doc) = first;
    let (second_runtime, second_request, second_session, second_doc) = second;
    (
        [first_runtime, second_runtime],
        [
            (first_request, first_session, first_doc),
            (second_request, second_session, second_doc),
        ],
    )
}

async fn accepted_tool_call_doc_id(
    access: &gents::ConfigAccess,
    session_id: &str,
    provider_tool_call_id: &str,
) -> Option<String> {
    let session_id = gents::store::graphql::escape_graphql_string(session_id);
    let response = access
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}, order: {{ sequence: ASC }}) {{ _docID node_did requester_did }} }}"#
        ))
        .await
        .expect("load lifecycle recovery accepted headers");
    for row in response["data"]["AgentMessage"]
        .as_array()
        .expect("AgentMessage header rows")
    {
        let header_doc_id = row["_docID"].as_str().expect("header _docID");
        let node_did = row["node_did"].as_str().expect("header node_did");
        let requester_did = row["requester_did"].as_str();
        let (header, _) =
            gents::session::load_canonical_message(access, header_doc_id, node_did, requester_did)
                .await
                .expect("reconstruct lifecycle recovery accepted header");
        for block in header.blocks {
            if let gents_protocol::output::MessageBlock::ToolCall {
                tool_call_doc_id,
                id,
                call_id,
                ..
            } = block
            {
                if id == provider_tool_call_id || call_id.as_deref() == Some(provider_tool_call_id)
                {
                    return Some(tool_call_doc_id);
                }
            }
        }
    }
    None
}

/// Patches the Agent's own row: fixture fault injection for state no live
/// path writes directly.
async fn patch_as_agent(
    access: &gents::ConfigAccess,
    collection: &str,
    doc_id: &str,
    input: serde_json::Value,
) {
    let input = gents_protocol::graphql::graphql_input_literal(&input).expect("render patch");
    access
        .write(
            "test.lifecycle_recovery.patch",
            &format!(
                r#"mutation {{ update_{collection}(docID: "{}", input: {input}) {{ _docID }} }}"#,
                gents::store::graphql::escape_graphql_string(doc_id)
            ),
        )
        .await
        .unwrap_or_else(|error| panic!("patch {collection} {doc_id} as its Agent: {error:#}"));
}

fn background_wake_input(session_id: &str) -> gents_protocol::request_input::RequestInput {
    use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
    RequestInput {
        queue: Some(RequestQueue {
            delivery: Default::default(),
            position: None,
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some(format!("background_completion:{session_id}")),
            queued_after_request_id: Some("foreground-parent".into()),
            interrupted_request_id: None,
            background_completion_wake_version: Some(1),
        }),
        ..Default::default()
    }
}

/// The Agent's own failed scheduled wake: admitted as a pending request,
/// then terminalized as its working Agent.
async fn seed_failed_wake(
    actors: &RequestActors,
    session_id: &str,
    input: &gents_protocol::request_input::RequestInput,
) {
    let mut seed = RequestSeed::new("failed-wake", session_id, "2026-08-12T00:00:00Z")
        .valid_until(Some("2026-08-12T00:00:01Z"));
    seed.content = Some("continue after background completion");
    seed.execution_origin = Some("scheduled");
    let mut create = actors.request_create(&seed);
    create.input = input.clone();
    create.retry_count = 1;
    create.max_retries = 3;
    let failed_doc = actors.create_signed(create).await;
    actors
        .update_request(
            &failed_doc,
            serde_json::json!({
                "lifecycle_state": "failed",
                "backend_id": BACKEND_ID,
                "max_total_tokens": 4096,
                "failure_reason": "backend admission failed",
                "terminalized_at": "2026-08-12T00:00:00Z",
                "terminal_redrive_attempts": 0,
                "deadline": "2026-08-12T00:00:01Z",
            }),
        )
        .await;
    actors
        .seed_session_observation_from_request(
            session_id,
            "failed-wake",
            "continue after background completion",
        )
        .await;
}

#[tokio::test]
async fn failed_background_wake_behind_a_newer_interactive_head_is_ineligible() {
    let db = test_db("lifecycle-background-wake-newer-head").await;
    let actors = RequestActors::new(&db).await;
    let session_id = "wake-redrive-session";
    seed_failed_wake(&actors, session_id, &background_wake_input(session_id)).await;
    let mut newer =
        RequestSeed::new("newer-interactive", session_id, "2099-01-01T00:00:00Z").state("pending");
    newer.content = Some("newer interactive");
    actors.create_request(newer).await;
    let blocked = RequestLifecycle::redrive_failed_background_wakeups_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(actors.agent()),
    )
    .await
    .expect("newer interactive head gate");
    assert_eq!(blocked.ineligible, 1);
    assert_eq!(blocked.redriven, 0);
    assert_eq!(blocked.failed, 0);
    assert_eq!(
        background_wake_retry_rows(&actors.agent_access, session_id)
            .await
            .len(),
        2,
        "an ineligible wake writes no successor"
    );
}

#[tokio::test]
async fn failed_background_wake_redrive_is_bounded_and_idempotent() {
    let _trace = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_test_writer()
            .with_max_level(tracing::Level::WARN)
            .finish(),
    );
    let db = test_db("lifecycle-background-wake-redrive").await;
    let actors = RequestActors::new(&db).await;
    let agent_identity = actors.agent().clone();
    let agent_did = agent_identity.did();
    let node_did = db.node_identity.did().to_string();
    let session_id = "wake-redrive-session";
    let input = background_wake_input(session_id);
    seed_failed_wake(&actors, session_id, &input).await;
    let access = &actors.agent_access;
    let (first, concurrent) = tokio::join!(
        RequestLifecycle::redrive_failed_background_wakeups_with_identities(
            &db.node,
            &node_did,
            std::slice::from_ref(&agent_identity)
        ),
        RequestLifecycle::redrive_failed_background_wakeups_with_identities(
            &db.node,
            &node_did,
            std::slice::from_ref(&agent_identity)
        ),
    );
    let first = first.expect("first concurrent redrive");
    let concurrent = concurrent.expect("second concurrent redrive");
    assert_eq!(first.scanned, 1);
    assert_eq!(concurrent.scanned, 1);
    assert_eq!(
        first.redriven + concurrent.redriven,
        1,
        "redrive reports: {first:?}, {concurrent:?}"
    );
    assert_eq!(first.already_redriven + concurrent.already_redriven, 1);
    assert_eq!(first.failed + concurrent.failed, 0);
    let rows = background_wake_retry_rows(access, session_id).await;
    assert_eq!(rows.len(), 2);
    let successor = rows
        .iter()
        .find(|row| {
            row.request_id != "failed-wake" && row.requester_did.as_deref() == Some(agent_did)
        })
        .expect("retry successor");
    assert_eq!(
        successor.lifecycle_state,
        Some(RequestLifecycleState::Pending)
    );
    assert_eq!(successor.execution_origin.as_deref(), Some("scheduled"));
    let source = rows
        .iter()
        .find(|row| row.request_id == "failed-wake")
        .unwrap();
    assert!(source.doc_id.is_some());
    assert_eq!(successor.retry_parent_request_doc_id, source.doc_id);
    assert_eq!(
        successor.admission_kind.as_deref(),
        Some("runtime-internal")
    );
    assert_eq!(successor.admission_signer_did.as_deref(), Some(agent_did));
    assert!(successor
        .admission_signature
        .as_deref()
        .is_some_and(|signature| !signature.is_empty()));
    assert_eq!(successor.backend_id, None);
    assert_eq!(successor.max_total_tokens, None);
    assert_eq!(
        successor.retry_parent_request.as_deref(),
        Some("failed-wake")
    );
    assert_eq!(successor.retry_root_request.as_deref(), Some("failed-wake"));
    assert_eq!(
        successor.content.as_deref(),
        Some(gents::tools::background::completion::BACKGROUND_COMPLETION_WAKE_PROMPT)
    );
    assert_eq!(successor.retry_count, Some(2));
    assert_eq!(successor.max_retries, Some(3));
    assert_eq!(successor.input.as_ref(), Some(&input));
    assert_eq!(successor.deadline, None);
    assert_eq!(successor.valid_until, None);
    let second = RequestLifecycle::redrive_failed_background_wakeups_with_identities(
        &db.node,
        &node_did,
        std::slice::from_ref(&agent_identity),
    )
    .await
    .expect("repeat redrive");
    assert_eq!(second.redriven, 0);
    assert_eq!(second.already_redriven, 1);
    assert_eq!(
        background_wake_retry_rows(access, session_id).await.len(),
        2
    );
}

#[tokio::test]
async fn failed_background_wake_without_pending_sibling_obeys_persisted_backoff() {
    let db = test_db("lifecycle-background-wake-isolated-backoff").await;
    let actors = RequestActors::new(&db).await;
    let agent_identity = actors.agent().clone();
    let access = &actors.agent_access;
    let node_did = db.node_identity.did().to_string();
    let session_id = "wake-isolated-backoff-session";
    let request_id = "failed-isolated-backoff-wake";
    let input = background_wake_input(session_id);
    let terminalized_at = chrono::Utc::now();
    let created_at = crate::support::agent_requests::canonical_at(
        terminalized_at - chrono::Duration::seconds(1),
    );
    let terminalized_at = terminalized_at.to_rfc3339();
    let mut seed = RequestSeed::new(request_id, session_id, &created_at);
    seed.content = Some("continue after background completion");
    seed.execution_origin = Some("scheduled");
    let mut create = actors.request_create(&seed);
    create.input = input;
    create.retry_count = 4;
    create.max_retries = 6;
    let failed_doc = actors.create_signed(create).await;
    actors
        .update_request(
            &failed_doc,
            serde_json::json!({
                "lifecycle_state": "failed",
                "failure_reason": "provider failed",
                "terminalized_at": terminalized_at,
            }),
        )
        .await;
    actors
        .seed_session_observation_from_request(
            session_id,
            request_id,
            "continue after background completion",
        )
        .await;

    let before = background_wake_retry_rows(access, session_id).await;
    assert_eq!(before.len(), 1, "no same-key pending wake may mask backoff");
    assert_eq!(
        before[0].lifecycle_state,
        Some(RequestLifecycleState::Failed)
    );
    let next_retry_at = gents::request::lifecycle::background_wake_next_retry_at(
        before[0].terminalized_at.as_deref(),
        before[0].retry_count.expect("persisted retry count"),
    )
    .expect("persisted terminal timestamp determines backoff");
    assert!(next_retry_at > chrono::Utc::now());

    let report = RequestLifecycle::redrive_failed_background_wakeups_with_identities(
        &db.node,
        &node_did,
        std::slice::from_ref(&agent_identity),
    )
    .await
    .expect("isolated backoff sweep");
    assert_eq!(report.scanned, 1);
    assert_eq!(report.deferred, 1);
    assert_eq!(report.redriven, 0);
    assert_eq!(report.coalesced, 0);
    assert_eq!(report.already_redriven, 0);
    assert_eq!(report.ineligible, 0);
    assert_eq!(report.failed, 0);
    assert_eq!(background_wake_retry_rows(access, session_id).await, before);
}

#[tokio::test]
async fn failed_background_wake_waits_for_persisted_backoff() {
    let db = test_db("lifecycle-background-wake-backoff").await;
    let actors = RequestActors::new(&db).await;
    let agent_identity = actors.agent().clone();
    let agent_did = agent_identity.did();
    let access = &actors.agent_access;
    let session_id = "wake-backoff-session";
    let input = background_wake_input(session_id);
    let source_request_id = "foreground-parent";
    let mut source = RequestSeed::new(source_request_id, session_id, "2020-01-01T00:00:00Z")
        .state("pending")
        .valid_until(Some("2020-01-01T00:00:00Z"));
    source.content = Some("expired background wake source");
    let source_doc_id = actors.create_request(source).await;
    let process_gate_dir = tempfile::tempdir_in(
        std::env::current_dir().expect("resolve lifecycle fixture working directory"),
    )
    .expect("create held-process gate directory");
    let process_gate = process_gate_dir.path().join("release");
    let held_process_args = serde_json::json!({
        "tool_name": "bash",
        "args": {
            "command": "sh",
            "args": ["-c", r#"while [ ! -f "$1" ]; do sleep 0.05; done"#,
                "held-process", process_gate.to_string_lossy()],
        },
    })
    .to_string();
    let backend = MockStreamingBackend::start_with_plans(
        "wake-backoff-model",
        vec![StreamPlan::current_authored_user(
            "continue",
            vec![
                StreamResponse::streams(
                    "continue",
                    vec![StreamChunk::tool_call(
                        "wake-backoff-spawn",
                        "spawn_process",
                        held_process_args,
                    )],
                ),
                StreamResponse::completes("continue", ["parent complete"]),
            ],
        )],
    )
    .expect("start failed-wake backend");
    bind_managed_agent_backend(
        &db,
        &actors.fixture,
        "wake-backoff-backend",
        backend.endpoint(),
        "wake-backoff-model",
    )
    .await;
    actors
        .set_session_title(
            session_id,
            &gents_protocol::session::SessionTitle {
                text: "generated-title".into(),
                source: gents_protocol::session::SessionTitleSource::Generated,
            },
        )
        .await;
    // The Agent's runtime-local wake continuing the stale foreground request.
    let wake_created_at = crate::support::agent_requests::canonical_now();
    let mut wake = RequestSeed::new("failed-wake-backoff", session_id, &wake_created_at);
    wake.content = Some("continue");
    wake.execution_origin = Some("scheduled");
    let mut wake = actors.request_create(&wake);
    wake.input = input;
    wake.admission =
        AgentRequestAdmissionRecord::runtime_local_control(agent_did, source_request_id);
    wake.caused_by_parent_request_id = Some(source_request_id.into());
    wake.caused_by_parent_request_doc_id = Some(source_doc_id);
    wake.retry_count = 4;
    wake.max_retries = 6;
    actors.create_signed(wake).await;
    let prepared = PreparedAcceptedTurn { backend };
    const WAKE_INPUT_MARKER: &str = "Review the background execution results above.";
    prepared.backend.enable_dynamic_followups("continue");
    prepared.backend.enable_dynamic_followups(WAKE_INPUT_MARKER);
    configure_agent_tools(
        &db,
        db.node_identity.did(),
        agent_did,
        None,
        read_only_bash_tools(&db, agent_did, "wake-backoff-tools", "sh", true),
        Vec::new(),
    )
    .await;
    let agent = recovery_runtime(&db).await;
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    let running = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if fetch_tool_call_snapshots_for_session(access, session_id)
                .await
                .iter()
                .any(|row| {
                    row.tool_name == "bash" && row.lifecycle_state.as_deref() == Some("running")
                })
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    if running.is_err() {
        let state = actors.query(
            r#"{ AgentRequest(filter: { request_id: { _eq: "failed-wake-backoff" } }, limit: 1) { request_id lifecycle_state failure_reason retry_count max_retries } }"#,
        ).await;
        panic!(
            "background process did not reach running; request={:?}; provider_calls={}",
            state.data,
            runtime.backend.observed_requests("continue"),
        );
    }
    runtime
        .backend
        .enqueue_response("continue", StreamResponse::bad_request("provider failed"));
    let release_second = chrono::Utc::now().timestamp();
    while chrono::Utc::now().timestamp() <= release_second {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    std::fs::write(&process_gate, b"release\n").expect("release held background process");
    let successor = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let request = actors.query(
                r#"{ AgentRequest(filter: { request_id: { _eq: "failed-wake-backoff" } }, limit: 1) { request_id lifecycle_state } }"#,
            ).await;
            if first_row::<StatusRow>(&request, "AgentRequest").lifecycle_state
                == Some(RequestLifecycleState::Failed)
                && fetch_message_snapshots_for_session_as(access, session_id)
                    .await
                    .iter()
                    .any(|message| message.message_key.starts_with("background-completion-notification:"))
                && runtime.backend.observed_requests("continue") == 2
                && runtime.backend.observed_requests(WAKE_INPUT_MARKER) == 1
                && background_wake_retry_rows(access, session_id)
                    .await
                    .iter()
                    .any(|row| {
                        row.request_id.starts_with("background-completion-")
                            && row.lifecycle_state == Some(RequestLifecycleState::Processing)
                    })
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    if let Err(error) = successor {
        let rows = background_wake_retry_rows(access, session_id).await;
        let notifications = fetch_message_snapshots_for_session_as(access, session_id)
            .await
            .into_iter()
            .map(|message| message.message_key)
            .collect::<Vec<_>>();
        let tools = fetch_tool_call_snapshots_for_session(access, session_id)
            .await
            .into_iter()
            .map(|call| {
                serde_json::json!({
                    "name":call.tool_name, "state":call.lifecycle_state,
                })
            })
            .collect::<Vec<_>>();
        let inputs = runtime
            .backend
            .observed_completion_bodies()
            .into_iter()
            .map(|body| {
                body["messages"]
                    .as_array()
                    .and_then(|messages| {
                        messages
                            .iter()
                            .rev()
                            .find(|message| message["role"] == "user")
                    })
                    .map(|message| message["content"].clone())
            })
            .collect::<Vec<_>>();
        panic!("failed wake did not retain its notification and processing successor: {error}; requests={}; messages={notifications:?}; tools={tools:?}; provider_inputs={inputs:?}; provider_calls={}",
            serde_json::to_string(&rows).expect("encode wake diagnostics"),
            runtime.backend.observed_requests("continue"),
        );
    }
    runtime.shutdown().await;

    let wake_rows_before = background_wake_retry_rows(access, session_id).await;
    let failed_wake = wake_rows_before
        .iter()
        .find(|row| row.request_id == "failed-wake-backoff")
        .expect("physical failed wake must remain persisted");
    assert_eq!(
        failed_wake.lifecycle_state,
        Some(RequestLifecycleState::Failed)
    );
    let next_retry_at = gents::request::lifecycle::background_wake_next_retry_at(
        failed_wake.terminalized_at.as_deref(),
        failed_wake.retry_count.expect("persisted retry count"),
    )
    .expect("failed wake must persist a parseable terminal timestamp");
    assert!(
        next_retry_at > chrono::Utc::now(),
        "the explicit sweep must run inside the persisted backoff window: {next_retry_at}"
    );
    assert!(
        wake_rows_before.iter().all(|row| {
            row.retry_parent_request_doc_id.as_deref() != failed_wake.doc_id.as_deref()
        }),
        "the live runtime must not have redriven the failed wake before the bounded sweep"
    );
    let sweep_candidates = wake_rows_before
        .iter()
        .filter(|row| {
            row.node_did.as_deref() == Some(db.node_identity.did())
                && row.lifecycle_state == Some(RequestLifecycleState::Failed)
                && row.execution_origin.as_deref() == Some("scheduled")
        })
        .map(|row| {
            row.doc_id
                .as_deref()
                .expect("wake candidate physical identity")
        })
        .collect::<Vec<_>>();
    let report = RequestLifecycle::redrive_failed_background_wakeups_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent_identity),
    )
    .await
    .expect("deferred redrive sweep");
    let wake_rows = background_wake_retry_rows(access, session_id).await;
    let coalesced_pending = wake_rows_before
        .iter()
        .filter(|row| {
            row.lifecycle_state == Some(RequestLifecycleState::Pending)
                && row
                    .input
                    .as_ref()
                    .and_then(|input| input.queue.as_ref())
                    .is_some_and(|queue| {
                        queue.source
                            == gents_protocol::request_input::QueueSource::BackgroundCompletion
                            && queue.policy == gents_protocol::request_input::QueuePolicy::Coalesce
                            && queue.key
                                == failed_wake
                                    .input
                                    .as_ref()
                                    .and_then(|input| input.queue.as_ref())
                                    .and_then(|queue| queue.key.clone())
                    })
        })
        .count();
    assert!(
        coalesced_pending <= 1,
        "one canonical pending wake per queue key"
    );
    for candidate in wake_rows_before.iter().filter(|row| {
        row.node_did.as_deref() == Some(db.node_identity.did())
            && row.lifecycle_state == Some(RequestLifecycleState::Failed)
            && row.execution_origin.as_deref() == Some("scheduled")
    }) {
        assert_eq!(
            candidate
                .input
                .as_ref()
                .and_then(|input| input.queue.as_ref())
                .and_then(|queue| queue.key.as_ref()),
            failed_wake
                .input
                .as_ref()
                .and_then(|input| input.queue.as_ref())
                .and_then(|queue| queue.key.as_ref()),
            "all scanned wakes share the physical coalescing key"
        );
    }
    assert_eq!(
        report.scanned,
        sweep_candidates.len(),
        "the sweep must account for the exact failed scheduled candidates present at its boundary: {sweep_candidates:?}"
    );
    assert_eq!(
        report.coalesced,
        if coalesced_pending == 1 {
            report.scanned
        } else {
            0
        }
    );
    assert_eq!(
        report.deferred,
        if coalesced_pending == 0 {
            report.scanned
        } else {
            0
        }
    );
    assert_eq!(
        report.redriven
            + report.deferred
            + report.already_redriven
            + report.coalesced
            + report.ineligible
            + report.failed,
        report.scanned,
        "every scanned canonical wake must have one exact sweep disposition"
    );
    assert_eq!(report.redriven, 0);
    assert_eq!(
        wake_rows, wake_rows_before,
        "neither disposition writes a retry successor"
    );
    assert!(!wake_rows.is_empty());
    let diagnostics = gents::load_background_completion_diagnostics(access, db.node_identity.did())
        .await
        .expect("load persisted completion diagnostics");
    assert_eq!(diagnostics.pending_notifications, 1);
    assert_eq!(diagnostics.stranded_notifications, 0, "{diagnostics:?}");
    let notification_key = fetch_message_snapshots_for_session_as(access, session_id)
        .await
        .into_iter()
        .filter(|message| {
            message
                .message_key
                .starts_with("background-completion-notification:")
        })
        .map(|message| message.message_key)
        .collect::<Vec<_>>();
    assert_eq!(
        notification_key.len(),
        1,
        "the failed tool must publish one exact canonical completion notification"
    );
    let notification_key = &notification_key[0];
    let active_epoch = diagnostics
        .epochs
        .iter()
        .filter(|epoch| {
            epoch
                .pending_notification_keys
                .iter()
                .any(|key| key == notification_key)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        active_epoch.len(),
        1,
        "the pending notification must select one physical active wake epoch: {diagnostics:?}"
    );
    let active_epoch = active_epoch[0];
    assert_eq!(active_epoch.state, "retry_backoff");
    assert_eq!(active_epoch.notification_count, 1);
    assert_eq!(active_epoch.acknowledged_notification_count, 0);
    assert!(active_epoch.next_retry_at.is_some());
    assert!(wake_rows
        .iter()
        .any(|row| row.request_id == active_epoch.active_request_id));
    for epoch in diagnostics
        .epochs
        .iter()
        .filter(|epoch| epoch.active_request_id != active_epoch.active_request_id)
    {
        assert_eq!(
            epoch.state, "retry_ineligible_not_latest",
            "a displaced physical wake must be diagnosed as ineligible, not as the active retry epoch"
        );
        assert_eq!(epoch.next_retry_at, None);
    }

    actors
        .create_request(
            RequestSeed::new(
                "later-interactive-request",
                session_id,
                "2099-01-01T00:00:00Z",
            )
            .state("pending"),
        )
        .await;
    actors
        .seed_session_observation_from_request(
            session_id,
            "later-interactive-request",
            "new user turn",
        )
        .await;
    let displaced = gents::load_background_completion_diagnostics(access, db.node_identity.did())
        .await
        .expect("load displaced completion diagnostics");
    assert_eq!(displaced.pending_notifications, 1);
    assert_eq!(displaced.stranded_notifications, 1);
    assert_eq!(displaced.epochs[0].state, "retry_ineligible_not_latest");
    assert_eq!(displaced.epochs[0].next_retry_at, None);
}

async fn background_wake_retry_rows(
    access: &gents::ConfigAccess,
    session_id: &str,
) -> Vec<AgentRequestRow> {
    let session_id = gents::store::graphql::escape_graphql_string(session_id);
    let response = access
        .execute(&format!(
            r#"{{
                AgentRequest(
                    filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                    order: {{ created_at: ASC }}
                ) {{
                    _docID request_id node_did requester_did content lifecycle_state execution_origin
                    retry_parent_request retry_parent_request_doc_id retry_root_request retry_count max_retries
                    admission_kind admission_signer_did admission_signature backend_id max_total_tokens
                    input deadline terminalized_at valid_until
                }}
            }}"#
        ))
        .await
        .expect("fetch background wake retries");
    serde_json::from_value(response["data"]["AgentRequest"].clone())
        .expect("decode wake retry rows")
}

/// Latches the crashed request's interrupt as its working Agent, expires its
/// lease, and recovers it through the canonical request owner.
async fn recover_interrupted_request_after_crash(
    db: &crate::support::TestDb,
    access: &gents::ConfigAccess,
    doc_id: &str,
    agent_identity: &Arc<dyn gents::NodeIdentity>,
) {
    let physical = gents::store::graphql::escape_graphql_string(doc_id);
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }} }}, limit: 2) {{ _docID request_id node_did requester_did execution_generation }} }}"#,
        ))
        .await
        .expect("load exact request interrupt scope");
    let row: AgentRequestRow = serde_json::from_value(response["data"]["AgentRequest"][0].clone())
        .expect("exact interrupted request");
    assert_eq!(row.doc_id.as_deref(), Some(doc_id));
    let node_did = row
        .node_did
        .as_deref()
        .expect("accepted request principal identity");
    gents::request::interrupt::interrupt_request_by_doc_id(
        &db.node,
        agent_identity.managed_agent_signer().as_ref(),
        doc_id,
        node_did,
        row.requester_did.as_deref(),
    )
    .await
    .expect("latch exact request interrupt through canonical owner");
    let generation = row
        .execution_generation
        .as_deref()
        .expect("crashed request execution generation");
    patch_as_agent(
        access,
        "AgentRequest",
        doc_id,
        serde_json::json!({
            "execution_generation": generation,
            "execution_lease_expires_at":
                (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
        }),
    )
    .await;
    let report = RequestLifecycle::recover_all_with_identities(
        &db.node,
        node_did,
        std::slice::from_ref(agent_identity),
    )
    .await
    .expect("recover exact interrupted request after crash");
    assert_eq!(
        report.requests_recovered, 1,
        "only the selected expired interrupted owner may terminalize before tool recovery"
    );
    let recovered = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }} }}, limit: 1) {{ request_id lifecycle_state }} }}"#,
        ))
        .await
        .expect("read recovered request as its Agent");
    let recovered: StatusRow =
        serde_json::from_value(recovered["data"]["AgentRequest"][0].clone()).unwrap();
    assert_eq!(
        recovered.lifecycle_state,
        Some(RequestLifecycleState::Interrupted),
        "canonical request recovery must establish the terminal-parent boundary"
    );
}

async fn seed_accepted_request_projection(
    db: &crate::support::TestDb,
    agent_identity: &Arc<dyn gents::NodeIdentity>,
    session_id: &str,
    request_id: &str,
) {
    create_recovery_session(db, agent_identity, session_id, "2026-03-23T00:00:00Z").await;
    crate::support::seed_session_observation_from_request(
        &db.node,
        session_id,
        request_id,
        "stuck request",
    )
    .await;
}

/// Fixture fault injection only: force the exact stored lease fields the
/// production recovery predicate compares; no live path writes them directly.
async fn seed_expired_execution_lease(
    node: &gents::defra_node::Node,
    request_doc_id: &str,
    generation: Option<&str>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    let request_doc_id = gents::store::graphql::escape_graphql_string(request_doc_id);
    let generation_field = match generation {
        Some(value) => format!(
            r#"execution_generation: "{}","#,
            gents::store::graphql::escape_graphql_string(value)
        ),
        None => "execution_generation: null,".into(),
    };
    let expiry_field = match expires_at {
        Some(value) => format!(
            r#"execution_lease_expires_at: "{}","#,
            gents::store::graphql::escape_graphql_string(&value.to_rfc3339())
        ),
        None => "execution_lease_expires_at: null,".into(),
    };
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{request_doc_id}" }} }},
                input: {{
                    {generation_field}
                    {expiry_field}
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "set execution lease failed: {:?}",
        response.errors
    );
}

fn fixture_provider_turn_source() -> gents_protocol::output::OutputSource {
    gents_protocol::output::OutputSource::ProviderTurn {
        scope: gents_protocol::rendered_request::CaptureScope {
            kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
            seq: 1,
        },
        turn_index: 0,
        attempt: 0,
    }
}

fn fixture_provider_message_key(
    request_doc_id: &str,
    source: &gents_protocol::output::OutputSource,
) -> String {
    format!(
        "provider:{request_doc_id}:{}",
        serde_json::to_string(source).expect("serializing fixture output source")
    )
}

fn canonical_created_doc_id(
    response: &gents::defra_node::QueryResponse,
    collection: &str,
) -> String {
    gents::store::graphql::single_mutation_document(response, &format!("create_{collection}"))
        .expect("decode canonical mutation response")
        .and_then(|row| row.get("_docID"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            panic!(
                "pinned create_{collection} response shape: {:?}",
                response.errors
            )
        })
}

#[allow(clippy::too_many_arguments)]
async fn seed_completed_canonical_output(
    db: &crate::support::TestDb,
    agent_identity: &Arc<dyn gents::NodeIdentity>,
    request_doc_id: &str,
    session_id: &str,
    node_did: &str,
    generation: &str,
    content: &str,
    created_at: &str,
) -> (String, String) {
    let source = fixture_provider_turn_source();
    let segment = gents_protocol::output::OutputSegment {
        node_did: node_did.to_string(),
        requester_did: Some(db.node_identity.did().to_owned()),
        session_id: session_id.to_string(),
        request_doc_id: request_doc_id.to_string(),
        source: source.clone(),
        writer: gents_protocol::output::OutputWriter::RequestExecution {
            execution_generation: generation.to_string(),
        },
        ordinal: Some(0),
        runs: vec![gents_protocol::output::SegmentRun {
            stream: 0,
            bytes: content.len() as u32,
            declaration: Some(gents_protocol::output::StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: gents_protocol::output::StreamPayload::Text,
            }),
        }],
        payload: content.to_string(),
        close: Some(gents_protocol::output::SourceClose::Closed {
            outcome: gents_protocol::output::OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![content.len() as u64],
        }),
        created_at: created_at.to_string(),
    };
    let segment_response = db
        .node
        .execute_request(
            gents::defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
        )
        .await;
    assert!(
        !segment_response.has_errors(),
        "canonical segment fixture create failed: {:?}",
        segment_response.errors
    );
    let segment_doc_id = canonical_created_doc_id(&segment_response, "AgentOutputSegment");
    grant_recovery_agent(
        db,
        "AgentOutputSegment",
        &segment_doc_id,
        agent_identity.did(),
    )
    .await;
    let close_doc_id = segment_doc_id.clone();
    let message = gents_protocol::output::TranscriptMessage {
        native_request_id: None,
        message_key: fixture_provider_message_key(request_doc_id, &source),
        node_did: node_did.to_string(),
        session_id: session_id.to_string(),
        requester_did: Some(db.node_identity.did().to_owned()),
        request_doc_id: Some(request_doc_id.to_string()),
        publication: gents_protocol::output::MessagePublication::RequestExecution {
            execution_generation: generation.to_string(),
        },
        outcome: gents_protocol::output::OutputOutcome::Complete,
        sequence: 1,
        role: gents_protocol::output::MessageRole::Assistant,
        native_id: None,
        blocks: vec![gents_protocol::output::MessageBlock::Text {
            text: gents_protocol::output::PresentedPayload {
                output: gents_protocol::output::PayloadRef {
                    close_doc_id: close_doc_id.clone(),
                    stream: 0,
                },
                presentation: gents_protocol::output::PayloadPresentation::Full,
            },
        }],
        created_at: created_at.to_string(),
    };
    let message_response = db
        .node
        .execute_request(
            gents::defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&message).unwrap()),
        )
        .await;
    assert!(
        !message_response.has_errors(),
        "canonical message fixture create failed: {:?}",
        message_response.errors
    );
    let message_doc_id = canonical_created_doc_id(&message_response, "AgentMessage");
    grant_recovery_agent(db, "AgentMessage", &message_doc_id, agent_identity.did()).await;
    (segment_doc_id, message_doc_id)
}

#[allow(clippy::too_many_arguments)]
async fn seed_open_canonical_flush(
    db: &crate::support::TestDb,
    agent_identity: &Arc<dyn gents::NodeIdentity>,
    request_doc_id: &str,
    session_id: &str,
    node_did: &str,
    generation: &str,
    content: &str,
    created_at: &str,
) -> String {
    let segment = gents_protocol::output::OutputSegment {
        node_did: node_did.to_string(),
        requester_did: Some(db.node_identity.did().to_owned()),
        session_id: session_id.to_string(),
        request_doc_id: request_doc_id.to_string(),
        source: fixture_provider_turn_source(),
        writer: gents_protocol::output::OutputWriter::RequestExecution {
            execution_generation: generation.to_string(),
        },
        ordinal: Some(0),
        runs: vec![gents_protocol::output::SegmentRun {
            stream: 0,
            bytes: content.len() as u32,
            declaration: Some(gents_protocol::output::StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: gents_protocol::output::StreamPayload::Text,
            }),
        }],
        payload: content.to_string(),
        close: None,
        created_at: created_at.to_string(),
    };
    let segment_response = db
        .node
        .execute_request(
            gents::defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
        )
        .await;
    assert!(
        !segment_response.has_errors(),
        "open canonical segment fixture create failed: {:?}",
        segment_response.errors
    );
    let doc_id = canonical_created_doc_id(&segment_response, "AgentOutputSegment");
    grant_recovery_agent(db, "AgentOutputSegment", &doc_id, agent_identity.did()).await;
    doc_id
}

#[tokio::test]
async fn recover_all_marks_requests_as_error() {
    let db = test_db("lifecycle-recover-error").await;
    let agent_identity = crate::support::managed_agent(&db).await;
    let request_doc_id = create_recovery_request(
        &db,
        &agent_identity,
        "stuck-1",
        "session-1",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    seed_expired_execution_lease(
        &db.node,
        &request_doc_id,
        Some("expired-generation"),
        Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
    )
    .await;
    seed_accepted_request_projection(&db, &agent_identity, "session-1", "stuck-1").await;

    let report = RequestLifecycle::recover_all_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent_identity),
    )
    .await
    .unwrap();
    assert_eq!(report.requests_recovered, 1);

    let resp = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "stuck-1" } },
                    limit: 1
                ) { request_id lifecycle_state execution_generation }
            }"#,
        )
        .await;
    let request = first_row::<StatusRow>(&resp, "AgentRequest");
    assert_eq!(request.lifecycle_state, Some(RequestLifecycleState::Failed));
    assert_ne!(
        request.execution_generation.as_deref(),
        Some("expired-generation"),
        "recovery must take ownership with a fresh generation"
    );
}

#[tokio::test]
async fn recover_all_preserves_completed_canonical_output_after_lease_expiry() {
    let db = test_db("lifecycle-recover-complete").await;
    let agent_identity = crate::support::managed_agent(&db).await;
    let request_doc_id = create_recovery_request(
        &db,
        &agent_identity,
        "stuck-complete",
        "session-complete",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    seed_expired_execution_lease(
        &db.node,
        &request_doc_id,
        Some("expired-completed-generation"),
        Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
    )
    .await;
    seed_accepted_request_projection(&db, &agent_identity, "session-complete", "stuck-complete")
        .await;
    let (segment_doc_id, header_doc_id) = seed_completed_canonical_output(
        &db,
        &agent_identity,
        &request_doc_id,
        "session-complete",
        db.node_identity.did(),
        "expired-completed-generation",
        "complete",
        "2026-03-23T00:00:05Z",
    )
    .await;
    let report = RequestLifecycle::recover_all_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent_identity),
    )
    .await
    .unwrap();
    assert_eq!(report.requests_recovered, 1);

    let escaped_request_doc_id = gents::store::graphql::escape_graphql_string(&request_doc_id);
    let request_resp = db
        .node
        .execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_request_doc_id}" }} }}, limit: 1)
            {{ request_id lifecycle_state failure_reason terminal_output }} }}"#))
        .await;
    let request = first_row::<StatusRow>(&request_resp, "AgentRequest");
    assert_eq!(
        request.request_id, "stuck-complete",
        "the exact physical request document must be addressed by doc id"
    );
    assert_eq!(
        request.lifecycle_state,
        Some(RequestLifecycleState::Failed),
        "an expired lease terminalizes as failed even when its output completed"
    );
    assert_eq!(
        request.failure_reason.as_deref(),
        Some("execution lease expired"),
        "production recovery records the exact lease-expiry failure reason"
    );
    assert_eq!(
        request.terminal_output,
        Some(gents_protocol::output::TerminalOutput::Message {
            message_doc_id: header_doc_id.clone(),
        }),
        "the completed source is closed Complete and headed, so recovery \
         selects the exact existing header rather than republishing"
    );

    let response = db
        .node
        .execute(&format!(
            r#"{{ AgentMessage(
                    filter: {{ request_doc_id: {{ _eq: "{escaped_request_doc_id}" }} }},
                    limit: 1
                ) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
        ))
        .await;
    let message =
        decode_transcript_message_row(&first_row::<serde_json::Value>(&response, "AgentMessage"))
            .unwrap();
    assert_eq!(
        message.message.message_key,
        fixture_provider_message_key(&request_doc_id, &fixture_provider_turn_source()),
        "completed canonical output must be preserved"
    );
    assert_eq!(
        message.message.outcome,
        gents_protocol::output::OutputOutcome::Complete
    );
    let [gents_protocol::output::MessageBlock::Text { text }] = message.message.blocks.as_slice()
    else {
        panic!("a completed fixture message carries exactly one text block")
    };
    assert_eq!(
        text.presentation,
        gents_protocol::output::PayloadPresentation::Full,
        "full presentation payload must be preserved"
    );
    assert_eq!(
        text.output.close_doc_id, segment_doc_id,
        "the retained text block must reference the exact closing record"
    );
    assert!(
        !message.message.blocks.iter().any(|block| matches!(
            block,
            gents_protocol::output::MessageBlock::Text { text }
                if matches!(text.presentation, gents_protocol::output::PayloadPresentation::Composed { .. })
        )),
        "recovery must not downgrade a complete publication to partial"
    );
}
#[tokio::test]
async fn recover_all_leaves_live_execution_lease_untouched() {
    let db = test_db("lifecycle-recover-live-lease").await;
    let agent_identity = crate::support::managed_agent(&db).await;
    let request_doc_id = create_recovery_request(
        &db,
        &agent_identity,
        "live-request",
        "live-session",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    seed_expired_execution_lease(
        &db.node,
        &request_doc_id,
        Some("live-generation"),
        Some(chrono::Utc::now() + chrono::Duration::minutes(5)),
    )
    .await;
    seed_accepted_request_projection(&db, &agent_identity, "live-session", "live-request").await;
    let (_segment_doc_id, _header_doc_id) = seed_completed_canonical_output(
        &db,
        &agent_identity,
        &request_doc_id,
        "live-session",
        db.node_identity.did(),
        "live-generation",
        "still running",
        "2026-03-23T00:00:05Z",
    )
    .await;

    let report = RequestLifecycle::recover_all_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent_identity),
    )
    .await
    .unwrap();
    assert_eq!(report.responses_recovered, 0);
    assert_eq!(report.requests_recovered, 0);

    let request_response = db
        .node
        .execute(
            r#"{
                AgentRequest(filter: { request_id: { _eq: "live-request" } }, limit: 1) {
                    request_id lifecycle_state execution_generation execution_lease_expires_at
                }
            }"#,
        )
        .await;
    let request = first_row::<StatusRow>(&request_response, "AgentRequest");
    assert_eq!(
        request.lifecycle_state,
        Some(RequestLifecycleState::Processing)
    );
    assert_eq!(
        request.execution_generation.as_deref(),
        Some("live-generation"),
        "a live lease must keep its exact stored generation"
    );

    let escaped_request_doc_id = gents::store::graphql::escape_graphql_string(&request_doc_id);
    let message = db
        .node
        .execute(&format!(
            r#"{{ AgentMessage(
                    filter: {{ request_doc_id: {{ _eq: "{escaped_request_doc_id}" }} }},
                    limit: 1
                ) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
        ))
        .await;
    let message =
        decode_transcript_message_row(&first_row::<serde_json::Value>(&message, "AgentMessage"))
            .unwrap();
    let [gents_protocol::output::MessageBlock::Text { text }] = message.message.blocks.as_slice()
    else {
        panic!("the live fixture message carries exactly one text block")
    };
    assert_eq!(
        text.presentation,
        gents_protocol::output::PayloadPresentation::Full
    );
    assert_eq!(
        message.message.message_key,
        fixture_provider_message_key(&request_doc_id, &fixture_provider_turn_source())
    );
    assert_eq!(
        message.message.publication,
        gents_protocol::output::MessagePublication::RequestExecution {
            execution_generation: "live-generation".to_string()
        },
        "live canonical output must be untouched"
    );
}

#[tokio::test]
async fn recover_all_interrupts_an_expired_lease_with_a_durable_interrupt() {
    let db = test_db("lifecycle-recover-expired-interrupt").await;
    let agent_identity = crate::support::managed_agent(&db).await;
    let request_doc_id = create_recovery_request(
        &db,
        &agent_identity,
        "interrupted-request",
        "interrupted-session",
        "processing",
        "2026-03-23T00:00:00Z",
    )
    .await;
    seed_expired_execution_lease(
        &db.node,
        &request_doc_id,
        Some("expired-interrupt-generation"),
        Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
    )
    .await;
    seed_accepted_request_projection(
        &db,
        &agent_identity,
        "interrupted-session",
        "interrupted-request",
    )
    .await;
    seed_open_canonical_flush(
        &db,
        &agent_identity,
        &request_doc_id,
        "interrupted-session",
        db.node_identity.did(),
        "expired-interrupt-generation",
        "partial turn",
        "2026-03-23T00:00:05Z",
    )
    .await;
    let interrupt_requested_at = chrono::Utc::now().to_rfc3339();
    let escaped_request_doc_id = gents::store::graphql::escape_graphql_string(&request_doc_id);
    let escaped_interrupt_requested_at =
        gents::store::graphql::escape_graphql_string(&interrupt_requested_at);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_request_doc_id}" }} }},
                input: {{ interrupt_requested_at: "{escaped_interrupt_requested_at}" }}
            ) {{ _docID }}
        }}"#
    );
    let response = db.node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "set durable interrupt failed: {:?}",
        response.errors
    );

    let report = RequestLifecycle::recover_all_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent_identity),
    )
    .await
    .unwrap();
    assert_eq!(report.requests_recovered, 1);
    assert_eq!(report.responses_recovered, 1);

    let request_response = db
        .node
        .execute(
            r#"{
                AgentRequest(
                    filter: { request_id: { _eq: "interrupted-request" } },
                    limit: 1
                ) { request_id lifecycle_state failure_reason execution_generation }
            }"#,
        )
        .await;
    let request = first_row::<StatusRow>(&request_response, "AgentRequest");
    assert_eq!(
        request.lifecycle_state,
        Some(RequestLifecycleState::Interrupted)
    );
    assert_eq!(
        request.failure_reason.as_deref(),
        Some("execution lease expired"),
        "recovery records the exact lease-expiry failure reason"
    );
    assert_ne!(
        request.execution_generation.as_deref(),
        Some("expired-interrupt-generation"),
        "recovery must take ownership with a fresh generation"
    );

    let segment_response = db
        .node
        .execute(&format!(
            r#"{{ AgentOutputSegment(
                    filter: {{ request_doc_id: {{ _eq: "{escaped_request_doc_id}" }} }},
                    limit: 2
                ) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#
        ))
        .await;
    assert!(
        !segment_response.has_errors(),
        "fetch recovered segments failed: {:?}",
        segment_response.errors
    );
    let segments: Vec<OutputSegmentRow> = segment_response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentOutputSegment"))
        .and_then(|rows| rows.as_array())
        .map(|rows| {
            rows.iter()
                .map(|row| decode_output_segment_row(row).expect("decode recovered segment"))
                .collect()
        })
        .expect("segment data present");
    assert_eq!(
        segments.len(),
        2,
        "recovery adds a terminal-only closing record; the open flush is retained"
    );
    let close = segments
        .iter()
        .find(|row| row.segment.close.is_some())
        .expect("recovery commits exactly one closing record");
    assert_eq!(
        close.segment.close,
        Some(gents_protocol::output::SourceClose::Closed {
            outcome: gents_protocol::output::OutputOutcome::Partial,
            segments: 1,
            stream_bytes: vec!["partial turn".len() as u64],
        }),
        "the exact retained extent is closed Partial"
    );

    let response = db
        .node
        .execute(&format!(
            r#"{{ AgentMessage(
                    filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }},
                    limit: 1
                ) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
        ))
        .await;
    let message =
        decode_transcript_message_row(&first_row::<serde_json::Value>(&response, "AgentMessage"))
            .unwrap();
    assert_eq!(
        message.message.outcome,
        gents_protocol::output::OutputOutcome::Partial,
        "interruption recovery publishes a partial canonical transcript message"
    );
    assert!(
        matches!(
            &message.message.publication,
            gents_protocol::output::MessagePublication::RequestRecovery { execution_generation }
                if !execution_generation.is_empty()
                    && execution_generation != "expired-interrupt-generation"
        ),
        "recovery publishes under its fresh recovery generation, never the \
         expired one; got {:?}",
        message.message.publication
    );
    assert!(
        message
            .message
            .message_key
            .starts_with(&format!("request-recovery:{request_doc_id}:")),
        "recovery keys its publication request-recovery:{{doc_id}}:{{source}}; got {:?}",
        message.message.message_key
    );
    let [gents_protocol::output::MessageBlock::Text { text }] = message.message.blocks.as_slice()
    else {
        panic!("recovery retains exactly the one text stream of the open flush")
    };
    assert_eq!(
        text.presentation,
        gents_protocol::output::PayloadPresentation::Full,
        "recovered text retains its full presentation"
    );
    assert_eq!(
        text.output.close_doc_id, close.doc_id,
        "the recovered block references the exact closing record recovery committed"
    );
}

#[tokio::test]
async fn recover_all_times_out_expired_running_tool_calls() {
    let db = test_db("tool-call-recover-timeout").await;
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let access = agent_access(&db, &fixture);
    let (runtime, _request_id, session_id, tool_doc_id) =
        boot_running_recovery_bash(&db, &fixture, &access, "tool-timeout", "tool-timeout-call")
            .await;
    patch_as_agent(
        &access,
        "AgentToolCall",
        &tool_doc_id,
        serde_json::json!({"deadline_at": "2020-01-01T00:00:00Z"}),
    )
    .await;

    let report = ToolCallLifecycle::recover_all(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&fixture.agent),
    )
    .await
    .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);

    let snapshots = fetch_tool_call_snapshots_for_session(&access, &session_id).await;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("timedOut"));
    assert_eq!(snapshots[0].cancel_cause.as_deref(), Some("deadline"));
    assert!(snapshots[0]
        .try_load_result_as(&access)
        .await
        .expect("timed-out call has an exact invocation reply")
        .contains("deadline exceeded"));
    runtime.shutdown().await;
}

#[tokio::test]
async fn recover_all_repairs_terminal_background_tool_notification_once() {
    let db = test_db("tool-call-repair-notification").await;
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let access = agent_access(&db, &fixture);
    let agent_did = fixture.binding.agent_did.as_str();
    let node_did = db.node_identity.did().to_string();
    let session_id = "tool-notification-session";
    let prepared = prepare_managed_accepted_turn(
        &db,
        &fixture,
        AcceptedTurnSpec {
            backend_id: "tool-notification-backend",
            model: "tool-notification-model",
            parent_agent_did: agent_did,
            configured_agent_dids: &[agent_did],
            request_id: "tool-notification-req",
            session_id,
            prompt: "run background notification fixture",
            accepted_chunks: vec![StreamChunk::tool_call(
                "tool-notification-spawn",
                "spawn_process",
                r#"{"tool_name":"bash","args":{"command":"printf","args":["durable result"]}}"#,
            )],
            child_plans: Vec::new(),
            valid_until: None,
            request_hop: None,
            request_setup: None,
        },
    )
    .await;
    configure_agent_tools(
        &db,
        &node_did,
        agent_did,
        None,
        read_only_bash_tools(&db, agent_did, "notification-tools", "printf", true),
        Vec::new(),
    )
    .await;
    let runtime = boot_prepared_accepted_turn(&db, prepared, recovery_runtime(&db).await).await;
    let terminal_tool = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(row) = fetch_tool_call_snapshots_for_session(&access, session_id)
                .await
                .into_iter()
                .find(|row| {
                    row.tool_name == "bash" && row.lifecycle_state.as_deref() == Some("completed")
                })
            {
                break row;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("background bash did not complete");
    let tool_doc_id = terminal_tool.doc_id.clone();
    let notification_key = format!("background-completion-notification:{tool_doc_id}:tool");
    // Stop the live observer first or it may legitimately reconcile the
    // marker ahead of the explicit startup recovery call under test.
    runtime.shutdown().await;
    patch_as_agent(
        &access,
        "AgentToolCall",
        &tool_doc_id,
        serde_json::json!({
            "status": "completionPending",
            "completion_notification_delivered_at": null,
        }),
    )
    .await;

    let first =
        ToolCallLifecycle::recover_all(&db.node, &node_did, std::slice::from_ref(&fixture.agent))
            .await
            .unwrap();
    assert_eq!(first.notifications_repaired, 1);
    let second =
        ToolCallLifecycle::recover_all(&db.node, &node_did, std::slice::from_ref(&fixture.agent))
            .await
            .unwrap();
    assert_eq!(second.notifications_repaired, 0);

    let messages = fetch_message_snapshots_for_session_as(&access, session_id).await;
    let matching = messages
        .iter()
        .filter(|message| message.message_key == notification_key)
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "repair must be durably idempotent by exact key"
    );
    assert!(matching[0].content.contains("durable result"));

    let response = access
        .execute(&format!(
            r#"{{
                AgentToolCall(
                    filter: {{ _docID: {{ _eq: "{}" }} }},
                    limit: 1
                ) {{ completion_notification_delivered_at }}
            }}"#,
            gents::store::graphql::escape_graphql_string(&tool_doc_id),
        ))
        .await
        .expect("read repaired tool call as its Agent");
    let row: NotificationDeliveryRow =
        serde_json::from_value(response["data"]["AgentToolCall"][0].clone())
            .expect("repaired tool call row");
    assert!(
        row.completion_notification_delivered_at.is_some(),
        "successful notification append must advance the delivery marker"
    );
}

#[tokio::test]
async fn recover_all_cancels_running_tool_call_for_interrupted_parent_only() {
    let db = test_db("tool-call-recover-cancel").await;
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let access = agent_access(&db, &fixture);
    let ([interrupted_runtime, unrelated_runtime], rows) =
        boot_two_running_recovery_bashes(&db, &fixture, &access).await;
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID }} }}"#,
            gents::store::graphql::escape_graphql_string(&rows[0].0),
        ))
        .await
        .expect("read interrupted request as its Agent");
    let interrupted_doc = response["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .expect("accepted interrupted request physical identity")
        .to_owned();
    interrupted_runtime.crash().await;
    recover_interrupted_request_after_crash(&db, &access, &interrupted_doc, &fixture.agent).await;

    let report = ToolCallLifecycle::recover_all(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&fixture.agent),
    )
    .await
    .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);

    let cancelled_snapshots = fetch_tool_call_snapshots_for_session(&access, &rows[0].1).await;
    assert_eq!(
        cancelled_snapshots
            .iter()
            .find(|row| row.doc_id == rows[0].2)
            .and_then(|row| row.lifecycle_state.as_deref()),
        Some("cancelled")
    );
    assert_eq!(
        cancelled_snapshots
            .iter()
            .find(|row| row.doc_id == rows[0].2)
            .and_then(|row| row.cancel_cause.as_deref()),
        Some("interrupted")
    );

    let unrelated_snapshots = fetch_tool_call_snapshots_for_session(&access, &rows[1].1).await;
    assert_eq!(
        unrelated_snapshots
            .iter()
            .find(|row| row.doc_id == rows[1].2)
            .and_then(|row| row.lifecycle_state.as_deref()),
        Some("running"),
        "unrelated running tool call should not be swept"
    );
    unrelated_runtime.shutdown().await;
}
