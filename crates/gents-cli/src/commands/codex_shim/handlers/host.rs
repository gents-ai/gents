use anyhow::Result;
use gents_codex_protocol as codex;

use super::super::host_runtime;
use super::super::protocol::{send_error, send_result};
use super::super::{ConnectionState, Outbound, ShimState};

pub(super) async fn handle_host_request(
    connection: &ConnectionState,
    state: &ShimState,
    outbound: &Outbound,
    request: codex::ClientRequest,
) -> Result<()> {
    match request {
        codex::ClientRequest::GitDiffToRemote {
            request_id, params, ..
        } => match host_runtime::git_diff_to_remote(state, params).await {
            Ok(response) => send_result(outbound, request_id, response).await,
            Err(err) => send_error(outbound, request_id, err.code, err.message).await,
        },
        codex::ClientRequest::FuzzyFileSearch {
            request_id, params, ..
        } => match host_runtime::fuzzy_file_search(state, params).await {
            Ok(response) => send_result(outbound, request_id, response).await,
            Err(err) => send_error(outbound, request_id, err.code, err.message).await,
        },
        codex::ClientRequest::FuzzyFileSearchSessionStart {
            request_id, params, ..
        } => match host_runtime::fuzzy_file_search_session_start(connection, params).await {
            Ok(response) => send_result(outbound, request_id, response).await,
            Err(err) => send_error(outbound, request_id, err.code, err.message).await,
        },
        codex::ClientRequest::FuzzyFileSearchSessionUpdate {
            request_id, params, ..
        } => {
            match host_runtime::fuzzy_file_search_session_update(connection, state, params).await {
                Ok(response) => send_result(outbound, request_id, response).await,
                Err(err) => send_error(outbound, request_id, err.code, err.message).await,
            }
        }
        codex::ClientRequest::FuzzyFileSearchSessionStop {
            request_id, params, ..
        } => match host_runtime::fuzzy_file_search_session_stop(connection, state, params).await {
            Ok(response) => send_result(outbound, request_id, response).await,
            Err(err) => send_error(outbound, request_id, err.code, err.message).await,
        },
        other => unreachable!(
            "non-host Codex request routed to host handler: {}",
            other.method()
        ),
    }
}
