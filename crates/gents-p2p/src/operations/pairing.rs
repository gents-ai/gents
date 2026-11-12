use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use gents_protocol::document_query::{
    fields::{peer_pairing_applied, peer_pairing_desired},
    Query, QuerySet, Selection,
};
use gents_protocol::enrollment::AuthorizationLease;
use gents_protocol::graphql::graphql_rows_from_response;
use gents_protocol::operations::p2p::{
    pairing_connected, pairing_replicating, pairing_subscribed, Pairing, PairingLease,
    PairingReaderTarget, PairingReaderTargets,
};
use gents_store::config_client::{ConfigAccess, ConfigClientError};
use serde_json::Value;

use super::{optional_string, string_list};
use crate::p2p_reconcile::{resolve_template, Delivery, TransportEndpoint};

#[derive(Debug, thiserror::Error)]
pub enum PairingReadError {
    #[error("reading the pairing observations")]
    Access(#[source] ConfigClientError),
    #[error("the pairing response omitted {0}")]
    Omitted(&'static str),
    #[error("a pairing observation is malformed")]
    Malformed(#[source] serde_json::Error),
}

struct Applied {
    node_did: Option<String>,
    peer_id: String,
    collections: BTreeSet<String>,
    replicator_addresses: BTreeSet<String>,
}

/// The desired pairings this actor can read, ordered by peer ID, each with
/// its lease at `now` and what the applied observations and `connected_peers`
/// (the runtime's live connections, as it reports them) say about it.
pub async fn list_pairings(
    access: &ConfigAccess,
    connected_peers: &[String],
    now: DateTime<Utc>,
) -> Result<Vec<Pairing>, PairingReadError> {
    let desired = Query::select(
        Selection::new()
            .field(peer_pairing_desired::PEER_ID)
            .field(peer_pairing_desired::NODE_DID)
            .field(peer_pairing_desired::ACTOR_DID)
            .field(peer_pairing_desired::COLLECTIONS)
            .field(peer_pairing_desired::REPLICATOR_ADDRESSES)
            .field(peer_pairing_desired::PROFILES)
            .field(peer_pairing_desired::TEMPLATE)
            .field(peer_pairing_desired::CREATED_AT)
            .field(peer_pairing_desired::UPDATED_AT)
            .field(peer_pairing_desired::ENROLLMENT_AUTHORIZATION_SEQUENCE)
            .field(peer_pairing_desired::ENROLLMENT_AUTHORIZATION_EXPIRES_AT),
    )
    .render();
    let desired = graphql_rows_from_response(
        &access
            .read(&desired)
            .await
            .map_err(PairingReadError::Access)?,
        "PeerPairingDesired",
    );
    let applied = Query::select(
        Selection::doc_id()
            .field(peer_pairing_applied::PEER_ID)
            .field(peer_pairing_applied::NODE_DID)
            .field(peer_pairing_applied::COLLECTIONS)
            .field(peer_pairing_applied::REPLICATOR_ADDRESSES),
    )
    .render();
    let applied = access
        .read(&applied)
        .await
        .map(|response| graphql_rows_from_response(&response, "PeerPairingApplied"))
        .unwrap_or_default();
    Ok(observed_pairings(
        desired,
        &canonical_applied(applied),
        connected_peers,
        now,
    ))
}

fn canonical_applied(rows: Vec<Value>) -> Vec<Applied> {
    let mut rows: Vec<(String, Applied)> = rows
        .into_iter()
        .filter_map(|row| {
            Some((
                row.get("_docID")?.as_str()?.to_owned(),
                Applied {
                    peer_id: optional_string(&row, "peer_id")?,
                    node_did: row
                        .get("node_did")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    collections: string_list(&row, "collections").into_iter().collect(),
                    replicator_addresses: string_list(&row, "replicator_addresses")
                        .into_iter()
                        .collect(),
                },
            ))
        })
        .collect();
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    let mut canonical = BTreeMap::new();
    for (_, row) in rows {
        canonical
            .entry((row.node_did.clone(), row.peer_id.clone()))
            .or_insert(row);
    }
    canonical.into_values().collect()
}

fn observed_pairings(
    rows: Vec<Value>,
    applied: &[Applied],
    connected_peers: &[String],
    now: DateTime<Utc>,
) -> Vec<Pairing> {
    let applied: BTreeMap<_, _> = applied
        .iter()
        .map(|row| ((row.node_did.as_deref(), row.peer_id.as_str()), row))
        .collect();
    let connected: BTreeSet<String> = connected_peers
        .iter()
        .flat_map(|peer| connected_peer_ids(peer))
        .collect();
    let mut pairings: Vec<Pairing> = rows
        .into_iter()
        .filter_map(|row| {
            let peer_id = optional_string(&row, "peer_id")?;
            let actor_did = optional_string(&row, "actor_did");
            let collections = string_list(&row, "collections");
            let replicator_addresses = string_list(&row, "replicator_addresses");
            let transport_peer_ids: Vec<String> = replicator_addresses
                .iter()
                .filter_map(|address| TransportEndpoint::parse(address.clone()).ok())
                .map(|endpoint| endpoint.peer_id().to_string())
                .collect();
            let template = optional_string(&row, "template");
            let observed = applied.get(&(actor_did.as_deref(), peer_id.as_str()));
            let replicating = pairing_replicating(
                !replicator_addresses.is_empty(),
                replicator_addresses.iter().all(|address| {
                    observed.is_some_and(|row| row.replicator_addresses.contains(address))
                }),
            );
            let subscribed = pairing_subscribed(
                template
                    .as_deref()
                    .and_then(resolve_template)
                    .is_some_and(|template| template.delivery == Delivery::Push),
                replicating,
                !collections.is_empty(),
                collections.iter().all(|collection| {
                    observed.is_some_and(|row| row.collections.contains(collection))
                }),
            );
            let is_connected = pairing_connected(
                !transport_peer_ids.is_empty(),
                transport_peer_ids
                    .iter()
                    .any(|peer_id| connected.contains(peer_id)),
                connected.contains(&peer_id),
            );
            let enrollment_authorization_expires_at =
                optional_string(&row, "enrollment_authorization_expires_at");
            let authorization_lease =
                enrollment_authorization_expires_at
                    .as_deref()
                    .map(|expires_at| {
                        let lease = AuthorizationLease::at(expires_at, now);
                        PairingLease {
                            state: lease.state,
                            remaining_days: lease.remaining_days(),
                        }
                    });
            Some(Pairing {
                peer_id,
                transport_peer_ids,
                node_did: optional_string(&row, "node_did"),
                actor_did,
                collections,
                replicator_addresses,
                profiles: string_list(&row, "profiles"),
                template,
                created_at: optional_string(&row, "created_at"),
                updated_at: optional_string(&row, "updated_at"),
                enrollment_authorization_sequence: row
                    .get("enrollment_authorization_sequence")
                    .and_then(Value::as_i64),
                enrollment_authorization_expires_at,
                authorization_lease,
                connected: is_connected,
                subscribed,
                replicating,
            })
        })
        .collect();
    pairings.sort_by(|left, right| left.peer_id.cmp(&right.peer_id));
    pairings
}

fn connected_peer_ids(peer: &str) -> Vec<String> {
    let peer = peer.trim();
    if peer.is_empty() {
        return Vec::new();
    }
    let mut ids = vec![peer.to_owned()];
    let parsed = p2p::iroh::parse_public_peer_addr(peer)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string())
        .or_else(|| {
            let (_, suffix) = peer.rsplit_once("/p2p/")?;
            suffix
                .split('/')
                .next()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(ToOwned::to_owned)
        });
    if let Some(id) = parsed.filter(|id| id != peer) {
        ids.push(id);
    }
    ids
}

/// The pairing observations this actor can read, as the documents a reader
/// grant names.
pub async fn pairing_reader_targets(
    access: &ConfigAccess,
) -> Result<PairingReaderTargets, PairingReadError> {
    let desired = Query::select(
        Selection::doc_id()
            .field(peer_pairing_desired::PEER_ID)
            .field(peer_pairing_desired::NODE_DID)
            .field(peer_pairing_desired::ACTOR_DID),
    );
    let applied = Query::select(
        Selection::doc_id()
            .field(peer_pairing_applied::PEER_ID)
            .field(peer_pairing_applied::NODE_DID),
    );
    let response = access
        .read(&QuerySet::new().plain(&desired).plain(&applied).render())
        .await
        .map_err(PairingReadError::Access)?;
    let targets = |collection: &'static str| -> Result<Vec<PairingReaderTarget>, PairingReadError> {
        response
            .pointer(&format!("/data/{collection}"))
            .and_then(Value::as_array)
            .ok_or(PairingReadError::Omitted(collection))?
            .iter()
            .cloned()
            .map(|row| serde_json::from_value(row).map_err(PairingReadError::Malformed))
            .collect()
    };
    Ok(PairingReaderTargets {
        desired: targets("PeerPairingDesired")?,
        applied: targets("PeerPairingApplied")?,
    })
}
