use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;

pub(super) fn prepare_state(
    _state: &mut ShellState,
    _client: Option<&ClientCore>,
    _store: Option<&ClientStore>,
) {
}
