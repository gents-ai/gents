use anyhow::Result;
use gents_codex_protocol as codex;

use super::compat::send_planned_stub;
use super::protocol::{client_request_from_jsonrpc, send_error};
use super::{trace, ConnectionState, ShimState, JSONRPC_INTERNAL_ERROR, JSONRPC_INVALID_REQUEST};

mod background;
mod basic;
mod host;
mod models;
mod skills;
mod thread;
mod turns;

pub(super) async fn handle_request(
    connection: &ConnectionState,
    state: &ShimState,
    request: codex::JSONRPCRequest,
) -> Result<()> {
    let request_id = request.id.clone();
    let method = request.method.clone();
    tracing::info!(%method, %request_id, "Codex shim request");
    trace::shim_event(&state.trace_path, format!("request {request_id} {method}"));
    let outbound = &connection.outbound;
    let codex_request = match client_request_from_jsonrpc(request) {
        Ok(request) => request,
        Err(err) => {
            return send_error(
                outbound,
                request_id,
                JSONRPC_INVALID_REQUEST,
                format!("invalid Codex shim request `{method}`: {err}"),
            )
            .await;
        }
    };

    if requires_client_identity(&codex_request) {
        return send_error(
            outbound,
            request_id,
            JSONRPC_INTERNAL_ERROR,
            "This operation requires an authenticated client DID; the Codex shim transport does not provide one".to_owned(),
        ).await;
    }

    let result = match codex_request {
        codex::ClientRequest::Initialize { .. }
        | codex::ClientRequest::GetAccount { .. }
        | codex::ClientRequest::GetAccountRateLimits { .. }
        | codex::ClientRequest::ModelList { .. }
        | codex::ClientRequest::ModelProviderCapabilitiesRead { .. }
        | codex::ClientRequest::ConfigRead { .. }
        | codex::ClientRequest::ConfigBatchWrite { .. }
        | codex::ClientRequest::ConfigValueWrite { .. }
        | codex::ClientRequest::ConfigRequirementsRead { .. }
        | codex::ClientRequest::ExternalAgentConfigDetect { .. }
        | codex::ClientRequest::ExternalAgentConfigImport { .. }
        | codex::ClientRequest::ExperimentalFeatureList { .. }
        | codex::ClientRequest::PermissionProfileList { .. }
        | codex::ClientRequest::CollaborationModeList { .. }
        | codex::ClientRequest::SkillsList { .. }
        | codex::ClientRequest::HooksList { .. }
        | codex::ClientRequest::PluginList { .. }
        | codex::ClientRequest::McpServerStatusList { .. } => {
            basic::handle_basic_request(outbound, state, codex_request).await
        }

        codex::ClientRequest::ThreadStart { .. }
        | codex::ClientRequest::ThreadResume { .. }
        | codex::ClientRequest::ThreadList { .. }
        | codex::ClientRequest::ThreadFork { .. }
        | codex::ClientRequest::ThreadSearch { .. }
        | codex::ClientRequest::ThreadLoadedList { .. }
        | codex::ClientRequest::ThreadRead { .. }
        | codex::ClientRequest::ThreadTurnsList { .. }
        | codex::ClientRequest::ThreadTurnsItemsList { .. }
        | codex::ClientRequest::ThreadUnsubscribe { .. }
        | codex::ClientRequest::ThreadArchive { .. }
        | codex::ClientRequest::ThreadUnarchive { .. }
        | codex::ClientRequest::ThreadSetName { .. }
        | codex::ClientRequest::ThreadMemoryModeSet { .. }
        | codex::ClientRequest::ThreadSettingsUpdate { .. }
        | codex::ClientRequest::ThreadMetadataUpdate { .. }
        | codex::ClientRequest::ThreadGoalSet { .. }
        | codex::ClientRequest::ThreadGoalGet { .. }
        | codex::ClientRequest::ThreadGoalClear { .. }
        | codex::ClientRequest::GetConversationSummary { .. } => {
            thread::handle_thread_request(connection, state, outbound, codex_request).await
        }

        codex::ClientRequest::TurnStart { .. }
        | codex::ClientRequest::TurnSteer { .. }
        | codex::ClientRequest::TurnInterrupt { .. } => {
            turns::handle_turn_request(connection, state, outbound, codex_request).await
        }

        codex::ClientRequest::ThreadBackgroundTerminalsClean { .. }
        | codex::ClientRequest::CommandExecTerminate { .. }
        | codex::ClientRequest::ProcessKill { .. } => {
            background::handle_background_request(outbound, state, codex_request).await
        }

        codex::ClientRequest::GitDiffToRemote { .. }
        | codex::ClientRequest::FuzzyFileSearch { .. }
        | codex::ClientRequest::FuzzyFileSearchSessionStart { .. }
        | codex::ClientRequest::FuzzyFileSearchSessionUpdate { .. }
        | codex::ClientRequest::FuzzyFileSearchSessionStop { .. } => {
            host::handle_host_request(connection, state, outbound, codex_request).await
        }

        codex::ClientRequest::SkillsConfigWrite {
            request_id, params, ..
        } => skills::handle_skills_config_write(outbound, state, request_id, params).await,

        unsupported => send_planned_stub(outbound, state, unsupported).await,
    };

    if let Err(err) = result {
        tracing::warn!(
            %method,
            %request_id,
            error = %err,
            "Codex shim request failed"
        );
        return send_error(
            outbound,
            request_id,
            JSONRPC_INTERNAL_ERROR,
            format!("Codex shim request `{method}` failed: {err}"),
        )
        .await;
    }

    Ok(())
}

/// The Codex transport authenticates an optional shared token, not a DID.
/// A runtime-signed database request cannot represent the connected user.
fn requires_client_identity(request: &codex::ClientRequest) -> bool {
    matches!(
        request,
        codex::ClientRequest::GetAccount { .. }
            | codex::ClientRequest::GetAccountRateLimits { .. }
            | codex::ClientRequest::ModelList { .. }
            | codex::ClientRequest::ConfigRead { .. }
            | codex::ClientRequest::SkillsList { .. }
            | codex::ClientRequest::ConfigBatchWrite { .. }
            | codex::ClientRequest::ConfigValueWrite { .. }
            | codex::ClientRequest::ExternalAgentConfigImport { .. }
            | codex::ClientRequest::SkillsConfigWrite { .. }
            | codex::ClientRequest::ThreadStart { .. }
            | codex::ClientRequest::ThreadResume { .. }
            | codex::ClientRequest::ThreadList { .. }
            | codex::ClientRequest::ThreadFork { .. }
            | codex::ClientRequest::ThreadSearch { .. }
            | codex::ClientRequest::ThreadLoadedList { .. }
            | codex::ClientRequest::ThreadRead { .. }
            | codex::ClientRequest::ThreadTurnsList { .. }
            | codex::ClientRequest::ThreadTurnsItemsList { .. }
            | codex::ClientRequest::ThreadUnsubscribe { .. }
            | codex::ClientRequest::ThreadArchive { .. }
            | codex::ClientRequest::ThreadUnarchive { .. }
            | codex::ClientRequest::ThreadSetName { .. }
            | codex::ClientRequest::ThreadMemoryModeSet { .. }
            | codex::ClientRequest::ThreadSettingsUpdate { .. }
            | codex::ClientRequest::ThreadMetadataUpdate { .. }
            | codex::ClientRequest::ThreadGoalSet { .. }
            | codex::ClientRequest::ThreadGoalGet { .. }
            | codex::ClientRequest::ThreadGoalClear { .. }
            | codex::ClientRequest::GetConversationSummary { .. }
            | codex::ClientRequest::TurnStart { .. }
            | codex::ClientRequest::TurnSteer { .. }
            | codex::ClientRequest::TurnInterrupt { .. }
            | codex::ClientRequest::ThreadBackgroundTerminalsClean { .. }
            | codex::ClientRequest::CommandExecTerminate { .. }
            | codex::ClientRequest::ProcessKill { .. }
    )
}

#[cfg(test)]
#[test]
fn connected_client_operations_do_not_use_runtime_authority() {
    let start: codex::ClientRequest = serde_json::from_value(serde_json::json!({
        "id": 1, "method": "thread/start", "params": {}
    }))
    .unwrap();
    let initialize: codex::ClientRequest = serde_json::from_value(serde_json::json!({
        "id": 2, "method": "initialize", "params": {"clientInfo": {"name": "test", "version": "1"}}
    }))
    .unwrap();
    assert!(requires_client_identity(&start));
    assert!(!requires_client_identity(&initialize));
}
