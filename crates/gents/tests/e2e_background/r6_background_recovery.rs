use serde::{Deserialize, Serialize};

use crate::support::agent_requests::submit_user_request;
use crate::support::fixtures::configure_agent_tools_as;
use crate::support::interrupt::spawn_runtime;
use crate::support::live_inference::{bind_target, live_target, wait_for_rows};
use crate::support::test_db_in;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    lifecycle_state: Option<String>,
    cancel_cause: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageRow {
    content: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct CrashWorkerReady {
    data_path: std::path::PathBuf,
    node_did: String,
    agent_binding: gents_protocol::agent_identity::AgentIdentityBinding,
    session_id: String,
    tool_call_id: String,
    process: gents_session::exec::ownership::ProcessIdentity,
}

const CRASH_WORKER_READY: &str = "GENTS_R6_CRASH_WORKER_READY";

#[cfg(unix)]
async fn run_recovery_crash_worker(ready_path: &std::path::Path) {
    let data_parent = ready_path.parent().unwrap().to_owned();
    let data_dir = tokio::task::spawn_blocking(move || {
        tempfile::Builder::new()
            .prefix("worker-data-")
            .tempdir_in(data_parent)
    })
    .await
    .unwrap()
    .unwrap();
    let db = test_db_in(data_dir).await;
    let target = live_target().await;
    let actors = bind_target(&db, &target).await;
    let node_did = actors.node_did().to_owned();
    let session_id = "r6-recovery-session";
    let root = ready_path.parent().unwrap();
    configure_agent_tools_as(
        &actors.fixture.access, &node_did, actors.agent_did(),
        Some("Follow the requested two tool steps. Start the background process, then wait on its returned handle. Do not start other work.".into()),
        gents_documents::Tools {
            tools_id: "live-crash-recovery-tools".into(), node_did: node_did.clone(),
            host: Some(gents_documents::HostTools {
                root: Some(root.display().to_string()),
                bash: Some(gents_documents::BashTools {
                    mode: gents_documents::BashMode::ReadOnly,
                    read_only_commands: Some(vec!["sh".into()]),
                    background_enabled: true,
                    ..Default::default()
                }), ..Default::default()
            }), ..Default::default()
        }, Vec::new(),
    ).await;
    let agent = gents::Gents::from_default_agent_documents(
        db.node.clone(),
        db.node_identity.clone(),
        gents::DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: gents_tools::surface::ToolCeiling::readonly_at(root),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .with_background_process_records(db.data_path().join("background-processes"));
    let executions = agent.background_execution_registry();
    let _runtime = spawn_runtime(&db, agent).await;
    let arguments = serde_json::json!({"tool_name":"bash", "args":{"command":"sh", "args":["-c", "exec 3< \"$1\"; printf READY; IFS= read -r release <&3", "gents-live-crash", root.join("release").display().to_string()]}});
    submit_user_request(&node_did, &actors.fixture, session_id, "r6-recovery-parent",
        &format!("Call spawn_process with exactly these arguments: {arguments}. Then call wait_process using the returned tool_call_id and timeout_secs 120. Wait for that result before answering."), Default::default()).await;
    let running = wait_for_rows(&actors.fixture.access, "AgentToolCall",
        r#"{ AgentToolCall(filter: {request_id: {_eq: "r6-recovery-parent"}, lifecycle_state: {_eq: "running"}}) {tool_call_id tool_name} }"#,
        Duration::from_secs(180), |rows| {
            let calls = rows["AgentToolCall"].as_array().unwrap();
            calls.iter().any(|row| row["tool_name"] == "bash") && calls.iter().any(|row| row["tool_name"] == "wait_process")
        }).await;
    let tool_call_id = running["AgentToolCall"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["tool_name"] == "bash")
        .unwrap()["tool_call_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let record_id = tool_call_id.clone();
    let process = tokio::task::spawn_blocking(move || {
        executions
            .try_process_record_list()
            .unwrap()
            .into_iter()
            .find(|record| record.tool_call_id == record_id)
            .expect("running background process record")
            .identity
    })
    .await
    .unwrap();
    let ready = CrashWorkerReady {
        data_path: db.data_path().to_path_buf(),
        node_did,
        agent_binding: actors.fixture.binding.clone(),
        session_id: session_id.into(),
        tool_call_id,
        process,
    };
    let mut socket = tokio::net::UnixStream::connect(ready_path).await.unwrap();
    socket
        .write_all(&serde_json::to_vec(&ready).unwrap())
        .await
        .unwrap();
    socket.shutdown().await.unwrap();
    std::future::pending::<()>().await;
}

async fn load_tool_call(
    access: &gents_store::config_client::ConfigAccess,
    tool_call_id: &str,
) -> ToolCallRow {
    let tool_call_id = gents_store::graphql::escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{tool_call_id}" }} }}, limit: 1) {{
                lifecycle_state
                cancel_cause
            }}
        }}"#
    );
    let response = access
        .execute(&query)
        .await
        .expect("read recovered tool call");
    serde_json::from_value(response["data"]["AgentToolCall"][0].clone())
        .expect("one recovered tool call")
}

async fn load_messages(
    access: &gents_store::config_client::ConfigAccess,
    session_id: &str,
) -> Vec<MessageRow> {
    let session_id = gents_store::graphql::escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{ _docID node_did requester_did }}
        }}"#
    );
    let response = access
        .execute(&query)
        .await
        .expect("load recovery messages");
    let headers = response["data"]["AgentMessage"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut messages = Vec::new();
    for header in headers {
        let (_, message) = gents_session::session::load_canonical_message(
            access,
            header["_docID"].as_str().expect("message header _docID"),
            header["node_did"].as_str().expect("message header agent"),
            header["requester_did"].as_str(),
        )
        .await
        .expect("reconstruct recovery notification");
        let content = match message {
            gents_inference::llm::message::Message::System { content } => content,
            gents_inference::llm::message::Message::User { content } => content
                .into_iter()
                .filter_map(|item| match item {
                    gents_inference::llm::message::UserContent::Text(text) => Some(text.text),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            gents_inference::llm::message::Message::Assistant { content, .. } => content
                .into_iter()
                .filter_map(|item| match item {
                    gents_inference::llm::message::AssistantContent::Text(text) => Some(text.text),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        messages.push(MessageRow { content });
    }
    messages
}

async fn load_wakes(
    access: &gents_store::config_client::ConfigAccess,
    session_id: &str,
) -> Vec<serde_json::Value> {
    let session_id = gents_store::graphql::escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    execution_origin: {{ _eq: "scheduled" }}
                }}
            ) {{ input }}
        }}"#
    );
    let response = access.execute(&query).await.expect("load recovery wakes");
    serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap_or_default()
}

#[cfg(unix)]
#[tokio::test]
async fn recover_all_interrupts_backgrounded_running_tool_with_live_parent() {
    if let Some(ready_path) = std::env::var_os(CRASH_WORKER_READY) {
        run_recovery_crash_worker(std::path::Path::new(&ready_path)).await;
        unreachable!();
    }
    let handshake = tokio::task::spawn_blocking(|| {
        tempfile::Builder::new()
            .prefix("gents-r6-")
            .tempdir_in("/tmp")
    })
    .await
    .unwrap()
    .unwrap();
    let ready_path = handshake.path().join("ready.sock");
    let listener = tokio::net::UnixListener::bind(&ready_path).unwrap();
    let fifo = handshake.path().join("release");
    assert!(tokio::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .await
        .unwrap()
        .success());
    let _gate = tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(fifo)
    })
    .await
    .unwrap()
    .unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("r6_background_recovery::recover_all_interrupts_backgrounded_running_tool_with_live_parent")
        .arg("--nocapture")
        .env(CRASH_WORKER_READY, &ready_path)
        .kill_on_drop(true)
        .spawn().expect("spawn live recovery crash worker");
    let ready = tokio::time::timeout(Duration::from_secs(240), async {
        tokio::select! {
            accepted = listener.accept() => {
                let (mut socket, _) = accepted.unwrap();
                let mut bytes = Vec::new();
                socket.read_to_end(&mut bytes).await.unwrap();
                serde_json::from_slice::<CrashWorkerReady>(&bytes).unwrap()
            }
            status = child.wait() => panic!("live recovery crash worker exited before readiness: {status:?}"),
        }
    }).await.expect("live crash worker readiness deadline");
    child
        .kill()
        .await
        .expect("kill and reap live recovery crash worker");

    let key_path = ready.data_path.join("node.key");
    let reopened_identity = tokio::task::spawn_blocking(move || {
        gents_store::identity::KeyIdentity::load_or_create(key_path, None)
    })
    .await
    .unwrap()
    .expect("reload crashed worker signing identity");
    assert_eq!(
        gents_store::identity::NodeIdentity::did(&reopened_identity),
        ready.node_did,
        "persisted crash-worker key must retain the exact node principal"
    );
    let agent_identity =
        gents_store::identity::resolve_agent_identity(&reopened_identity, &ready.agent_binding)
            .expect("reload crashed worker's managed Agent identity");
    // Reopen through the home owner: the store is protected by its home key
    // and lives under the home's data directory.
    let home = gents::runtime::embedded::EmbeddedHome::open_retained(&ready.data_path)
        .await
        .expect("reopen crashed recovery home");
    assert_eq!(home.did(), ready.node_did);
    let node = home.node.clone();
    let records = ready.data_path.join("background-processes");
    let process = ready.process.clone();
    assert_eq!(
        tokio::task::spawn_blocking(move || process.observe())
            .await
            .unwrap(),
        gents_session::exec::ownership::ProcessObservation::Running,
        "the spawned process must survive its runtime's crash"
    );
    let executions = gents_toolcalls::call::hook::BackgroundExecutionRegistry::default()
        .with_process_records(records);
    gents_toolcalls::call::lifecycle::ToolCallLifecycle::recover_all_with_executions(
        &node,
        &ready.node_did,
        std::slice::from_ref(&agent_identity),
        &executions,
    )
    .await
    .unwrap();
    let process = ready.process.clone();
    assert_eq!(
        tokio::task::spawn_blocking(move || process.observe())
            .await
            .unwrap(),
        gents_session::exec::ownership::ProcessObservation::Exited,
        "restart recovery must stop the owned orphan"
    );

    // The recovered rows belong to the working Agent; read them as it.
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent_identity.did().parse().expect("valid Agent DID"),
        agent_identity.as_ref(),
    )
    .expect("recovered Agent access");
    let row = load_tool_call(&agent_access, &ready.tool_call_id).await;
    assert_eq!(
        row.lifecycle_state.as_deref(),
        Some("cancelled"),
        "unexpected recovered row: {row:?}"
    );
    assert_eq!(row.cancel_cause.as_deref(), Some("interrupted"));
    let messages = load_messages(&agent_access, &ready.session_id).await;
    let completions = messages
        .iter()
        .filter(|message| {
            message.content.contains(&format!(
                r#"<tool-completion tool_call_id="{}""#,
                ready.tool_call_id
            ))
        })
        .collect::<Vec<_>>();
    assert_eq!(completions.len(), 1);
    assert!(completions[0].content.contains(r#"status="cancelled""#));
    assert!(completions[0]
        .content
        .contains("<reason>interrupted_on_restart</reason>"));

    let wakes = load_wakes(&agent_access, &ready.session_id).await;
    assert_eq!(
        wakes.len(),
        1,
        "recovery notification should enqueue one resumable agent turn"
    );
}
