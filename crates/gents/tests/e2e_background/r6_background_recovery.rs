use serde::{Deserialize, Serialize};

use crate::support::accepted_turn::{boot_prepared_accepted_turn, AcceptedTurnSpec};
use crate::support::fixtures::configure_agent_tools;
use crate::support::streaming_backend::StreamChunk;
use crate::support::test_db_in;

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    lifecycle_state: Option<String>,
    cancel_cause: Option<String>,
}

/// Reads as the working Agent, which owns its tool calls under document ACP.
async fn wait_for_running_tool(access: &gents::ConfigAccess, session_id: &str) -> String {
    let session_id = gents::store::graphql::escape_graphql_string(session_id);
    for _ in 0..100 {
        let response = access
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }}, tool_name: {{ _eq: "bash" }}, lifecycle_state: {{ _eq: "running" }} }}, limit: 1) {{ tool_call_id }} }}"#
            ))
            .await
            .expect("read running background tool as its Agent");
        if let Some(id) = response
            .get("data")
            .and_then(|data| data.get("AgentToolCall"))
            .and_then(serde_json::Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("tool_call_id"))
            .and_then(serde_json::Value::as_str)
        {
            return id.to_string();
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("background bash never reached running");
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
}

const CRASH_WORKER_READY: &str = "GENTS_R6_CRASH_WORKER_READY";

async fn run_recovery_crash_worker(ready_path: &std::path::Path) {
    let data_dir = tempfile::Builder::new()
        .prefix("worker-data-")
        .tempdir_in(ready_path.parent().expect("handshake parent"))
        .unwrap();
    let db = test_db_in(data_dir).await;
    let node_did = db.node_identity.did().to_string();
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let agent_binding = fixture.binding.clone();
    let agent_did = agent_binding.agent_did.as_str();
    let agent_access = gents::ConfigAccess::local_managed_agent(
        db.node.clone(),
        agent_did.parse().expect("valid Agent DID"),
        fixture.agent.as_ref(),
    )
    .expect("managed worker Agent access");
    let session_id = "r6-recovery-session";
    let spec = AcceptedTurnSpec {
        backend_id: "r6-recovery-backend",
        model: "test-model",
        parent_agent_did: agent_did,
        configured_agent_dids: &[agent_did],
        request_id: "r6-recovery-parent",
        session_id,
        prompt: "start recovery background process",
        accepted_chunks: vec![StreamChunk::tool_call(
            "r6-recovery-spawn",
            "spawn_process",
            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
        )],
        child_plans: Vec::new(),
        valid_until: None,
        request_hop: None,
        request_setup: None,
    };
    let prepared =
        crate::support::accepted_turn::prepare_managed_accepted_turn(&db, &fixture, spec).await;
    prepared
        .backend
        .enable_dynamic_followups("start recovery background process");
    configure_agent_tools(
        &db,
        &node_did,
        agent_did,
        None,
        gents::documents::Tools {
            tools_id: format!("{agent_did}:tools"),
            node_did: node_did.clone(),
            host: Some(gents::documents::HostTools {
                bash: Some(gents::documents::BashTools {
                    mode: gents::BashMode::ReadOnly,
                    read_only_commands: Some(vec!["sleep".to_string()]),
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
    let identity: std::sync::Arc<dyn gents::NodeIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: gents::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .expect("build recovery crash worker runtime")
    .with_background_process_records(db.data_path().join("background-processes"));
    let _runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    let tool_call_id = wait_for_running_tool(&agent_access, session_id).await;
    let parent = agent_access
        .execute(
            r#"{ AgentRequest(filter: { request_id: { _eq: "r6-recovery-parent" } }, limit: 1) { lifecycle_state } }"#,
        )
        .await
        .expect("load live recovery parent");
    assert_eq!(
        parent["data"]["AgentRequest"][0]["lifecycle_state"].as_str(),
        Some("processing"),
        "crash recovery fixture must stop with a live parent: {parent}"
    );
    let ready = CrashWorkerReady {
        data_path: db.data_path().to_path_buf(),
        node_did,
        agent_binding,
        session_id: session_id.to_string(),
        tool_call_id,
    };
    let pending = ready_path.with_extension("pending");
    std::fs::write(&pending, serde_json::to_vec(&ready).unwrap()).unwrap();
    std::fs::rename(pending, ready_path).unwrap();
    std::future::pending::<()>().await;
}

async fn load_tool_call(access: &gents::ConfigAccess, tool_call_id: &str) -> ToolCallRow {
    let tool_call_id = gents::store::graphql::escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{tool_call_id}" }} }}, limit: 1) {{
                lifecycle_state
                cancel_cause
            }}
        }}"#
    );
    let response = access.execute(&query).await.expect("read recovered tool call");
    serde_json::from_value(response["data"]["AgentToolCall"][0].clone())
        .expect("one recovered tool call")
}

async fn load_messages(access: &gents::ConfigAccess, session_id: &str) -> Vec<MessageRow> {
    let session_id = gents::store::graphql::escape_graphql_string(session_id);
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
        let (_, message) = gents::session::load_canonical_message(
            access,
            header["_docID"].as_str().expect("message header _docID"),
            header["node_did"].as_str().expect("message header agent"),
            header["requester_did"].as_str(),
        )
        .await
        .expect("reconstruct recovery notification");
        let content = match message {
            gents::inference::llm::message::Message::System { content } => content,
            gents::inference::llm::message::Message::User { content } => content
                .into_iter()
                .filter_map(|item| match item {
                    gents::inference::llm::message::UserContent::Text(text) => Some(text.text),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            gents::inference::llm::message::Message::Assistant { content, .. } => content
                .into_iter()
                .filter_map(|item| match item {
                    gents::inference::llm::message::AssistantContent::Text(text) => Some(text.text),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        messages.push(MessageRow { content });
    }
    messages
}

async fn load_wakes(access: &gents::ConfigAccess, session_id: &str) -> Vec<serde_json::Value> {
    let session_id = gents::store::graphql::escape_graphql_string(session_id);
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
    let response = access
        .execute(&query)
        .await
        .expect("load recovery wakes");
    serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap_or_default()
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks existence of this test's own child.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(unix)]
#[tokio::test]
async fn recover_all_interrupts_backgrounded_running_tool_with_live_parent() {
    if let Some(ready_path) = std::env::var_os(CRASH_WORKER_READY) {
        run_recovery_crash_worker(std::path::Path::new(&ready_path)).await;
        unreachable!();
    }
    let handshake = tempfile::tempdir().unwrap();
    let ready_path = handshake.path().join("ready.json");
    struct ChildGuard(Option<std::process::Child>);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("r6_background_recovery::recover_all_interrupts_backgrounded_running_tool_with_live_parent")
        .arg("--nocapture")
        .env(CRASH_WORKER_READY, &ready_path)
        .spawn()
        .expect("spawn recovery crash worker");
    let mut child = ChildGuard(Some(child));
    let ready = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
                panic!("recovery crash worker exited before readiness: {status}");
            }
            if let Ok(bytes) = std::fs::read(&ready_path) {
                break serde_json::from_slice::<CrashWorkerReady>(&bytes).unwrap();
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("crash worker readiness");
    let mut killed = child.0.take().unwrap();
    killed.kill().expect("kill recovery crash worker");
    killed.wait().expect("reap recovery crash worker");

    let reopened_identity =
        gents::KeyIdentity::load_or_create(ready.data_path.join("node.key"), None)
            .expect("reload crashed worker signing identity");
    assert_eq!(
        gents::NodeIdentity::did(&reopened_identity),
        ready.node_did,
        "persisted crash-worker key must retain the exact node principal"
    );
    let agent_identity =
        gents::store::identity::resolve_agent_identity(&reopened_identity, &ready.agent_binding)
            .expect("reload crashed worker's managed Agent identity");
    // Reopen through the home owner: the store is protected by its home key
    // and lives under the home's data directory.
    let home = gents::eval::runner::embedded::EmbeddedHome::open_retained(&ready.data_path)
        .await
        .expect("reopen crashed recovery home");
    assert_eq!(home.did(), ready.node_did);
    let node = home.node.clone();
    let records = ready.data_path.join("background-processes");
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            std::fs::read_dir(&records)
                .expect("crashed worker left a process record")
                .map(|entry| entry.unwrap().path())
                .find(|path| path.extension().is_some_and(|ext| ext == "json"))
                .expect("process record file"),
        )
        .unwrap(),
    )
    .unwrap();
    let orphan_pid = record["identity"]["pid"].as_i64().unwrap() as i32;
    assert!(
        process_alive(orphan_pid),
        "the spawned process must survive its runtime's crash"
    );
    let executions = gents::BackgroundExecutionRegistry::default().with_process_records(records);
    let report = gents::tools::call::lifecycle::ToolCallLifecycle::recover_all_with_executions(
        &node,
        &ready.node_did,
        std::slice::from_ref(&agent_identity),
        &executions,
    )
    .await
    .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);
    let stopped = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while process_alive(orphan_pid) {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    assert!(
        stopped.is_ok(),
        "restart recovery must stop the owned orphan"
    );

    // The recovered rows belong to the working Agent; read them as it.
    let agent_access = gents::ConfigAccess::local_managed_agent(
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
