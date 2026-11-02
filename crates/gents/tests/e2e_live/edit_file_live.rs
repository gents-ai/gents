use std::sync::Arc;
use std::time::Duration;

use gents::documents::{FileTools, HostTools, Tools};
use gents::NodeIdentity;
use gents::{DocumentRuntimeOptions, FileToolMode, Gents, ToolCeiling};

use crate::support::fixtures::{configure_agent_tools, test_identity};
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::live_inference::{bind_target, live_target, wait_for_request_terminal};
use crate::support::snapshots::fetch_tool_call_payloads_for_request;
use crate::support::test_db;

const SEED: &str = "{\r\n  \"max_turns\": 20,  \r\n  \"model_name\": \"fixture-model\"\r\n}\r\n";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: set GENTS_EVAL_TARGET and pass --ignored"]
async fn edit_file_live_model_lands_drifted_edit_without_write_file() {
    let target = live_target();

    let db = test_db("edit-file-live").await;
    let identity: Arc<dyn NodeIdentity> = Arc::new(test_identity("edit-file-live"));

    let workspace = tempfile::tempdir().expect("workspace tempdir");
    std::fs::write(workspace.path().join("profile.json"), SEED).unwrap();

    let (node_did, agent_id) = bind_target(&db.node, identity.as_ref(), &target).await;

    configure_agent_tools(
        &db.node,
        &node_did,
        &agent_id,
        None,
        Tools {
            tools_id: "edit-live-tools".to_string(),
            node_did: node_did.clone(),
            host: Some(HostTools {
                root: Some(workspace.path().display().to_string()),
                files: Some(FileTools {
                    mode: FileToolMode::ReadWrite,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;

    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        Arc::clone(&identity),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readwrite(workspace.path()),
            ..Default::default()
        },
    )
    .await
    .expect("boot agent");
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(&db.node, &node_did).await;
    let booted = BootedAgent::new(shutdown_tx, handle, node_did.clone());

    let request_id = "edit-file-live-req-1";
    create_runtime_request(
        &db.node,
        &node_did,
        &agent_id,
        request_id,
        "edit-file-live-session-1",
        "In profile.json, change max_turns from 20 to 250. Use read_file first, \
         then make the change with edit_file, passing the content_hash from the \
         read as expected_content_hash. Do not use write_file. Reply DONE when \
         the edit is applied.",
    )
    .await;

    let terminal = wait_for_request_terminal(&db.node, request_id, Duration::from_secs(300)).await;
    assert_eq!(terminal, "completed", "live edit run must complete");

    let bytes = std::fs::read(workspace.path().join("profile.json")).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("\"max_turns\": 250"),
        "edited content:\n{text}"
    );
    assert!(
        !text.contains("\"max_turns\": 20"),
        "old value gone:\n{text}"
    );
    assert!(
        text.contains("\r\n"),
        "CRLF endings must survive the edit:\n{text:?}"
    );
    assert!(
        text.contains("  \"model_name\": \"fixture-model\""),
        "unrelated line untouched:\n{text}"
    );

    let calls = fetch_tool_call_payloads_for_request(&db.node, request_id).await;
    assert!(!calls.is_empty(), "tool calls must be persisted");
    let edit_completed = calls.iter().any(|c| {
        c.tool_name == "edit_file"
            && c.lifecycle_state.as_deref() == Some("completed")
            && c.result
                .as_deref()
                .is_some_and(|r| r.contains("match_strategy"))
    });
    assert!(
        edit_completed,
        "a completed edit_file call with match_strategy metadata is required; calls: {:?}",
        calls
            .iter()
            .map(|c| (c.tool_name.clone(), c.lifecycle_state.clone()))
            .collect::<Vec<_>>()
    );
    let used_write_file = calls.iter().any(|c| c.tool_name == "write_file");
    assert!(
        !used_write_file,
        "model fell back to write_file — the #738 failure pattern"
    );

    booted.shutdown().await;
}
