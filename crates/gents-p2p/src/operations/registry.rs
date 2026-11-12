use std::collections::BTreeSet;

use chrono::{DateTime, SecondsFormat, Utc};
use gents_protocol::document_query::{
    fields::{peer_pairing_desired, peer_registry},
    Delete, Filter, Query, Selection,
};
use gents_protocol::graphql::graphql_rows_from_response;
use gents_protocol::operations::p2p::{
    peer_online, RegisteredPeer, RegistryDeregistered, RegistryError, RegistryRegistered,
    RegistryRegistration,
};
use gents_store::config_client::ConfigAccess;
use serde_json::Value;

use super::{optional_string, string_list};
use crate::p2p_reconcile::registry::{
    registry_upsert_mutation, RegistryEntry, UpsertKind, DEFAULT_NETWORK_ID,
};
use crate::p2p_reconcile::templates::resolve_template;
use crate::p2p_reconcile::{heartbeat_is_fresh, REGISTRY_STALE_AFTER};

fn passthrough(error: impl Into<anyhow::Error>) -> RegistryError {
    RegistryError::Passthrough(
        error
            .into()
            .reallocate_into_boxed_dyn_error_without_backtrace(),
    )
}

/// Every registered peer this actor can read, ordered by peer ID, with
/// whether each is online at `now` and whether this node pairs with it.
pub async fn list_registered_peers(
    access: &ConfigAccess,
    now: DateTime<Utc>,
) -> Result<Vec<RegisteredPeer>, RegistryError> {
    let registry = Query::select(
        Selection::new()
            .field(peer_registry::PEER_ID)
            .field(peer_registry::NODE_DID)
            .field(peer_registry::DISPLAY_NAME)
            .field(peer_registry::NETWORK_ID)
            .field(peer_registry::TEMPLATES)
            .field(peer_registry::ADDRESSES)
            .field(peer_registry::STATUS)
            .field(peer_registry::UPDATED_AT),
    )
    .render();
    let rows = graphql_rows_from_response(
        &access.read(&registry).await.map_err(passthrough)?,
        "PeerRegistry",
    );
    let desired = Query::select(Selection::new().field(peer_pairing_desired::PEER_ID)).render();
    let paired: BTreeSet<String> = access
        .read(&desired)
        .await
        .map(|response| graphql_rows_from_response(&response, "PeerPairingDesired"))
        .unwrap_or_default()
        .iter()
        .filter_map(|row| optional_string(row, "peer_id"))
        .collect();
    Ok(registered_peers(rows, &paired, now))
}

fn registered_peers(
    rows: Vec<Value>,
    paired: &BTreeSet<String>,
    now: DateTime<Utc>,
) -> Vec<RegisteredPeer> {
    let mut peers: Vec<RegisteredPeer> = rows
        .into_iter()
        .filter_map(|row| {
            let peer_id = optional_string(&row, "peer_id")?;
            let status = optional_string(&row, "status");
            let updated_at = optional_string(&row, "updated_at");
            let fresh = updated_at
                .as_deref()
                .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
                .is_some_and(|at| {
                    heartbeat_is_fresh(at.with_timezone(&Utc), now, REGISTRY_STALE_AFTER)
                });
            Some(RegisteredPeer {
                online: peer_online(status.as_deref() == Some("online"), fresh),
                paired: paired.contains(&peer_id),
                peer_id,
                node_did: optional_string(&row, "node_did"),
                display_name: optional_string(&row, "display_name"),
                network_id: optional_string(&row, "network_id")
                    .unwrap_or_else(|| DEFAULT_NETWORK_ID.to_owned()),
                templates: string_list(&row, "templates"),
                addresses: string_list(&row, "addresses"),
                status,
                updated_at,
            })
        })
        .collect();
    peers.sort_by(|left, right| left.peer_id.cmp(&right.peer_id));
    peers
}

fn offered_templates(offered: &[String]) -> Result<Vec<String>, RegistryError> {
    let mut templates: Vec<String> = Vec::new();
    for id in offered {
        let id = id.trim();
        if resolve_template(id).is_none() {
            return Err(RegistryError::UnknownTemplate(id.to_owned()));
        }
        if !templates.iter().any(|known| known == id) {
            templates.push(id.to_owned());
        }
    }
    Ok(templates)
}

/// Writes this node's own registry row as online at `now`. A listen address
/// that carries no peer ID gets the node's appended.
pub async fn register_self(
    access: &ConfigAccess,
    registration: &RegistryRegistration,
    now: DateTime<Utc>,
) -> Result<RegistryRegistered, RegistryError> {
    let peer_id = registration
        .peer_id
        .as_deref()
        .map(str::trim)
        .filter(|peer_id| !peer_id.is_empty())
        .ok_or(RegistryError::PeerIdUnknown)?
        .to_owned();
    let templates = offered_templates(&registration.templates)?;
    let addresses: Vec<String> = registration
        .listen_addresses
        .iter()
        .map(|address| address.trim())
        .filter(|address| !address.is_empty())
        .map(|address| {
            if address.starts_with('/') && !address.contains("/p2p/") {
                format!("{address}/p2p/{peer_id}")
            } else {
                address.to_owned()
            }
        })
        .collect();
    let display_name = registration
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned);
    let entry = RegistryEntry {
        peer_id,
        node_did: registration.node_did.clone(),
        addresses,
        templates,
        display_name,
        status: "online".to_owned(),
        network_id: registration
            .network_id
            .clone()
            .unwrap_or_else(|| DEFAULT_NETWORK_ID.to_owned()),
    };
    let mutation = registry_upsert_mutation(
        &entry,
        &now.to_rfc3339_opts(SecondsFormat::Secs, true),
        UpsertKind::Full,
    );
    access
        .write("p2p.registry.register", &mutation)
        .await
        .map_err(passthrough)?;
    Ok(RegistryRegistered {
        peer_id: entry.peer_id,
        node_did: entry.node_did,
        display_name: entry.display_name,
        templates: entry.templates,
        network_id: entry.network_id,
        addresses: entry.addresses,
    })
}

/// Removes the registry rows of the peer the runtime reports as its own.
pub async fn deregister_self(
    access: &ConfigAccess,
    peer_id: Option<&str>,
) -> Result<RegistryDeregistered, RegistryError> {
    let peer_id = peer_id
        .map(str::trim)
        .filter(|peer_id| !peer_id.is_empty())
        .ok_or(RegistryError::PeerIdUnknown)?;
    let response = access
        .write_rendered(
            "p2p.registry.deregister",
            &Delete::matching(Filter::eq(peer_registry::PEER_ID, peer_id)).render(),
        )
        .await
        .map_err(passthrough)?;
    let deleted = &response["data"]["delete_PeerRegistry"];
    let removed_count = match deleted.as_array() {
        Some(rows) => rows
            .iter()
            .filter(|row| row.get("_docID").is_some())
            .count(),
        None => usize::from(deleted.get("_docID").is_some()),
    };
    Ok(RegistryDeregistered {
        peer_id: peer_id.to_owned(),
        removed_count,
    })
}
