//! #738/#724 LIVE qualification: a real model, faced with drifted file
//! content, lands the edit through `edit_file` instead of falling back to
//! `write_file` — the production failure pattern that motivated #738 (amy
//! gave up on `edit_file` for 80% of her mutation work).
//!
//! What real inference exercises that the deterministic fences cannot:
//! whether the MODEL, given the new tool description, diagnostics, and
//! content-hash flow, actually completes the read → edit_file loop on a
//! file whose bytes drift from what the read rendering suggests (CRLF +
//! trailing whitespace). The matcher itself is Lean-fenced
//! (`Proofs/EditMatch/`) and conformance-fenced; this pins the
//! model-facing contract end to end.
//!
//! Gated on `GENTS_D4F_LIVE=1` (same gate and backend as
//! `steward_loop_live.rs`). Run with:
//!
//! ```bash
//! GENTS_D4F_LIVE=1 cargo test --test e2e_live \
//!   edit_file_live_model_lands_drifted_edit_without_write_file \
//!   -- --ignored --test-threads=1 --nocapture
//! ```

use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{
    load_agent_behavior, upsert_agent_behavior, upsert_tool_selection, DocumentRuntimeOptions,
    Gents, ToolCeiling, ToolSelectionDocument,
};
use serde::Deserialize;

use gents::AgentIdentity;

use crate::steward_loop_live::{bind_d4f_backend, wait_for_request_terminal};
use crate::support::fixtures::test_identity;
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::test_db;

fn d4f_enabled() -> bool {
    std::env::var("GENTS_D4F_LIVE").as_deref() == Ok("1")
}

const SEED: &str = "{\r\n  \"max_turns\": 20,  \r\n  \"model_name\": \"d4f\"\r\n}\r\n";

#[derive(Deserialize)]
struct ToolCallRow {
    tool_name: Option<String>,
    status: Option<String>,
    result: Option<String>,
}

async fn fetch_tool_calls(node: &EmbeddedNode, request_id: &str) -> Vec<ToolCallRow> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ request_id: {{ _eq: "{escaped}" }} }}) {{
                tool_name
                status
                result
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "tool call query failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|rows| rows.as_array())
        .map(|rows| {
            rows.iter()
                .cloned()
                .map(|value| serde_json::from_value(value).expect("decode AgentToolCall row"))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: set GENTS_D4F_LIVE=1 and pass --ignored"]
async fn edit_file_live_model_lands_drifted_edit_without_write_file() {
    assert!(
        d4f_enabled(),
        "set GENTS_D4F_LIVE=1 and pass --ignored to run the edit_file live qualification"
    );

    let db = test_db("edit-file-live").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("edit-file-live"));

    let workspace = tempfile::tempdir().expect("workspace tempdir");
    std::fs::write(workspace.path().join("profile.json"), SEED).unwrap();

    let (agent_did, behavior_id) = bind_d4f_backend(db.node.as_ref(), identity.as_ref()).await;

    upsert_tool_selection(
        db.node.as_ref(),
        &ToolSelectionDocument {
            selection_id: "edit-live-tools".to_string(),
            agent_did: agent_did.clone(),
            enable_file_tools: Some(true),
            file_tools_mode: Some("ReadWrite".to_string()),
            file_tool_root: Some(workspace.path().display().to_string()),
            enable_bash: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut behavior = load_agent_behavior(db.node.as_ref(), &behavior_id)
        .await
        .expect("load behavior")
        .expect("behavior exists");
    behavior.tool_selection_id = Some("edit-live-tools".to_string());
    upsert_agent_behavior(db.node.as_ref(), &behavior)
        .await
        .expect("bind tool selection");

    let agent = Gents::from_default_behavior_documents(
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
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    let booted = BootedAgent::new(shutdown_tx, handle, agent_did.clone());

    let request_id = "edit-file-live-req-1";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        request_id,
        "edit-file-live-session-1",
        "In profile.json, change max_turns from 20 to 250. Use read_file first, \
         then make the change with edit_file, passing the content_hash from the \
         read as expected_content_hash. Do not use write_file. Reply DONE when \
         the edit is applied.",
    )
    .await;

    let terminal =
        wait_for_request_terminal(db.node.as_ref(), request_id, Duration::from_secs(300)).await;
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
        text.contains("  \"model_name\": \"d4f\""),
        "unrelated line untouched:\n{text}"
    );

    let calls = fetch_tool_calls(db.node.as_ref(), request_id).await;
    assert!(!calls.is_empty(), "tool calls must be persisted");
    let edit_completed = calls.iter().any(|c| {
        c.tool_name.as_deref() == Some("edit_file")
            && c.status.as_deref() == Some("completed")
            && c.result
                .as_deref()
                .is_some_and(|r| r.contains("match_strategy"))
    });
    assert!(
        edit_completed,
        "a completed edit_file call with match_strategy metadata is required; calls: {:?}",
        calls
            .iter()
            .map(|c| (c.tool_name.clone(), c.status.clone()))
            .collect::<Vec<_>>()
    );
    let used_write_file = calls
        .iter()
        .any(|c| c.tool_name.as_deref() == Some("write_file"));
    assert!(
        !used_write_file,
        "model fell back to write_file — the #738 failure pattern"
    );

    booted.shutdown().await;
}
