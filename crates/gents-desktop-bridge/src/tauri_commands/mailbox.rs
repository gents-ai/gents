use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use tauri::State;

use crate::commands::{dismiss_mailbox, list_mailbox, start_mailbox_request};
use crate::error::BridgeError;
use crate::state::{current_core, DesktopAppState};
use crate::types::{MailboxItemRequest, MailboxItemView};

fn running_core(state: &State<'_, DesktopAppState>) -> Result<Arc<ClientCore>, BridgeError> {
    current_core(state).ok_or_else(|| BridgeError::untyped("desktop client is not running"))
}

#[tauri::command]
pub fn desktop_mailbox_list(
    state: State<'_, DesktopAppState>,
) -> Result<Vec<MailboxItemView>, BridgeError> {
    Ok(list_mailbox(running_core(&state)?.as_ref()))
}

#[tauri::command]
pub fn desktop_mailbox_start_request(
    request: MailboxItemRequest,
    state: State<'_, DesktopAppState>,
) -> Result<MailboxItemView, BridgeError> {
    start_mailbox_request(running_core(&state)?.as_ref(), &request.item_id)
        .map_err(|error| BridgeError::untyped(error.to_string()))
}

#[tauri::command]
pub async fn desktop_mailbox_dismiss(
    request: MailboxItemRequest,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    dismiss_mailbox(running_core(&state)?.as_ref(), &request.item_id)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
}
