use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_session::request::lifecycle::RequestLifecycle;
use std::sync::Arc;

use crate::support::agent_requests::{RequestActors, RequestSeed};
use crate::support::fixtures::bind_managed_agent_backend;
use crate::support::fixtures::configure_agent_tools;
use crate::support::{
    accepted_turn::{boot_prepared_accepted_turn, PreparedAcceptedTurn},
    trial_store,
};
use gents_inference::test_support::scripted_backend::{
    ScriptedBackend, StreamChunk, StreamPlan, StreamResponse,
};
use gents_store::test_support::rows_when;

fn read_only_bash_tools(
    db: &gents_store::test_support::TestStore,
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
async fn recovery_runtime(db: &gents_store::test_support::TestStore) -> gents::Gents {
    let identity: Arc<dyn gents_store::identity::NodeIdentity> = db.node_identity.clone();
    gents::Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            backend_admission: Some(db.backend_admission()),
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

#[tokio::test]
async fn failed_background_wake_sweep_follows_the_lean_redrive_model() {
    use gents_lean_vocab::families::wake_redrive_sweep_sweep;
    for case in gents_lean_vocab::families::wake_redrive_sweep_sweep::cases::<String>() {
        let db = trial_store("lifecycle-background-wake-isolated-backoff").await;
        let actors = RequestActors::new(&db).await;
        let agent_identity = actors.agent().clone();
        let access = &actors.agent_access;
        let node_did = db.node_identity.did().to_string();
        let session_id = "wake-isolated-backoff-session";
        let request_id = "failed-isolated-backoff-wake";
        let input = background_wake_input(session_id);
        let terminalized_at =
            chrono::Utc::now() - chrono::Duration::seconds(case.input.elapsed_seconds as i64);
        let created_at = crate::support::agent_requests::canonical_at(
            terminalized_at - chrono::Duration::seconds(1),
        );
        let terminalized_at = terminalized_at.to_rfc3339();
        let mut seed = RequestSeed::new(request_id, session_id, &created_at);
        seed.content = Some("continue after background completion");
        seed.execution_origin = Some("scheduled");
        let mut create = actors.request_create(&seed);
        create.input = input;
        create.retry_count = case.input.retry_count.try_into().unwrap();
        let failed_doc = actors.create_signed(create).await;
        gents_session::request::retry_budget::write_pin(
            actors.node.clone(),
            db.node_identity.clone(),
            db.node_identity.did(),
            &failed_doc,
            case.input.max_retries,
        )
        .await
        .unwrap();
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
        let report = RequestLifecycle::redrive_failed_background_wakeups_with_identities(
            &db.node,
            &node_did,
            std::slice::from_ref(&agent_identity),
        )
        .await
        .expect("redrive sweep");
        let decision = match (
            report.scanned,
            report.ineligible,
            report.coalesced,
            report.deferred,
            report.redriven,
            report.already_redriven,
            report.failed,
        ) {
            (1, 1, 0, 0, 0, 0, 0) => "ineligible".to_owned(),
            (1, 0, 1, 0, 0, 0, 0) => "coalesced".to_owned(),
            (1, 0, 0, 1, 0, 0, 0) => "deferred".to_owned(),
            (1, 0, 0, 0, 1, 0, 0) => "redriven".to_owned(),
            _ => format!("{report:?}"),
        };
        case.check(wake_redrive_sweep_sweep::Output {
            decision,
            rows_unchanged: background_wake_retry_rows(access, session_id).await == before,
        });
    }
}

/// Property: a failed background wake is not redriven before its persisted
/// backoff (`background_wake_next_retry_at` of its terminal time and retry
/// count): neither the live runtime nor a sweep inside the window creates a
/// retry successor. And the requesting user sees the background-completion
/// notification of the claimed successor, observed on the collection that
/// changes when its session link is published (`AgentMessage`).
#[tokio::test]
async fn failed_background_wake_waits_for_persisted_backoff() {
    let db = trial_store("lifecycle-background-wake-backoff").await;
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
    let backend = ScriptedBackend::start_with_plans(
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
    // Its claim pins the default budget of 3; two prior attempts keep it
    // within budget and inside its backoff window.
    wake.retry_count = 2;
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
    // Both waits read as the Agent that owns the rows, so every term below
    // changes in a transaction that also changes the subscribed collection.
    rows_when(
        &actors,
        "AgentToolCall",
        r#"{ AgentToolCall(filter: {session_id: {_eq: "wake-backoff-session"}, tool_name: {_eq: "bash"}, lifecycle_state: {_eq: "running"}}) {_docID} }"#,
        |data| data["AgentToolCall"].as_array().is_some_and(|rows| !rows.is_empty()),
    ).await;
    runtime
        .backend
        .enqueue_response("continue", StreamResponse::bad_request("provider failed"));
    use tokio::io::AsyncWriteExt;
    release.write_all(b"release\n").await.unwrap();
    rows_when(
        &actors,
        "AgentRequest",
        r#"{
            AgentRequest(filter: {session_id: {_eq: "wake-backoff-session"}}) {request_id lifecycle_state}
            AgentMessage(filter: {session_id: {_eq: "wake-backoff-session"}}) {message_key}
        }"#,
        |data| {
            let requests = data["AgentRequest"].as_array().unwrap();
            let state = |row: &serde_json::Value| {
                RequestLifecycleState::parse_opt(row["lifecycle_state"].as_str())
            };
            let wake = requests
                .iter()
                .find(|row| row["request_id"] == "failed-wake-backoff")
                .and_then(state);
            let successor = requests
                .iter()
                .find(|row| row["request_id"].as_str().is_some_and(|id| id.starts_with("background-completion-")))
                .and_then(state);
            assert!(
                wake.is_none_or(|wake| wake == RequestLifecycleState::Failed || !wake.is_terminal())
                    && successor.is_none_or(|successor| !successor.is_terminal()),
                "the wake must fail and its held successor must stay live: {data}"
            );
            wake == Some(RequestLifecycleState::Failed)
                && successor == Some(RequestLifecycleState::Processing)
                && data["AgentMessage"].as_array().unwrap().iter().any(|row| row["message_key"].as_str().is_some_and(|key| key.starts_with("background-completion-notification:")))
        },
    ).await;
    // The test user sees the notification once its session link is
    // published, which changes AgentMessage and no AgentRequest.
    rows_when(
        &actors.fixture.access,
        "AgentMessage",
        r#"{ AgentMessage(filter: {session_id: {_eq: "wake-backoff-session"}}) {message_key} }"#,
        |data| {
            data["AgentMessage"].as_array().unwrap().iter().any(|row| {
                row["message_key"]
                    .as_str()
                    .is_some_and(|key| key.starts_with("background-completion-notification:"))
            })
        },
    )
    .await;
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
                    retry_parent_request retry_parent_request_doc_id retry_root_request retry_count
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
