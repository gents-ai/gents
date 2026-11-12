use gents_protocol::output::rows::{
    decode_output_segment_row, decode_transcript_message_row, output_segment_create_variables,
    transcript_message_create_variables, OutputSegmentRow, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS, CREATE_AGENT_MESSAGE_MUTATION,
    CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_session::request::lifecycle::RequestLifecycle;
use std::sync::Arc;

use crate::support::agent_requests::{RequestActors, RequestSeed};
use crate::support::fixtures::bind_managed_agent_backend;
use crate::support::fixtures::configure_agent_tools;
use crate::support::live_inference::wait_for_rows;
use crate::support::{
    accepted_turn::{boot_prepared_accepted_turn, PreparedAcceptedTurn},
    first_row,
    streaming_backend::{MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse},
    test_db, BACKEND_ID,
};

type StatusRow = AgentRequestRow;

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

fn runtime_access(db: &crate::support::TestDb) -> gents_store::config_client::ConfigAccess {
    gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    )
}

async fn create_recovery_session(
    db: &crate::support::TestDb,
    agent_identity: &Arc<dyn gents_store::identity::NodeIdentity>,
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
    let response = gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    )
    .write(
        "test.lifecycle_recovery_session",
        &format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
    )
    .await
    .expect("create recovery session");
    let doc_id = gents_store::graphql::created_doc_id(&response, "AgentSession").unwrap();
    grant_recovery_agent(db, "AgentSession", &doc_id, agent_identity.did()).await;
}

async fn create_recovery_request(
    db: &crate::support::TestDb,
    agent_identity: &Arc<dyn gents_store::identity::NodeIdentity>,
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
        "max_retries": gents_session::request::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
        "request_hop": 0,
    }))
    .unwrap();
    let response = gents_store::config_client::ConfigAccess::local(db.node.clone())
        .write(
            "test.lifecycle_recovery_request",
            &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
        )
        .await
        .expect("create recovery request");
    let doc_id = gents_store::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    grant_recovery_agent(db, "AgentRequest", &doc_id, agent_identity.did()).await;
    doc_id
}

fn read_only_bash_tools(
    db: &crate::support::TestDb,
    agent_did: &str,
    tools: &str,
    command: &str,
    background_enabled: bool,
) -> gents_documents::Tools {
    gents_documents::Tools {
        tools_id: format!("{agent_did}:{tools}"),
        node_did: db.node_identity.did().to_string(),
        host: Some(gents_documents::HostTools {
            bash: Some(gents_documents::BashTools {
                mode: gents_documents::BashMode::ReadOnly,
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
    let identity: Arc<dyn gents_store::identity::NodeIdentity> = db.node_identity.clone();
    gents::Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: gents_tools::surface::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .expect("build lifecycle recovery runtime")
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
        Some(gents_protocol::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT)
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
    let next_retry_at = gents_session::request::lifecycle::background_wake_next_retry_at(
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
    let process_gate_dir =
        tokio::task::spawn_blocking(|| tempfile::tempdir_in(std::env::current_dir().unwrap()))
            .await
            .unwrap()
            .unwrap();
    let process_gate = process_gate_dir.path().join("release");
    assert!(tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new("mkfifo")
            .arg(&process_gate)
            .status(),
    )
    .await
    .unwrap()
    .unwrap()
    .success());
    let gate_path = process_gate.clone();
    let release = tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(gate_path)
    })
    .await
    .unwrap()
    .unwrap();
    let mut release = tokio::fs::File::from_std(release);
    let held_process_args = serde_json::json!({
        "tool_name": "bash",
        "args": {
            "command": "sh",
            "args": ["-c", r#"exec 3< "$1"; IFS= read -r release <&3"#,
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
    let wake_created_at = crate::support::agent_requests::canonical_at(
        chrono::Utc::now() - chrono::Duration::seconds(2),
    );
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
    wait_for_rows(
        &actors.fixture.access,
        "AgentToolCall",
        r#"{ AgentToolCall(filter: {session_id: {_eq: "wake-backoff-session"}, tool_name: {_eq: "bash"}, lifecycle_state: {_eq: "running"}}) {_docID} }"#,
        std::time::Duration::from_secs(10),
        |data| data["AgentToolCall"].as_array().is_some_and(|rows| !rows.is_empty()),
    ).await;
    runtime
        .backend
        .enqueue_response("continue", StreamResponse::bad_request("provider failed"));
    use tokio::io::AsyncWriteExt;
    release.write_all(b"release\n").await.unwrap();
    wait_for_rows(
        &actors.fixture.access,
        "AgentRequest",
        r#"{
            AgentRequest(filter: {session_id: {_eq: "wake-backoff-session"}}) {request_id lifecycle_state}
            AgentMessage(filter: {session_id: {_eq: "wake-backoff-session"}}) {message_key}
        }"#,
        std::time::Duration::from_secs(10),
        |data| {
            let requests = data["AgentRequest"].as_array().unwrap();
            requests.iter().any(|row| row["request_id"] == "failed-wake-backoff" && row["lifecycle_state"] == "failed")
                && requests.iter().any(|row| row["request_id"].as_str().is_some_and(|id| id.starts_with("background-completion-")) && row["lifecycle_state"] == "processing")
                && data["AgentMessage"].as_array().unwrap().iter().any(|row| row["message_key"].as_str().is_some_and(|key| key.starts_with("background-completion-notification:")))
        },
    ).await;
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
    let next_retry_at = gents_session::request::lifecycle::background_wake_next_retry_at(
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
    RequestLifecycle::redrive_failed_background_wakeups_with_identities(
        &db.node,
        db.node_identity.did(),
        std::slice::from_ref(&agent_identity),
    )
    .await
    .expect("deferred redrive sweep");
    assert_eq!(
        background_wake_retry_rows(access, session_id).await,
        wake_rows_before,
        "a sweep inside the persisted backoff window must not create a retry successor"
    );
    let diagnostics =
        gents_toolcalls::background::diagnostics::load_background_completion_diagnostics(
            access,
            db.node_identity.did(),
        )
        .await
        .expect("load persisted completion diagnostics");
    assert!(diagnostics.epochs.iter().any(|epoch|
        epoch.state == "retry_backoff" && epoch.next_retry_at.is_some()
            && epoch.notification_count > epoch.acknowledged_notification_count
    ), "the failed wake's pending notification must remain visible during backoff: {diagnostics:?}");
}

async fn background_wake_retry_rows(
    access: &gents_store::config_client::ConfigAccess,
    session_id: &str,
) -> Vec<AgentRequestRow> {
    let session_id = gents_store::graphql::escape_graphql_string(session_id);
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

async fn seed_accepted_request_projection(
    db: &crate::support::TestDb,
    agent_identity: &Arc<dyn gents_store::identity::NodeIdentity>,
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
    let request_doc_id = gents_store::graphql::escape_graphql_string(request_doc_id);
    let generation_field = match generation {
        Some(value) => format!(
            r#"execution_generation: "{}","#,
            gents_store::graphql::escape_graphql_string(value)
        ),
        None => "execution_generation: null,".into(),
    };
    let expiry_field = match expires_at {
        Some(value) => format!(
            r#"execution_lease_expires_at: "{}","#,
            gents_store::graphql::escape_graphql_string(&value.to_rfc3339())
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
    gents_store::graphql::single_mutation_document(response, &format!("create_{collection}"))
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
    agent_identity: &Arc<dyn gents_store::identity::NodeIdentity>,
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
    agent_identity: &Arc<dyn gents_store::identity::NodeIdentity>,
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

    let escaped_request_doc_id = gents_store::graphql::escape_graphql_string(&request_doc_id);
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

    let escaped_request_doc_id = gents_store::graphql::escape_graphql_string(&request_doc_id);
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
    let escaped_request_doc_id = gents_store::graphql::escape_graphql_string(&request_doc_id);
    let escaped_interrupt_requested_at =
        gents_store::graphql::escape_graphql_string(&interrupt_requested_at);
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
