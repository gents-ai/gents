use anyhow::Result;
use gents_codex_protocol as codex;

use super::super::background::{cancel_projected_background_tool_key, clean_background_terminals};
use super::super::protocol::{send_error, send_result};
use super::super::{Outbound, ShimState, JSONRPC_INVALID_PARAMS};

pub(super) async fn handle_background_request(
    outbound: &Outbound,
    state: &ShimState,
    request: codex::ClientRequest,
) -> Result<()> {
    match request {
        codex::ClientRequest::ThreadBackgroundTerminalsClean {
            request_id, params, ..
        } => {
            clean_background_terminals(state, &params.thread_id).await?;
            send_result(
                outbound,
                request_id,
                codex::ThreadBackgroundTerminalsCleanResponse {},
            )
            .await
        }
        codex::ClientRequest::CommandExecTerminate {
            request_id, params, ..
        } => match cancel_projected_background_tool_key(state, &params.process_id).await {
            Ok(_) => {
                send_result(outbound, request_id, codex::CommandExecTerminateResponse {}).await
            }
            Err(err) => {
                send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    err.to_string(),
                )
                .await
            }
        },
        codex::ClientRequest::ProcessKill {
            request_id, params, ..
        } => match cancel_projected_background_tool_key(state, &params.process_handle).await {
            Ok(_) => send_result(outbound, request_id, codex::ProcessKillResponse {}).await,
            Err(err) => {
                send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    err.to_string(),
                )
                .await
            }
        },
        other => unreachable!(
            "non-background Codex request routed to background handler: {}",
            other.method()
        ),
    }
}
