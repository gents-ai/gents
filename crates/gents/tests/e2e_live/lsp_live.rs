//! Live qualification: a real model uses the native `lsp` tool against
//! rust-analyzer on **this** Gents workspace.
//!
//! Offline and required CI skip this. The spec still says no live
//! rust-analyzer in CI. Run it when you want proof the model asked
//! rust-analyzer both an unscripted semantic question and a deterministic
//! sequence about the runtime crate:
//!
//! ```bash
//! rust-analyzer --version
//! GENTS_LIVE_LSP=1 cargo test -p gents --test e2e_live \
//!   lsp_live_model_uses_rust_analyzer \
//!   -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! Uses the same DeepSeek V4 Flash backend as the other d4f live tests.

use std::path::PathBuf;
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

fn pack_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../demo/lsp-rust")
}

fn pack_json_string(relative: &str, field: &str) -> String {
    let raw = std::fs::read_to_string(pack_dir().join(relative)).unwrap_or_else(|err| {
        panic!("read demo/lsp-rust/{relative}: {err}");
    });
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|err| {
        panic!("parse demo/lsp-rust/{relative}: {err}");
    });
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("demo/lsp-rust/{relative} missing string {field}"))
        .to_string()
}

fn pack_default_prompt() -> String {
    pack_json_string("experiment.json", "default_prompt")
}

fn pack_lsp_config() -> String {
    pack_json_string("tool-selections/lsp-readonly/object.json", "lsp_config")
}

fn pack_system_prompt() -> String {
    std::fs::read_to_string(pack_dir().join("agent-behaviors/lsp-coder/system_prompt.md"))
        .expect("pack system prompt")
}

fn pack_unscripted_prompt() -> String {
    std::fs::read_to_string(pack_dir().join("unscripted_prompt.md"))
        .expect("pack unscripted prompt")
}

struct CurrentDirGuard(PathBuf);

impl CurrentDirGuard {
    fn set(path: &std::path::Path) -> Self {
        let original = std::env::current_dir().expect("current directory");
        std::env::set_current_dir(path).expect("set live LSP workspace cwd");
        Self(original)
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.0).expect("restore current directory after live LSP test");
    }
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
    // The document runtime resolves its initial host-tool root from cwd before
    // the persisted selection is reconciled. This ignored qualification runs
    // with --test-threads=1; restore cwd on every exit so it cannot poison a
    // subsequent ignored live test in the same process.
    let _cwd = CurrentDirGuard::set(&workspace);

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
            lsp_config: Some(pack_lsp_config()),
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
    behavior.system_prompt = Some(pack_system_prompt());
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

    // UX arm: no paths, line numbers, symbol-discovery steps, retries, or
    // status choreography. The model must discover and use the semantic tool.
    let unscripted_request_id = "lsp-live-unscripted-1";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        unscripted_request_id,
        "lsp-live-unscripted-session-1",
        &pack_unscripted_prompt(),
    )
    .await;
    let terminal = wait_for_request_terminal(
        db.node.as_ref(),
        unscripted_request_id,
        Duration::from_secs(600),
    )
    .await;
    assert_eq!(
        terminal, "completed",
        "unscripted live lsp run must complete"
    );
    let unscripted_calls = fetch_tool_calls(db.node.as_ref(), unscripted_request_id).await;
    let useful_semantic = unscripted_calls.iter().any(|call| {
        call.tool_name.as_deref() == Some("lsp")
            && call_completed(call)
            && matches!(
                action_of(call).as_deref(),
                Some(
                    "hover"
                        | "symbols"
                        | "definition"
                        | "type_definition"
                        | "implementation"
                        | "references"
                )
            )
            && !result_is_error(call)
    });
    assert!(
        useful_semantic,
        "unscripted arm must produce at least one useful semantic lsp result; calls: {:?}",
        summarize_calls(unscripted_calls.iter())
    );
    let unscripted_answer = wait_for_assistant_answer(
        db.node.as_ref(),
        unscripted_request_id,
        Duration::from_secs(10),
    )
    .await;
    assert!(
        unscripted_answer.contains("Disabled")
            && unscripted_answer.contains("Inherit")
            && unscripted_answer.contains("Enabled")
            && (unscripted_answer.to_ascii_lowercase().contains("restrict")
                || unscripted_answer.to_ascii_lowercase().contains("wins")),
        "unscripted answer must report that the more restrictive mode wins in Disabled < Inherit < Enabled order; got:\n{unscripted_answer}"
    );

    // Deterministic arm: retained as a stable harness/protocol regression gate.
    let request_id = "lsp-live-req-1";
    let prompt = pack_default_prompt();
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
    let meet_symbols = find_useful_action_for_file(&lsp_calls, "symbols", MEET_FILE, "meet");
    let meet_symbols_text = meet_symbols.result.as_deref().unwrap_or("");
    assert!(
        !result_is_error(meet_symbols) && meet_symbols_text.contains("meet"),
        "document symbols for {MEET_FILE} must include meet; got:\n{meet_symbols_text}\nall: {:?}",
        summarize_calls(lsp_calls.iter().copied())
    );
    let advertised_symbols =
        find_useful_action_for_file(&lsp_calls, "symbols", ADVERTISED_FILE, "lsp_advertised");
    let advertised_symbols_text = advertised_symbols.result.as_deref().unwrap_or("");
    assert!(
        !result_is_error(advertised_symbols)
            && advertised_symbols_text.contains("lsp_advertised"),
        "document symbols for {ADVERTISED_FILE} must include lsp_advertised; got:\n{advertised_symbols_text}\nall: {:?}",
        summarize_calls(lsp_calls.iter().copied())
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
                && file_of(call)
                    .is_some_and(|path| gents::toolset::result_path_matches(file, &path))
                && symbol_of(call).as_deref() == Some(symbol)
        })
        .unwrap_or_else(|| {
            panic!(
                "need a completed hover on {file} symbol={symbol}; lsp calls: {:?}",
                summarize_calls(calls.iter().copied())
            )
        })
}

fn find_useful_action_for_file<'a>(
    calls: &'a [&ToolCallRow],
    action: &str,
    file: &str,
    result_contains: &str,
) -> &'a ToolCallRow {
    calls
        .iter()
        .copied()
        .find(|call| {
            call_completed(call)
                && action_of(call).as_deref() == Some(action)
                && file_of(call)
                    .is_some_and(|path| gents::toolset::result_path_matches(file, &path))
                && !result_is_error(call)
                && call
                    .result
                    .as_deref()
                    .is_some_and(|result| result.contains(result_contains))
        })
        .unwrap_or_else(|| {
            panic!(
                "need a useful completed {action} call on {file} containing {result_contains}; lsp calls: {:?}",
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
    gents::toolset::result_looks_failed(call.result.as_deref().unwrap_or(""))
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
