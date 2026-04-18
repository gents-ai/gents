use super::shared::build_peer_entries;
use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;

pub(super) fn prepare_state(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let Some(client) = client else {
        state.setup.selected_peer_id = None;
        return;
    };

    let peers = build_peer_entries(client, store);
    if peers.is_empty() {
        state.setup.show_add_form = true;
        return;
    }

    if state
        .setup
        .selected_peer_id
        .as_deref()
        .is_none_or(|record_id| !peers.iter().any(|peer| peer.record_id == record_id))
    {
        state.setup.selected_peer_id = peers.first().map(|peer| peer.record_id.clone());
    }
}
