use anyhow::Result;
use gents_codex_protocol as codex;
use serde_json::json;

use super::super::protocol::{send_error, send_result};
use super::super::thread_projection::load_codex_thread;
use super::super::turn::{
    interrupt_active_turn, interrupt_caused_thread_turn, start_gents_turn, steer_gents_turn,
};
use super::super::{trace, ConnectionState, Outbound, ShimState, JSONRPC_INVALID_PARAMS};

pub(super) async fn handle_turn_request(
    connection: &ConnectionState,
    state: &ShimState,
    outbound: &Outbound,
    request: codex::ClientRequest,
) -> Result<()> {
    match request {
        codex::ClientRequest::TurnStart {
            request_id, params, ..
        } => {
            let connection = connection.clone();
            let state = state.clone();
            tokio::spawn(async move {
                if let Err(err) = start_gents_turn(
                    &connection,
                    &state,
                    request_id,
                    params.thread_id,
                    params.input,
                )
                .await
                {
                    tracing::warn!(%err, "Codex shim GENTS turn task failed");
                }
            });
            Ok(())
        }
        codex::ClientRequest::TurnSteer {
            request_id, params, ..
        } => steer_gents_turn(connection, state, request_id, params).await,
        codex::ClientRequest::TurnInterrupt {
            request_id, params, ..
        } => {
            let caused = load_codex_thread(state, &params.thread_id)
                .await?
                .and_then(|record| record.caused);
            trace::shim_event_fields(
                &state.trace_path,
                "turn_interrupt_received",
                json!({
                    "request_id": request_id,
                    "thread_id": params.thread_id,
                    "requested_turn_id": params.turn_id,
                }),
            );
            if let Some(thread) = caused.as_ref() {
                // The Codex user speaks for the root thread.
                let caller = gents::session_origin::SessionScope {
                    node_did: state.node_did.to_string(),
                    session_id: thread.root_session_id.clone(),
                    requester_did: Some(state.local_requester_did().to_string()),
                };
                if let Some(refusal) =
                    gents::session_message::interrupt_refusal(&state.node, &caller, &thread.scope())
                        .await?
                {
                    return send_error(outbound, request_id, JSONRPC_INVALID_PARAMS, refusal).await;
                }
            }
            match caused {
                Some(thread) => {
                    interrupt_caused_thread_turn(connection, state, &thread, &params.turn_id)
                        .await?
                }
                None => {
                    interrupt_active_turn(connection, state, &params.thread_id, &params.turn_id)
                        .await?
                }
            }
            let result = send_result(
                outbound,
                request_id.clone(),
                codex::TurnInterruptResponse {},
            )
            .await;
            trace::shim_event_fields(
                &state.trace_path,
                "turn_interrupt_response_sent",
                json!({
                    "request_id": request_id,
                    "thread_id": params.thread_id,
                    "requested_turn_id": params.turn_id,
                    "ok": result.is_ok(),
                }),
            );
            result
        }
        other => unreachable!(
            "non-turn Codex request routed to turn handler: {}",
            other.method()
        ),
    }
}
