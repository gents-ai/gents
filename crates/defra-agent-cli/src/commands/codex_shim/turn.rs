mod active;
mod stream;
mod submission;

use anyhow::Result;
use codex_app_server_protocol as codex;
use tokio::sync::watch;

pub(super) use active::interrupt_active_turn;

use active::{
    cancel_abandoned_steering_request, clear_active_turn_if_current, install_active_turn,
};
use stream::stream_defra_turn;
use submission::create_agent_request_with_retry;

use super::protocol::{
    codex_steering_metadata, codex_turn_metadata, send_committed_user_message, send_error,
    send_notification, send_result, turn_value, user_text_from_input,
};
use super::turn_projection::TurnProjection;
use super::{
    ConnectionState, ShimState, JSONRPC_INTERNAL_ERROR, JSONRPC_INVALID_PARAMS,
    JSONRPC_INVALID_REQUEST,
};
use crate::RequestSubmitOptions;

pub(super) async fn start_defra_turn(
    connection: &ConnectionState,
    state: &ShimState,
    request_id: codex::RequestId,
    thread_id: String,
    input: Vec<codex::UserInput>,
) -> Result<()> {
    let user_text = user_text_from_input(&input);
    if user_text.trim().is_empty() {
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_REQUEST,
            "Codex turn input did not contain text for DEFRA".to_string(),
        )
        .await;
    }

    let cwd = connection
        .thread_cwds
        .lock()
        .await
        .get(&thread_id)
        .cloned()
        .unwrap_or_else(|| state.cwd.clone());
    let metadata = codex_turn_metadata(&cwd);

    let submitted = match create_agent_request_with_retry(
        state,
        &user_text,
        Some(&thread_id),
        RequestSubmitOptions {
            metadata: Some(metadata),
            ..RequestSubmitOptions::default()
        },
    )
    .await
    {
        Ok(submitted) => submitted,
        Err(err) => {
            return send_error(
                &connection.outbound,
                request_id,
                JSONRPC_INTERNAL_ERROR,
                format!("failed to submit DEFRA AgentRequest: {err}"),
            )
            .await;
        }
    };

    let turn_id = submitted.request_id.clone();
    let started_turn = turn_value(&turn_id, codex::TurnStatus::InProgress, Vec::new(), None);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    install_active_turn(
        connection,
        thread_id.clone(),
        turn_id.clone(),
        submitted.request_id.clone(),
        cancel_tx,
    )
    .await;

    if let Err(err) = send_result(
        &connection.outbound,
        request_id,
        codex::TurnStartResponse {
            turn: started_turn.clone(),
        },
    )
    .await
    {
        clear_active_turn_if_current(connection, &thread_id, &turn_id).await;
        return Err(err);
    }

    send_notification(
        &connection.outbound,
        state,
        codex::ServerNotification::TurnStarted(codex::TurnStartedNotification {
            thread_id: thread_id.clone(),
            turn: started_turn,
        }),
    )
    .await?;

    send_committed_user_message(&connection.outbound, state, &thread_id, &turn_id, &input).await?;

    let mut projection = TurnProjection::new(state, &thread_id, &turn_id, cwd.clone());
    let result =
        match stream_defra_turn(connection, state, &submitted, &mut projection, cancel_rx).await {
            Ok(()) => Ok(()),
            Err(err) => {
                let message = format!("DEFRA turn failed: {err}");
                projection
                    .append_agent_delta(&connection.outbound, &format!("[agent error] {message}\n"))
                    .await?;
                projection
                    .finish_turn(
                        &connection.outbound,
                        codex::TurnStatus::Failed,
                        Some(message),
                    )
                    .await
            }
        };

    clear_active_turn_if_current(connection, &thread_id, &turn_id).await;
    result
}

pub(super) async fn steer_defra_turn(
    connection: &ConnectionState,
    state: &ShimState,
    request_id: codex::RequestId,
    params: codex::TurnSteerParams,
) -> Result<()> {
    if params.expected_turn_id.trim().is_empty() {
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_REQUEST,
            "expectedTurnId must not be empty".to_string(),
        )
        .await;
    }

    let user_text = user_text_from_input(&params.input);
    if user_text.trim().is_empty() {
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_REQUEST,
            "input must not be empty".to_string(),
        )
        .await;
    }

    let cwd = connection
        .thread_cwds
        .lock()
        .await
        .get(&params.thread_id)
        .cloned()
        .unwrap_or_else(|| state.cwd.clone());

    let active_snapshot = connection.active_turn.lock().await.clone();
    let Some(active_turn) = active_snapshot else {
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_PARAMS,
            "no active turn to steer".to_string(),
        )
        .await;
    };
    if active_turn.thread_id != params.thread_id {
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_PARAMS,
            "no active turn to steer".to_string(),
        )
        .await;
    }
    if active_turn.turn_id != params.expected_turn_id {
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_PARAMS,
            format!(
                "expected active turn id `{}` but found `{}`",
                params.expected_turn_id, active_turn.turn_id
            ),
        )
        .await;
    }

    let turn_id = active_turn.turn_id.clone();
    let queued_after_request_id = active_turn.request_id.clone();
    let metadata = codex_steering_metadata(&cwd, &queued_after_request_id);
    let submitted = match create_agent_request_with_retry(
        state,
        &user_text,
        Some(&params.thread_id),
        RequestSubmitOptions {
            metadata: Some(metadata),
            ..RequestSubmitOptions::default()
        },
    )
    .await
    {
        Ok(submitted) => submitted,
        Err(err) => {
            return send_error(
                &connection.outbound,
                request_id,
                JSONRPC_INTERNAL_ERROR,
                format!("failed to submit DEFRA steering AgentRequest: {err}"),
            )
            .await;
        }
    };

    let mut active = connection.active_turn.lock().await;
    let Some(current_active) = active.as_mut() else {
        drop(active);
        cancel_abandoned_steering_request(state, submitted.request_id.clone());
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_PARAMS,
            "active turn ended while submitting steering request".to_string(),
        )
        .await;
    };
    if current_active.thread_id != params.thread_id || current_active.turn_id != turn_id {
        let current_turn_id = current_active.turn_id.clone();
        drop(active);
        cancel_abandoned_steering_request(state, submitted.request_id.clone());
        return send_error(
            &connection.outbound,
            request_id,
            JSONRPC_INVALID_PARAMS,
            format!("active turn changed from `{turn_id}` to `{current_turn_id}`"),
        )
        .await;
    }
    current_active
        .queued_steering_request_ids
        .push(submitted.request_id.clone());
    drop(active);

    send_result(
        &connection.outbound,
        request_id,
        codex::TurnSteerResponse {
            turn_id: turn_id.clone(),
        },
    )
    .await?;
    send_committed_user_message(
        &connection.outbound,
        state,
        &params.thread_id,
        &turn_id,
        &params.input,
    )
    .await?;
    tracing::info!(
        turn_id,
        queued_after_request_id,
        steering_request_id = %submitted.request_id,
        "Codex shim accepted active-turn steering request"
    );
    Ok(())
}
