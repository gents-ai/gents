use anyhow::Result;
use tokio::sync::watch;

use super::super::{ActiveTurn, ConnectionState, ShimState};

pub(super) async fn install_active_turn(
    connection: &ConnectionState,
    thread_id: String,
    turn_id: String,
    request_id: String,
    cancel_tx: watch::Sender<bool>,
) {
    *connection.active_turn.lock().await = Some(ActiveTurn {
        thread_id,
        turn_id,
        request_id,
        queued_steering_request_ids: Vec::new(),
        cancel_tx,
    });
}

pub(super) async fn clear_active_turn_if_current(
    connection: &ConnectionState,
    thread_id: &str,
    turn_id: &str,
) {
    let mut active = connection.active_turn.lock().await;
    if active
        .as_ref()
        .is_some_and(|turn| turn.thread_id == thread_id && turn.turn_id == turn_id)
    {
        *active = None;
    }
}

pub(super) fn cancel_abandoned_steering_request(state: &ShimState, request_id: String) {
    let node = state.node.clone();
    tokio::spawn(async move {
        if let Err(error) = defra_agent::interrupt_request(node.as_ref(), &request_id).await {
            tracing::warn!(
                %error,
                request_id,
                "Codex shim failed to interrupt abandoned steering request"
            );
        }
    });
}

pub(super) async fn take_next_steering_request(
    connection: &ConnectionState,
    thread_id: &str,
    turn_id: &str,
) -> Option<String> {
    let mut active = connection.active_turn.lock().await;
    let active_turn = active.as_mut()?;
    if active_turn.thread_id != thread_id || active_turn.turn_id != turn_id {
        return None;
    }
    if active_turn.queued_steering_request_ids.is_empty() {
        return None;
    }
    let next_request_id = active_turn.queued_steering_request_ids.remove(0);
    active_turn.request_id = next_request_id.clone();
    Some(next_request_id)
}

pub(in crate::commands::codex_shim) async fn interrupt_active_turn(
    connection: &ConnectionState,
    state: &ShimState,
    thread_id: &str,
    turn_id: &str,
) -> Result<()> {
    let active = {
        let mut guard = connection.active_turn.lock().await;
        let Some(active) = guard.as_ref().cloned() else {
            return Ok(());
        };
        if active.thread_id != thread_id {
            return Ok(());
        }
        if active.turn_id != turn_id {
            tracing::warn!(
                active_turn_id = %active.turn_id,
                requested_turn_id = %turn_id,
                thread_id,
                "Codex shim interrupt turn id did not match active turn; interrupting active thread turn"
            );
        }
        *guard = None;
        active
    };
    let _ = active.cancel_tx.send(true);
    let node = state.node.clone();
    let request_id = active.request_id.clone();
    tokio::spawn(async move {
        if let Err(error) = defra_agent::interrupt_request(node.as_ref(), &request_id).await {
            tracing::warn!(%error, request_id, "Codex shim failed to forward DEFRA interrupt");
        }
    });
    Ok(())
}
