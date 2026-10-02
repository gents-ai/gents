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
//! GENTS_LIVE_LSP=1 GENTS_EVAL_TARGET=workstation-1 \
//!   GENTS_LSP_RUST_PACK_DIR=<packs checkout>/packs/gents/lsp_rust \
//!   cargo test -p gents --test e2e_live \
//!   lsp_live_model_uses_rust_analyzer \
//!   -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! Runs against the inference target named by `GENTS_EVAL_TARGET`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::document_config::{FileTools, HostTools, IntegrationTools, LspTools, Tools};
use gents::{DocumentRuntimeOptions, FileToolMode, Gents, ToolCeiling};

use gents::AgentIdentity;

use crate::support::fixtures::{configure_behavior_tools, test_identity};
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::live_inference::{
    bind_target, live_target, wait_for_assistant_answer, wait_for_request_terminal,
};
use crate::support::snapshots::fetch_tool_call_payloads_for_request;
use crate::support::test_db;

const MEET_FILE: &str = "crates/gents-loop/src/tool_policy.rs";
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

/// `lsp_rust` no longer ships inside this repository; point this at a
/// checkout of gents-ai/packs's `packs/gents/lsp_rust`.
fn pack_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("GENTS_LSP_RUST_PACK_DIR").unwrap_or_else(|_| {
            panic!(
                "GENTS_LSP_RUST_PACK_DIR must name a checkout of the lsp_rust pack \
             (gents-ai/packs, packs/gents/lsp_rust)"
            )
        }),
    )
}

fn pack_json_string(relative: &str, field: &str) -> String {
    let path = pack_dir().join(relative);
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("read {}: {err}", path.display());
    });
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|err| {
        panic!("parse {}: {err}", path.display());
    });
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("{} missing string {field}", path.display()))
        .to_string()
}

fn pack_default_prompt() -> String {
    pack_json_string("experiment.json", "default_prompt")
}

fn pack_lsp_config() -> String {
    let relative = "pack_config.json";
    let path = pack_dir().join(relative);
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("read {}: {err}", path.display());
    });
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|err| {
        panic!("parse {}: {err}", path.display());
    });
    value["tools"]
        .as_array()
        .and_then(|tools| {
            tools
                .iter()
                .find(|tools| tools["tools_id"] == "lsp-readonly")
        })
        .and_then(|tools| tools["integrations"]["lsp"]["config"].as_str())
        .unwrap_or_else(|| panic!("{} missing lsp-readonly Tools config", path.display()))
        .to_owned()
}

fn pack_system_prompt() -> String {
    std::fs::read_to_string(pack_dir().join("agent_behaviors/lsp_coder/system_prompt.md"))
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

#[derive(Clone, Debug)]
struct ToolCallRow {
    tool_name: Option<String>,
    status: Option<String>,
    lifecycle_state: Option<String>,
    args: Option<String>,
    result: Option<String>,
}

async fn fetch_tool_calls(node: &Arc<EmbeddedNode>, request_id: &str) -> Vec<ToolCallRow> {
    fetch_tool_call_payloads_for_request(node, request_id)
        .await
        .into_iter()
        .map(|call| ToolCallRow {
            tool_name: Some(call.tool_name),
            status: call.status,
            lifecycle_state: call.lifecycle_state,
            args: call.arguments,
            result: call.result,
        })
        .collect()
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

    let (agent_did, behavior_id) =
        bind_target(db.node.as_ref(), identity.as_ref(), &live_target()).await;

    configure_behavior_tools(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        Some(pack_system_prompt()),
        Tools {
            tools_id: "lsp-live-tools".to_string(),
            agent_did: agent_did.clone(),
            host: Some(HostTools {
                root: Some(workspace.display().to_string()),
                files: Some(FileTools {
                    mode: FileToolMode::ReadOnly,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            integrations: Some(IntegrationTools {
                lsp: Some(LspTools {
                    config: Some(pack_lsp_config()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;

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
    let unscripted_calls = fetch_tool_calls(&db.node, unscripted_request_id).await;
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
        answer_reports_meet_contract(&unscripted_answer),
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

    let calls = fetch_tool_calls(&db.node, request_id).await;
    let lsp_calls: Vec<_> = calls
        .iter()
        .filter(|call| call.tool_name.as_deref() == Some("lsp"))
        .collect();
    assert!(
        !lsp_calls.is_empty(),
        "model must persist at least one lsp tool call; calls: {:?}",
        summarize_calls(calls.iter())
    );
    let meet_hover = find_hover(&lsp_calls, MEET_FILE, "meet", &["Disabled", "Inherit"]);
    assert!(!result_is_error(meet_hover));

    let advertised_hover = find_hover(
        &lsp_calls,
        ADVERTISED_FILE,
        "lsp_advertised",
        &["FileToolMode"],
    );
    assert!(!result_is_error(advertised_hover));

    let answer =
        wait_for_assistant_answer(db.node.as_ref(), request_id, Duration::from_secs(10)).await;
    assert!(
        answer.contains("FileToolMode")
            || (answer.contains("Disabled") && answer.contains("Inherit")),
        "assistant must report a rust-analyzer fact; got:\n{answer}"
    );

    booted.shutdown().await;
}

/// `tool_policy.rs` defines several `meet` methods, so a hover on the right
/// file and symbol is selected by the facts it must quote.
fn find_hover<'a>(
    calls: &'a [&ToolCallRow],
    file: &str,
    symbol: &str,
    required: &[&str],
) -> &'a ToolCallRow {
    calls
        .iter()
        .copied()
        .find(|call| {
            call_completed(call)
                && action_of(call).as_deref() == Some("hover")
                && file_of(call)
                    .is_some_and(|path| gents::toolset::result_path_matches(file, &path))
                && symbol_of(call).as_deref() == Some(symbol)
                && !result_is_error(call)
                && call
                    .result
                    .as_deref()
                    .is_some_and(|text| required.iter().all(|needle| text.contains(needle)))
        })
        .unwrap_or_else(|| {
            panic!(
                "need a completed hover on {file} symbol={symbol} quoting {required:?}; lsp calls: {:?}",
                summarize_calls(calls.iter().copied())
            )
        })
}

fn call_completed(call: &ToolCallRow) -> bool {
    call.lifecycle_state.as_deref() == Some("completed")
}

fn answer_reports_meet_contract(answer: &str) -> bool {
    let lower = answer.to_ascii_lowercase();
    lower.contains("disabled < inherit < enabled")
        && (lower.contains("restrict") || lower.contains("wins"))
}

#[test]
fn completed_persistence_status_does_not_mask_a_failed_lifecycle() {
    let failed = ToolCallRow {
        tool_name: Some("lsp".into()),
        status: Some("completed".into()),
        lifecycle_state: Some("failed".into()),
        args: Some(r#"{"action":"hover"}"#.into()),
        result: Some("policy denied".into()),
    };
    assert!(!call_completed(&failed));
}

#[test]
fn meet_contract_match_uses_the_explicit_rank_order_not_first_name_occurrence() {
    let answer = "The enum declares Inherit, Disabled, Enabled. More restrictive mode wins: \
                  Disabled < Inherit < Enabled.";
    assert!(answer_reports_meet_contract(answer));
}

#[test]
fn hover_selector_skips_completed_but_semantically_empty_retries() {
    let empty = ToolCallRow {
        tool_name: Some("lsp".into()),
        status: Some("completed".into()),
        lifecycle_state: Some("completed".into()),
        args: Some(r#"{"action":"hover","file":"src/lib.rs","symbol":"target"}"#.into()),
        result: Some("No hover information".into()),
    };
    let useful = ToolCallRow {
        result: Some("pub fn target()".into()),
        ..empty.clone()
    };
    let calls = [&empty, &useful];
    assert!(std::ptr::eq(
        find_hover(&calls, "src/lib.rs", "target", &[]),
        &useful
    ));
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
