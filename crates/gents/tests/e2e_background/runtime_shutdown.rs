use crate::support::accepted_turn::{
    boot_prepared_accepted_turn, prepare_managed_accepted_turn, AcceptedTurnRuntime,
    AcceptedTurnSpec,
};
use crate::support::fixtures::configure_agent_tools_as;
use crate::support::live_inference::wait_for_request_terminal;
use crate::support::trial_store;
use gents_inference::test_support::scripted_backend::StreamChunk;
use gents_protocol::tool_call::CancelCause;
use gents_store::graphql::escape_graphql_string;
use gents_store::test_support::rows_when;
use gents_store::test_support::STALL_CEILING;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

const R6_BACKEND_ID: &str = "r6-background-backend";
const R6_MODEL: &str = "test-model";

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    tool_call_id: Option<String>,
    lifecycle_state: Option<String>,
    cancel_cause: Option<String>,
}

struct AcceptedBackgroundTurn {
    db: gents_store::test_support::TestStore,
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
    let db = trial_store(test_name).await;
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
            backend_admission: Some(db.backend_admission()),
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

/// `Some(true)` while this test's own child is alive, `Some(false)` once it
/// has exited, and `None` once its managed owner reaped it. `WNOWAIT` leaves
/// the status for that owner, and an unreaped pid cannot have been recycled,
/// so an answer here always describes the process this test spawned.
#[cfg(unix)]
fn observe_own_child(pid: i32) -> Option<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: a non-blocking, non-consuming wait on this process's own child.
    let observed = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if observed == -1 {
        return None;
    }
    Some(unsafe { info.si_pid() } == 0)
}

/// Ends the test's own spawned child if an assertion unwinds before the
/// shutdown under test has stopped it.
#[cfg(unix)]
struct SpawnedChildGuard(i32);

#[cfg(unix)]
impl Drop for SpawnedChildGuard {
    fn drop(&mut self) {
        if observe_own_child(self.0) == Some(true) {
            // SAFETY: the pid was just observed as this process's own unreaped
            // child, so it cannot name anything else.
            unsafe { libc::kill(self.0, libc::SIGKILL) };
        }
    }
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
    let pid_path = directory.path().join("pid");
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
                            r#"trap '' TERM; printf '%s' $$ > "$1"; read ignored < "$2""#,
                            "gents-shutdown-probe",
                            pid_path.to_string_lossy(),
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
    let pid: i32 = tokio::time::timeout(STALL_CEILING, async {
        loop {
            if let Ok(recorded) = tokio::fs::read_to_string(&pid_path).await {
                if let Ok(pid) = recorded.trim().parse() {
                    return pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the background child installs its TERM trap and records its pid");
    let guard = SpawnedChildGuard(pid);
    assert_eq!(observe_own_child(pid), Some(true));

    turn.runtime.shutdown().await;

    assert_ne!(
        observe_own_child(pid),
        Some(true),
        "runtime shutdown must terminate the background process it owns"
    );
    drop(guard);

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
