use anyhow::Result;
use codex_app_server_protocol as codex;

use super::super::protocol::send_result;
use super::super::turn::{interrupt_active_turn, start_defra_turn, steer_defra_turn};
use super::super::{ConnectionState, Outbound, ShimState};

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
            interrupt_active_turn(connection, state, &params.thread_id, &params.turn_id).await?;
            send_result(outbound, request_id, codex::TurnInterruptResponse {}).await
        }
        other => unreachable!(
            "non-turn Codex request routed to turn handler: {}",
            other.method()
        ),
    }
}
