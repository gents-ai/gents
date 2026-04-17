use crate::client::{ClientCore, ClientStore};
use crate::operator::controller as operator_controller;
use crate::state::ShellState;

pub(super) fn prepare_state(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    if let Some(store) = store {
        let peer_statuses = client.map(ClientCore::peer_statuses).unwrap_or_default();
        operator_controller::sync_from_snapshot(&mut state.operator, &peer_statuses, store);
    }
}
