//! The peer-registry and pairing operations every client calls.

pub mod pairing;
pub mod registry;

use serde_json::Value;

fn optional_string(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn string_list(row: &Value, field: &str) -> Vec<String> {
    row.get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use gents_lean_vocab::families;
    use gents_protocol::operations::p2p::{
        pairing_connected, pairing_replicating, pairing_subscribed, peer_online, RegistryRefusal,
    };

    #[test]
    fn peer_and_pairing_answers_match_lean() {
        families::peer_operations_peer_online::replay(|i| {
            peer_online(i.reports_online, i.heartbeat_fresh)
        });
        families::peer_operations_pairing_replicating::replay(|i| {
            pairing_replicating(i.has_addresses, i.all_addresses_applied)
        });
        families::peer_operations_pairing_subscribed::replay(|i| {
            pairing_subscribed(
                i.template_pushes,
                i.replicating,
                i.has_collections,
                i.all_collections_applied,
            )
        });
        families::peer_operations_pairing_connected::replay(|i| {
            pairing_connected(
                i.has_transport_peers,
                i.transport_peer_connected,
                i.own_peer_connected,
            )
        });
        families::peer_operations_registry_refusal_recovery::replay(|i| {
            RegistryRefusal::recovery(i.registry_refusal)
        });
    }
}
