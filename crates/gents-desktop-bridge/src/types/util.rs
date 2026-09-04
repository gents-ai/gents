use gents_protocol::client_protocol::ClientTurnState;

pub fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub fn turn_state_label(state: ClientTurnState) -> &'static str {
    match state {
        ClientTurnState::WaitingForClaim => "waitingForClaim",
        ClientTurnState::Streaming => "streaming",
        ClientTurnState::Completed => "completed",
        ClientTurnState::Failed => "failed",
        ClientTurnState::Superseded => "superseded",
        ClientTurnState::Interrupted => "interrupted",
    }
}

/// True for the two non-terminal turn states (`WaitingForClaim`,
/// `Streaming`) that still have a live tail worth overlaying onto a
/// snapshot. Single owner for the `Some(WaitingForClaim) | Some(Streaming)`
/// check shared by `snapshot::session::projection` and
/// `snapshot::session::live_delta`.
pub(crate) fn is_live_turn_state(state: Option<ClientTurnState>) -> bool {
    state.is_some_and(|state| !state.is_terminal())
}
