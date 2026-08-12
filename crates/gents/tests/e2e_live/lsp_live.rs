//! Live qualification: a real model uses the native `lsp` tool against
//! rust-analyzer on the checked-in `demo/lsp-rust` fixture crate.
//!
//! Offline and required CI skip this. The spec still says no live
//! rust-analyzer in CI. Run it when you want proof the model actually
//! called `lsp` and got rust-analyzer output:
//!
//! ```bash
//! rust-analyzer --version
//! GENTS_LIVE_LSP=1 cargo test -p gents --test e2e_live \
//!   lsp_live_model_uses_rust_analyzer \
//!   -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! Uses the same DeepSeek V4 Flash backend as the other d4f live tests.

use std::path::{Path, PathBuf};
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

fn live_lsp_enabled() -> bool {
    std::env::var("GENTS_LIVE_LSP").as_deref() == Ok("1")
}

fn rust_analyzer_on_path() -> bool {
    std::process::Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn copy_demo_workspace() -> tempfile::TempDir {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../demo/lsp-rust/workspace");
    let dir = tempfile::tempdir().expect("workspace tempdir");
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    copy_file(&src.join("Cargo.toml"), &dir.path().join("Cargo.toml"));
    copy_file(&src.join("src/lib.rs"), &dir.path().join("src/lib.rs"));
    dir
}

fn copy_file(from: &Path, to: &Path) {
    std::fs::copy(from, to)
        .unwrap_or_else(|err| panic!("copy {} -> {}: {err}", from.display(), to.display()));
}

#[derive(Deserialize, Debug)]
struct ToolCallRow {
    tool_name: Option<String>,
    status: Option<String>,
    lifecycle_state: Option<String>,
    args: Option<String>,
    result: Option<String>,
}

async fn fetch_tool_calls(node: &EmbeddedNode, request_id: &str) -> Vec<ToolCallRow> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ request_id: {{ _eq: "{escaped}" }} }}) {{
                tool_name
                status
                lifecycle_state
                args
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
#[ignore = "live: set GENTS_LIVE_LSP=1 and pass --ignored"]
async fn lsp_live_model_uses_rust_analyzer() {
    assert!(
        live_lsp_enabled(),
        "set GENTS_LIVE_LSP=1 and pass --ignored to run the rust-analyzer lsp live qualification"
    );
    assert!(
        rust_analyzer_on_path(),
        "rust-analyzer must be on PATH (rust-analyzer --version)"
    );

    let db = test_db("lsp-live").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("lsp-live"));
    let workspace = copy_demo_workspace();

    let (agent_did, behavior_id) = bind_d4f_backend(db.node.as_ref(), identity.as_ref()).await;

    upsert_tool_selection(
        db.node.as_ref(),
        &ToolSelectionDocument {
            selection_id: "lsp-live-tools".to_string(),
            agent_did: agent_did.clone(),
            enable_file_tools: Some(true),
            file_tools_mode: Some("ReadOnly".to_string()),
            file_tool_root: Some(workspace.path().display().to_string()),
            enable_bash: Some(false),
            enable_lsp: Some(true),
            lsp_config: Some(r#"{"servers":{"rust-analyzer":{"warmup_timeout_ms":30000}}}"#.into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut behavior = load_agent_behavior(db.node.as_ref(), &behavior_id)
        .await
        .expect("load behavior")
        .expect("behavior exists");
    behavior.tool_selection_id = Some("lsp-live-tools".to_string());
    upsert_agent_behavior(db.node.as_ref(), &behavior)
        .await
        .expect("bind tool selection");

    let agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        Arc::clone(&identity),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readonly_at(workspace.path()),
            ..Default::default()
        },
    )
    .await
    .expect("boot agent");
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    let booted = BootedAgent::new(shutdown_tx, handle, agent_did.clone());

    let request_id = "lsp-live-req-1";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        request_id,
        "lsp-live-session-1",
        "The workspace is a tiny Rust crate. You must use the lsp tool — do not guess types.\n\
         1. Call lsp with action=hover, file=src/lib.rs, line=4, symbol=add.\n\
         2. Call lsp with action=definition on the same file and symbol.\n\
         3. Call lsp with action=status.\n\
         Quote the hover signature for add. Reply DONE when those three calls have completed.",
    )
    .await;

    let terminal =
        wait_for_request_terminal(db.node.as_ref(), request_id, Duration::from_secs(300)).await;
    assert_eq!(terminal, "completed", "live lsp run must complete");

    let calls = fetch_tool_calls(db.node.as_ref(), request_id).await;
    let lsp_calls: Vec<_> = calls
        .iter()
        .filter(|call| call.tool_name.as_deref() == Some("lsp"))
        .collect();
    assert!(
        !lsp_calls.is_empty(),
        "model must persist at least one lsp tool call; calls: {:?}",
        calls
            .iter()
            .map(|call| (
                call.tool_name.clone(),
                call.status.clone(),
                call.args.clone()
            ))
            .collect::<Vec<_>>()
    );
    assert!(
        lsp_calls.iter().any(|call| {
            call.status.as_deref() == Some("completed")
                || call.lifecycle_state.as_deref() == Some("completed")
        }),
        "at least one lsp call must complete; lsp calls: {:?}",
        lsp_calls
            .iter()
            .map(|call| {
                (
                    call.status.clone(),
                    call.lifecycle_state.clone(),
                    call.args.clone(),
                    call.result.clone(),
                )
            })
            .collect::<Vec<_>>()
    );

    let results = lsp_calls
        .iter()
        .filter_map(|call| call.result.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    let saw_analyzer = results.to_lowercase().contains("rust-analyzer");
    let saw_add_sig = results.contains("add")
        && (results.contains("u32") || results.contains("fn add") || results.contains("left"));
    assert!(
        saw_analyzer || saw_add_sig,
        "lsp results must mention rust-analyzer or the add signature; results:\n{results}"
    );

    booted.shutdown().await;
}
