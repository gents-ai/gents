use anyhow::Result;
use gents_codex_protocol as codex;

use super::super::background::{cancel_projected_background_tool_key, clean_background_terminals};
use super::super::protocol::{send_error, send_result};
use super::super::{Outbound, ShimState, JSONRPC_INVALID_PARAMS};

/// `thread/backgroundTerminals/clean` cancels every running background row of
/// the thread, plugin jobs included, by design. ProcessKill and
/// CommandExecTerminate stop a job row through the shared cancel owner, which
/// reports no children count.
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
            Ok(send_result(
                outbound,
                request_id,
                codex::ThreadBackgroundTerminalsCleanResponse {},
            )
            .await?)
        }
        codex::ClientRequest::CommandExecTerminate {
            request_id, params, ..
        } => match cancel_projected_background_tool_key(state, &params.process_id).await {
            Ok(_) => Ok(
                send_result(outbound, request_id, codex::CommandExecTerminateResponse {}).await?,
            ),
            Err(err) => Ok(send_error(
                outbound,
                request_id,
                JSONRPC_INVALID_PARAMS,
                err.to_string(),
            )
            .await?),
        },
        codex::ClientRequest::ProcessKill {
            request_id, params, ..
        } => match cancel_projected_background_tool_key(state, &params.process_handle).await {
            Ok(_) => Ok(send_result(outbound, request_id, codex::ProcessKillResponse {}).await?),
            Err(err) => Ok(send_error(
                outbound,
                request_id,
                JSONRPC_INVALID_PARAMS,
                err.to_string(),
            )
            .await?),
        },
        other => unreachable!(
            "non-background Codex request routed to background handler: {}",
            other.method()
        ),
    }
}
