use anyhow::Result;
use codex_app_server_protocol as codex;
use serde_json::json;

use super::super::protocol::send_result;
use super::super::turn::{interrupt_active_turn, start_defra_turn, steer_defra_turn};
use super::super::{trace, ConnectionState, Outbound, ShimState};

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
                if let Err(err) = start_defra_turn(
                    &connection,
                    &state,
                    request_id,
                    params.thread_id,
                    params.input,
                )
                .await
                {
                    tracing::warn!(%err, "Codex shim DEFRA turn task failed");
                }
            });
            Ok(())
        }
        codex::ClientRequest::TurnSteer {
            request_id, params, ..
        } => steer_defra_turn(connection, state, request_id, params).await,
        codex::ClientRequest::TurnInterrupt {
            request_id, params, ..
        } => {
            trace::shim_event_fields(
                &state.trace_path,
                "turn_interrupt_received",
                json!({
                    "request_id": request_id,
                    "thread_id": params.thread_id,
                    "requested_turn_id": params.turn_id,
                }),
            );
            interrupt_active_turn(connection, state, &params.thread_id, &params.turn_id).await?;
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
