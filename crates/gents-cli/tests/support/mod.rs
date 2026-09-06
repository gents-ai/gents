#![allow(dead_code, unused_imports)]

#[path = "../../src/desired_state/document_handle.rs"]
mod document_handle;
pub mod fs;
pub(crate) use document_handle::document_handle;
pub mod graphql;
pub mod mocks;
pub mod ports;
pub mod process;
pub mod waits;

pub use fs::{
    assert_json_schema_valid, assert_manifest_agent_dids, assert_runtime_init_state,
    manifest_contains, parse_jsonl, project_object_fields, read_captured_log, read_json_file,
    read_runtime_state_json, read_workspace_json, rewrite_manifest_agent_dids, workspace_root,
    write_json_file, write_manifest_root_from_export,
};
pub use graphql::{
    doc_id_for_selection, doc_id_from_create, escape_graphql_string, exec, first_graphql_row,
    graphql_query,
};
pub use mocks::{
    completion_text_sse, request_contains_role_text, request_has_tool_result_message,
    request_system_message, request_tool_names, request_tool_result_text, tool_call_sse,
    tool_call_sse_with_id, MockChatEndpoint, MockModelEndpoint, MockOpenAIEndpoint,
};
pub use ports::{allocate_port, graphql_url};
pub use process::{
    cli_bin, desktop_bin, run_cli_failure_stderr, run_cli_failure_stdout_json, run_cli_json,
    run_cli_text, run_desktop_init_json, run_init_json, spawn_cli, spawn_server,
    spawn_server_with_env, spawn_server_with_ready_json, wait_for_port, ServeProcess,
};
pub use waits::{
    insert_terminal_response, wait_for_completed_inference_behaviors,
    wait_for_completed_tool_calls, wait_for_connected_peer, wait_for_request,
    wait_for_request_lifecycle_state, wait_for_runtime_quiescence, wait_for_runtime_ready,
    wait_for_runtime_state_graphql, wait_for_tool_call,
};

// Matches the DEFAULT_LIVE_ENDPOINT the gents e2e_live suite uses
// (workstation-1 over Tailscale). The previous default was a LAN address
// unroutable from anywhere else, so tests falling through the
// GENTS_CLI_E2E_MODEL_* overrides hung until their deadlines.
pub const DEFAULT_MODEL_ENDPOINT: &str = "http://100.73.235.38:8000/v1";
pub const DEFAULT_MODEL_NAME: &str = "GLM-5.2";

/// Initialize an agent home through `gents init --identity-only` and open its
/// embedded node with the registered signing identity. Homes assembled without
/// this fail the CLI's initialized-home check, and their commits would be
/// unsigned.
pub async fn initialized_agent_node(
    cwd: &std::path::Path,
    agent_home: &std::path::Path,
    agent_name: &str,
) -> anyhow::Result<gents::defra_node::EmbeddedNode> {
    use anyhow::Context as _;

    let home = agent_home.to_str().context("agent home utf8")?;
    let init = run_init_json(
        cwd,
        &[
            "--identity-only",
            "--agent-name",
            agent_name,
            "--home",
            home,
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let key_path = init
        .get("key_path")
        .and_then(serde_json::Value::as_str)
        .context("identity-only init output missing key_path")?;
    let _identity = gents::KeyIdentity::load_or_create(key_path, None)
        .context("loading initialized agent identity")?;

    gents::defra_node::EmbeddedNode::builder()
        .data_path(agent_home.join("data"))
        .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
        .with_node_identity_did(&agent_did)
        .build()
        .await
        .context("opening initialized embedded node")
}

pub fn agent_did_from_init(init: &serde_json::Value) -> anyhow::Result<String> {
    let agent_did = init
        .get("agent_did")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("init output missing agent_did: {init}"))?;
    anyhow::ensure!(
        !agent_did.starts_with("did:test:"),
        "init returned a name-derived DID placeholder: {agent_did}"
    );
    Ok(agent_did.to_string())
}

/// Load the exact initialized-home identity into this test process so fixtures
/// that represent runtime-authored documents can use the production signing
/// path. The server process registers the same key independently.
pub fn identity_from_init(init: &serde_json::Value) -> anyhow::Result<gents::KeyIdentity> {
    use anyhow::Context as _;

    let key_path = init
        .get("key_path")
        .and_then(serde_json::Value::as_str)
        .context("init output missing key_path")?;
    gents::KeyIdentity::load_or_create(key_path, None).context("loading initialized agent identity")
}
