//! Live qualification: a real model uses the native `lsp` tool against
//! rust-analyzer on **this** Gents workspace.
//!
//! Offline and required CI skip this. The spec still says no live
//! rust-analyzer in CI. Run it when you want proof the model asked
//! rust-analyzer a checkable question about the runtime crate:
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

use crate::steward_loop_live::{
    bind_d4f_backend, wait_for_assistant_answer, wait_for_request_terminal,
};
use crate::support::fixtures::test_identity;
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::test_db;

const MEET_FILE: &str = "crates/gents/src/toolset/shared/command.rs";
const ADVERTISED_FILE: &str = "crates/gents/src/toolset/lsp/auth.rs";

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

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn first_line_containing(path: &Path, needle: &str) -> u32 {
    let text = std::fs::read_to_string(path).unwrap_or_else(|err| {
        panic!("read {}: {err}", path.display());
    });
    text.lines()
        .position(|line| line.contains(needle))
        .map(|idx| u32::try_from(idx + 1).expect("line"))
        .unwrap_or_else(|| panic!("{}: missing `{needle}`", path.display()))
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

    let workspace = repo_root();
    std::env::set_current_dir(&workspace).expect("chdir to Gents repo root");
    let meet_line =
        first_line_containing(&workspace.join(MEET_FILE), "pub fn meet(self, other: Self)");
    let advertised_line =
        first_line_containing(&workspace.join(ADVERTISED_FILE), "pub fn lsp_advertised");

    let db = test_db("lsp-live").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("lsp-live"));

    let (agent_did, behavior_id) = bind_d4f_backend(db.node.as_ref(), identity.as_ref()).await;

    upsert_tool_selection(
        db.node.as_ref(),
        &ToolSelectionDocument {
            selection_id: "lsp-live-tools".to_string(),
            agent_did: agent_did.clone(),
            enable_file_tools: Some(true),
            file_tools_mode: Some("ReadOnly".to_string()),
            file_tool_root: Some(workspace.display().to_string()),
            enable_bash: Some(false),
            enable_lsp: Some(true),
            lsp_config: Some(
                r#"{"servers":{"rust-analyzer":{"warmup_timeout_ms":60000,"workspace_ready_timings":{"initial":45000},"settings":{"rust-analyzer":{"checkOnSave":false}}}}}"#
                    .into(),
            ),
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
            tool_ceiling: ToolCeiling::readonly_at(&workspace),
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
    let prompt = format!(
        "This workspace is the Gents repository. Answer from rust-analyzer via the lsp tool only. Do not guess types or comments.\n\
         \n\
         1. What rank order does CommandNetworkMode::meet document? Call lsp action=hover on {MEET_FILE}, line={meet_line}, symbol=meet.\n\
         2. What is the signature of lsp_advertised? Call lsp action=hover on {ADVERTISED_FILE}, line={advertised_line}, symbol=lsp_advertised.\n\
         3. Call lsp action=status.\n\
         \n\
         Quote the hover text in your reply. End with DONE."
    );
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        request_id,
        "lsp-live-session-1",
        &prompt,
    )
    .await;

    let terminal =
        wait_for_request_terminal(db.node.as_ref(), request_id, Duration::from_secs(600)).await;
    assert_eq!(terminal, "completed", "live lsp run must complete");

    let calls = fetch_tool_calls(db.node.as_ref(), request_id).await;
    let lsp_calls: Vec<_> = calls
        .iter()
        .filter(|call| call.tool_name.as_deref() == Some("lsp"))
        .collect();
    assert!(
        !lsp_calls.is_empty(),
        "model must persist at least one lsp tool call; calls: {:?}",
        summarize_calls(calls.iter())
    );

    let meet_hover = find_hover(&lsp_calls, MEET_FILE, "meet");
    let meet_text = meet_hover.result.as_deref().unwrap_or("");
    assert!(
        !result_is_error(meet_hover)
            && meet_text.contains("Disabled")
            && meet_text.contains("Inherit"),
        "hover on CommandNetworkMode::meet must quote Disabled < Inherit; got:\n{meet_text}\nall: {:?}",
        summarize_calls(lsp_calls.iter().copied())
    );

    let advertised_hover = find_hover(&lsp_calls, ADVERTISED_FILE, "lsp_advertised");
    let advertised_text = advertised_hover.result.as_deref().unwrap_or("");
    assert!(
        !result_is_error(advertised_hover) && advertised_text.contains("FileToolMode"),
        "hover on lsp_advertised must include FileToolMode; got:\n{advertised_text}\nall: {:?}",
        summarize_calls(lsp_calls.iter().copied())
    );

    let status = lsp_calls
        .iter()
        .find(|call| call_completed(call) && action_of(call).as_deref() == Some("status"));
    let status_result = status.and_then(|call| call.result.as_deref()).unwrap_or("");
    assert!(
        status_result.contains("rust-analyzer (ready)"),
        "status must show rust-analyzer (ready); got:\n{status_result}\nall: {:?}",
        summarize_calls(lsp_calls.iter().copied())
    );

    let answer =
        wait_for_assistant_answer(db.node.as_ref(), request_id, Duration::from_secs(10)).await;
    assert!(
        answer.contains("FileToolMode")
            || (answer.contains("Disabled") && answer.contains("Inherit")),
        "assistant must report a rust-analyzer fact; got:\n{answer}"
    );

    booted.shutdown().await;
}

fn find_hover<'a>(calls: &'a [&ToolCallRow], file: &str, symbol: &str) -> &'a ToolCallRow {
    calls
        .iter()
        .copied()
        .find(|call| {
            call_completed(call)
                && action_of(call).as_deref() == Some("hover")
                && file_of(call).is_some_and(|path| path.ends_with(file) || path.contains(file))
                && symbol_of(call).as_deref() == Some(symbol)
        })
        .unwrap_or_else(|| {
            panic!(
                "need a completed hover on {file} symbol={symbol}; lsp calls: {:?}",
                summarize_calls(calls.iter().copied())
            )
        })
}

fn call_completed(call: &ToolCallRow) -> bool {
    call.status.as_deref() == Some("completed")
        || call.lifecycle_state.as_deref() == Some("completed")
}

fn args_json(call: &ToolCallRow) -> Option<serde_json::Value> {
    serde_json::from_str(call.args.as_deref()?).ok()
}

fn action_of(call: &ToolCallRow) -> Option<String> {
    args_json(call)?
        .get("action")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn file_of(call: &ToolCallRow) -> Option<String> {
    args_json(call)?
        .get("file")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn symbol_of(call: &ToolCallRow) -> Option<String> {
    args_json(call)?
        .get("symbol")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn result_is_error(call: &ToolCallRow) -> bool {
    let result = call.result.as_deref().unwrap_or("");
    result.contains("error")
        || result.contains("failed")
        || result.contains("unavailable")
        || result.contains("stdout closed")
        || result.contains("timed out")
        || result.contains("No hover information")
}

fn summarize_calls<'a>(
    calls: impl IntoIterator<Item = &'a ToolCallRow>,
) -> Vec<(
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
)> {
    calls
        .into_iter()
        .map(|call| {
            (
                call.tool_name.clone(),
                action_of(call),
                call.status.clone(),
                call.result.clone(),
            )
        })
        .collect()
}
