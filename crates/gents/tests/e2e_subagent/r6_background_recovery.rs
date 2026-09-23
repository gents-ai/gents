//! R6 background-tool recovery tests.

use serde::{Deserialize, Serialize};

use crate::support::accepted_turn::{boot_prepared_accepted_turn, AcceptedTurnSpec};
use crate::support::fixtures::configure_behavior_tools;
use crate::support::streaming_backend::StreamChunk;
use crate::support::{first_row, test_db_in};

#[derive(Debug, Deserialize)]
struct ToolCallRow {
    tool_name: Option<String>,
    await_mode: Option<String>,
    request_id: Option<String>,
    deadline_at: Option<String>,
    lifecycle_state: Option<String>,
    cancel_cause: Option<String>,
    tool_failure_class: Option<String>,
}

async fn wait_for_running_tool(node: &gents::defra_node::EmbeddedNode, session_id: &str) -> String {
    let session_id = gents::graphql::escape_graphql_string(session_id);
    for _ in 0..100 {
        let response = node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }}, tool_name: {{ _eq: "bash" }}, lifecycle_state: {{ _eq: "running" }} }}, limit: 1) {{ tool_call_id }} }}"#
            ))
            .await;
        if let Some(id) = response
            .data
            .as_ref()
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
    agent_did: String,
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
    let agent_did = db.node_identity.did().to_string();
    let behavior_id = "r6-background-recovery";
    let session_id = "r6-recovery-session";
    let spec = AcceptedTurnSpec {
        backend_id: "r6-recovery-backend",
        model: "test-model",
        parent_behavior_id: behavior_id,
        configured_behavior_ids: &[behavior_id],
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
        subagent_depth: None,
        request_setup: None,
    };
    let prepared = crate::support::accepted_turn::prepare_accepted_turn(&db, spec).await;
    // Keep the accepted parent live after the spawn receipt. The recovery
    // contract distinguishes a live parent from a cleanly completed one; an
    // unpaused second provider response would race the crash and exercise the
    // terminal-parent failure branch instead.
    prepared
        .backend
        .enable_dynamic_followups("start recovery background process");
    configure_behavior_tools(
        db.node.as_ref(),
        &agent_did,
        behavior_id,
        None,
        gents::document_config::Tools {
            tools_id: format!("{behavior_id}:tools"),
            agent_did: agent_did.clone(),
            host: Some(gents::document_config::HostTools {
                bash: Some(gents::document_config::BashTools {
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
    let identity: std::sync::Arc<dyn gents::AgentIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            tool_ceiling: gents::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .expect("build recovery crash worker runtime");
    let _runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    let tool_call_id = wait_for_running_tool(db.node.as_ref(), session_id).await;
    let parent = db
        .node
        .execute(
            r#"{ AgentRequest(filter: { request_id: { _eq: "r6-recovery-parent" } }, limit: 1) { lifecycle_state } }"#,
        )
        .await;
    assert!(
        !parent.has_errors(),
        "load live recovery parent: {:?}",
        parent.errors
    );
    assert_eq!(
        parent.data.as_ref().and_then(|data| {
            data["AgentRequest"]
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row["lifecycle_state"].as_str())
        }),
        Some("processing"),
        "crash recovery fixture must stop with a live parent: {:?}",
        parent.data
    );
    let ready = CrashWorkerReady {
        data_path: db.data_path().to_path_buf(),
        agent_did,
        session_id: session_id.to_string(),
        tool_call_id,
    };
    let pending = ready_path.with_extension("pending");
    std::fs::write(&pending, serde_json::to_vec(&ready).unwrap()).unwrap();
    std::fs::rename(pending, ready_path).unwrap();
    std::future::pending::<()>().await;
}

async fn load_tool_call(node: &gents::defra_node::EmbeddedNode, tool_call_id: &str) -> ToolCallRow {
    let tool_call_id = gents::graphql::escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{tool_call_id}" }} }}, limit: 1) {{
                lifecycle_state
                tool_name
                await_mode
                request_id
                deadline_at
                cancel_cause
                tool_failure_class
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentToolCall")
}

async fn load_messages(
    node: &gents::defra_node::EmbeddedNode,
    session_id: &str,
) -> Vec<MessageRow> {
    let session_id = gents::graphql::escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{ _docID agent_did requester_did }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "load recovery messages failed: {:?}",
        response.errors
    );
    let headers = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut messages = Vec::new();
    for header in headers {
        let (_, message) = gents::session::load_canonical_message_from_node(
            node,
            header["_docID"].as_str().expect("message header _docID"),
            header["agent_did"].as_str().expect("message header agent"),
            header["requester_did"].as_str(),
        )
        .await
        .expect("reconstruct recovery notification");
        let content = match message {
            gents::llm::message::Message::System { content } => content,
            gents::llm::message::Message::User { content } => content
                .into_iter()
                .filter_map(|item| match item {
                    gents::llm::message::UserContent::Text(text) => Some(text.text),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            gents::llm::message::Message::Assistant { content, .. } => content
                .into_iter()
                .filter_map(|item| match item {
                    gents::llm::message::AssistantContent::Text(text) => Some(text.text),
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
    node: &gents::defra_node::EmbeddedNode,
    session_id: &str,
) -> Vec<serde_json::Value> {
    let session_id = gents::graphql::escape_graphql_string(session_id);
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
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "load recovery wakes failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

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
        gents::AgentIdentity::did(&reopened_identity),
        ready.agent_did,
        "persisted crash-worker key must retain the exact node principal"
    );
    let node = std::sync::Arc::new(
        gents::defra_node::EmbeddedNode::builder()
            .data_path(&ready.data_path)
            .with_node_identity_did(&ready.agent_did)
            .build()
            .await
            .expect("reopen crashed recovery store"),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let report =
        gents::tool_call_lifecycle::ToolCallLifecycle::recover_all(&node, &ready.agent_did)
            .await
            .unwrap();
    assert_eq!(report.tool_calls_recovered, 1);

    let row = load_tool_call(node.as_ref(), &ready.tool_call_id).await;
    assert_eq!(
        row.lifecycle_state.as_deref(),
        Some("cancelled"),
        "unexpected recovered row: {row:?}"
    );
    assert_eq!(row.cancel_cause.as_deref(), Some("interrupted"));
    let messages = load_messages(node.as_ref(), &ready.session_id).await;
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

    let wakes = load_wakes(node.as_ref(), &ready.session_id).await;
    assert_eq!(
        wakes.len(),
        1,
        "recovery notification should enqueue one resumable agent turn"
    );
}
