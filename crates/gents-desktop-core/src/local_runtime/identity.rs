use p2p::iroh::parse_public_peer_addr;

pub(super) fn normalize_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn peer_id_from_public_addr(value: &str) -> Option<String> {
    let value = normalize_optional_string(Some(value))?;
    parse_public_peer_addr(&value)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string())
}

pub(super) fn resolve_p2p_peer_id(
    live_peer_id: Option<&str>,
    shareable_address: Option<&str>,
    stored_peer_id: Option<&str>,
) -> Option<String> {
    normalize_optional_string(live_peer_id)
        .or_else(|| shareable_address.and_then(peer_id_from_public_addr))
        .or_else(|| normalize_optional_string(stored_peer_id))
}
