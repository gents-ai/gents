use std::cmp::Reverse;

pub type SessionOrderKey = (
    Reverse<Option<String>>,
    Reverse<Option<String>>,
    String,
    Option<String>,
);

/// Lean `SessionOperations.sessionListOrder`: latest activity first, then the
/// newest session, then the session and requester identities.
pub fn session_order_key(
    updated_at: Option<&str>,
    created_at: Option<&str>,
    session_id: &str,
    requester_did: Option<&str>,
) -> SessionOrderKey {
    (
        Reverse(updated_at.map(str::to_owned)),
        Reverse(created_at.map(str::to_owned)),
        session_id.to_owned(),
        requester_did.map(str::to_owned),
    )
}
