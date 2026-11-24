use crate::support::accepted_turn::{
    boot_prepared_accepted_turn, prepare_managed_accepted_turn, AcceptedTurnRuntime,
    AcceptedTurnSpec,
};
use crate::support::fixtures::configure_agent_tools_as;
use crate::support::live_inference::wait_for_request_terminal;
use crate::support::test_db;
use gents_inference::test_support::scripted_backend::StreamChunk;
use gents_protocol::tool_call::CancelCause;
use gents_store::graphql::escape_graphql_string;
use gents_store::test_support::rows_when;
use gents_store::test_support::STALL_CEILING;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

const R6_BACKEND_ID: &str = "r6-background-backend";
const R6_MODEL: &str = "test-model";

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    tool_call_id: Option<String>,
    lifecycle_state: Option<String>,
    cancel_cause: Option<String>,
}

struct AcceptedBackgroundTurn {
    db: crate::support::TestDb,
    fixture: crate::support::ManagedAgentFixture,
    runtime: AcceptedTurnRuntime,
    session_id: String,
    request_id: String,
    prompt: String,
}

async fn boot_background_turn(
    test_name: &str,
    accepted_chunks: Vec<StreamChunk>,
) -> AcceptedBackgroundTurn {
    let db = test_db(test_name).await;
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let agent_identity = fixture.agent.clone();
    let agent_did = agent_identity.did();
    let session_id = format!("{test_name}-session");
    let request_id = format!("{test_name}-request");
    let prompt = format!("{test_name}-prompt");
    let prepared = prepare_managed_accepted_turn(
        &db,
        &fixture,
        AcceptedTurnSpec {
            backend_id: R6_BACKEND_ID,
            model: R6_MODEL,
            parent_agent_did: agent_did,
            configured_agent_dids: &[agent_did],
            request_id: &request_id,
            session_id: &session_id,
            prompt: &prompt,
            accepted_chunks,
            child_plans: Vec::new(),
            valid_until: None,
            request_hop: None,
            request_setup: None,
        },
    )
    .await;
    prepared.backend.enable_dynamic_followups(&prompt);
    configure_agent_tools_as(
        &fixture.access,
        db.node_identity.did(),
        agent_did,
        None,
        gents_documents::Tools {
            tools_id: format!("{agent_did}:tools"),
            node_did: db.node_identity.did().to_string(),
            host: Some(gents_documents::HostTools {
                bash: Some(gents_documents::BashTools {
                    mode: gents_documents::BashMode::ReadOnly,
                    read_only_commands: Some(vec![
                        "sleep".to_string(),
                        "printf".to_string(),
                        "sh".to_string(),
                    ]),
                    background_enabled: true,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    let identity: Arc<dyn gents_store::identity::NodeIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: gents_tools::surface::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .expect("build accepted background runtime");
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    AcceptedBackgroundTurn {
        db,
        fixture,
        runtime,
        session_id,
        request_id,
        prompt,
    }
}

fn first_tool_call(data: &Value) -> Option<ToolCallRow> {
    data["AgentToolCall"]
        .as_array()
        .and_then(|rows| rows.first())
        .map(|row| serde_json::from_value(row.clone()).expect("decode AgentToolCall row"))
}

async fn tool_call_when<F>(
    access: &gents_store::config_client::ConfigAccess,
    session_id: &str,
    selector: (&str, &str),
    predicate: F,
) -> ToolCallRow
where
    F: Fn(&ToolCallRow) -> bool,
{
    let session_id = escape_graphql_string(session_id);
    let (field, value) = selector;
    let value = escape_graphql_string(value);
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }}, {field}: {{ _eq: "{value}" }} }}, limit: 1) {{ tool_call_id lifecycle_state cancel_cause }} }}"#
    );
    let data = rows_when(access, "AgentToolCall", &query, |data| {
        first_tool_call(data).is_some_and(|row| predicate(&row))
    })
    .await;
    first_tool_call(&data).expect("AgentToolCall row")
}

fn finish_dynamic_turn(turn: &AcceptedBackgroundTurn) {
    turn.runtime.backend.enqueue_response(
        &turn.prompt,
        gents_inference::test_support::scripted_backend::StreamResponse::completes(
            &turn.prompt,
            ["parent complete"],
        ),
    );
}

/// Opens the writing end of `gate`, which returns once a reader holds it
/// open. Seatbelt's read-only profile lets the probe read the FIFO but not
/// write anywhere, so its reading open is its only signal of readiness.
#[cfg(unix)]
async fn open_gate_writer(gate: std::path::PathBuf) -> std::fs::File {
    tokio::time::timeout(
        STALL_CEILING,
        tokio::task::spawn_blocking(move || std::fs::OpenOptions::new().write(true).open(gate)),
    )
    .await
    .expect("the background probe opens its gate")
    .expect("join the gate open")
    .expect("open the gate for writing")
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_shutdown_terminates_a_spawned_background_process() {
    let turn = boot_background_turn("r6-background-shutdown-stop", Vec::new()).await;
    finish_dynamic_turn(&turn);
    assert_eq!(
        wait_for_request_terminal(&turn.fixture.access, &turn.request_id).await,
        "completed",
        "initial accepted request did not complete"
    );
    let directory = tempfile::tempdir().unwrap();
    let gate = directory.path().join("gate");
    assert!(tokio::process::Command::new("mkfifo")
        .arg(&gate)
        .status()
        .await
        .unwrap()
        .success());
    let prompt = "start a process that must not outlive this runtime";
    turn.runtime.backend.enable_dynamic_followups(prompt);
    turn.runtime.backend.enqueue_response(
        prompt,
        gents_inference::test_support::scripted_backend::StreamResponse::streams(
            prompt,
            vec![StreamChunk::tool_call(
                "meta-bg-shutdown",
                "spawn_process",
                serde_json::json!({
                    "tool_name": "bash",
                    "args": {
                        "command": "sh",
                        "args": [
                            "-c",
                            r#"trap '' TERM; read ignored < "$1""#,
                            "gents-shutdown-probe",
                            gate.to_string_lossy()
                        ]
                    }
                })
                .to_string(),
            )],
        ),
    );
    turn.runtime.backend.enqueue_response(
        prompt,
        gents_inference::test_support::scripted_backend::StreamResponse::completes(
            prompt,
            ["started"],
        ),
    );
    crate::support::accepted_turn::enqueue_managed_accepted_request(
        &turn.db,
        &turn.fixture,
        "r6-background-shutdown-stop-request-2",
        &turn.session_id,
        prompt,
    )
    .await;
    let row = tool_call_when(
        &turn.fixture.access,
        &turn.session_id,
        ("tool_name", "bash"),
        |_| true,
    )
    .await;
    let tool_call_id = row.tool_call_id.expect("background handle");
    let mut gate = open_gate_writer(gate).await;

    turn.runtime.shutdown().await;

    use std::io::Write;
    let written = gate.write_all(b"outlived\n");
    assert_eq!(
        written.map_err(|error| error.kind()),
        Err(std::io::ErrorKind::BrokenPipe),
        "runtime shutdown must terminate the background process it owns"
    );

    let settled = tool_call_when(
        &turn.fixture.access,
        &turn.session_id,
        ("tool_call_id", tool_call_id.as_str()),
        |row| row.lifecycle_state.as_deref() != Some("running"),
    )
    .await;
    assert_eq!(settled.lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(
        settled
            .cancel_cause
            .as_deref()
            .and_then(CancelCause::from_persisted),
        Some(CancelCause::Interrupted)
    );
}
