use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use tauri::{Runtime, State};

use crate::commands::{dismiss_mailbox, observe_mailbox_item, start_mailbox_request};
use crate::error::BridgeError;
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    MailboxItemObservation, MailboxItemObservationRequest, MailboxItemRequest, MailboxItemView,
};

fn running_core(state: &State<'_, DesktopAppState>) -> Result<Arc<ClientCore>, BridgeError> {
    current_core(state).ok_or_else(|| BridgeError::untyped("desktop client is not running"))
}

/// The node whose store lists the item: the one the request names, else the
/// calling window's own node (`ClientWindowScope.commandNode`).
fn item_node(
    state: &State<'_, DesktopAppState>,
    window: &str,
    request: &MailboxItemRequest,
) -> Result<String, BridgeError> {
    state
        .window_command_node(window, request.node_did.as_deref())
        .ok_or_else(|| BridgeError::untyped("a mailbox item names its node"))
}

#[tauri::command]
pub async fn desktop_mailbox_observe(
    request: MailboxItemObservationRequest,
    state: State<'_, DesktopAppState>,
) -> Result<Option<MailboxItemObservation>, BridgeError> {
    observe_mailbox_item(running_core(&state)?.as_ref(), &request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub fn desktop_mailbox_start_request<R>(
    window: tauri::WebviewWindow<R>,
    request: MailboxItemRequest,
    state: State<'_, DesktopAppState>,
) -> Result<MailboxItemView, BridgeError>
where
    R: Runtime,
{
    let node_did = item_node(&state, window.label(), &request)?;
    start_mailbox_request(running_core(&state)?.as_ref(), &node_did, &request.item_id)
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_mailbox_dismiss<R>(
    window: tauri::WebviewWindow<R>,
    request: MailboxItemRequest,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    let node_did = item_node(&state, window.label(), &request)?;
    dismiss_mailbox(running_core(&state)?.as_ref(), &node_did, &request.item_id)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}
