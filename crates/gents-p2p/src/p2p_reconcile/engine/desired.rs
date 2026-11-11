use std::collections::BTreeSet;

use super::super::templates::{
    equality_filter, resolve_template, DidSource, PairingFilters, Scope,
};
use super::super::{policy, templates, PairingDesired};
use super::rows::PairingStateRow;
use super::EnrollmentEndpointEntry;

#[derive(Debug, thiserror::Error)]
pub(super) enum DataPlaneRefusal {
    #[error(
        "DataPlanePairingDesired for peer {peer_id} scopes node_did {row_did} but this node is \
         {self_did}; refusing to install a data-plane replicator for a foreign DID"
    )]
    ForeignScope {
        peer_id: String,
        row_did: String,
        self_did: String,
    },
    #[error("data-plane pairing row is missing its scope template")]
    MissingTemplate,
    #[error("unknown data-plane pairing scope template {0:?}")]
    UnknownTemplate(String),
    #[error("client data-plane pairing row is missing its durable route key")]
    MissingRouteKey,
    #[error(transparent)]
    UndirectedRouteKey(anyhow::Error),
    #[error(
        "DataPlanePairingDesired for peer {peer_id} uses template {template_id:?} but the signed \
         PeerRegistry has a blank node_did"
    )]
    UnsignedPeer {
        peer_id: String,
        template_id: String,
    },
}

pub(super) fn data_plane_desired_from_pairing_row(
    mut row: PairingStateRow,
    signed_endpoint: &EnrollmentEndpointEntry,
    self_did: &str,
) -> Result<PairingDesired, DataPlaneRefusal> {
    let row_addresses = row
        .replicator_addresses
        .as_ref()
        .map(|values| {
            values
                .iter()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    if !row_addresses.is_empty()
        && (row_addresses.len() != 1 || !row_addresses.contains(signed_endpoint.address.as_str()))
    {
        tracing::warn!(target: "gents::runtime::agent::p2p_reconcile::engine",
            peer_id = %signed_endpoint.peer_id,
            signed_address = %signed_endpoint.address,
            row_addresses = ?row_addresses,
            "DataPlanePairingDesired address did not match signed PeerRegistry; using signed endpoint address"
        );
    }

    if let Some(row_did) = row
        .node_did
        .as_deref()
        .map(str::trim)
        .filter(|did| !did.is_empty())
    {
        if row_did != self_did {
            return Err(DataPlaneRefusal::ForeignScope {
                peer_id: signed_endpoint.peer_id.clone(),
                row_did: row_did.to_string(),
                self_did: self_did.to_string(),
            });
        }
    }

    let template_id = row
        .template
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or(DataPlaneRefusal::MissingTemplate)?;
    let template = resolve_template(template_id)
        .ok_or_else(|| DataPlaneRefusal::UnknownTemplate(template_id.to_string()))?;
    let client_route_direction = if template.scope == Scope::ClientRoute {
        let route_id = row
            .peer_id
            .as_deref()
            .map(str::trim)
            .filter(|route_id| !route_id.is_empty())
            .ok_or(DataPlaneRefusal::MissingRouteKey)?;
        match policy::client_route_direction(route_id) {
            Ok(direction) => direction,
            // Enrollment base routes are keyed by the member peer, not a
            // directional suffix.
            Err(_) if row.source.as_deref() == Some("enrollment") => {
                policy::PairingDirection::RuntimeToClient
            }
            Err(error) => return Err(DataPlaneRefusal::UndirectedRouteKey(error)),
        }
    } else {
        policy::PairingDirection::RuntimeToClient
    };
    let peer_did = signed_endpoint.node_did.trim();
    if data_plane_scope_requires_signed_peer_did(&template.scope) && peer_did.is_empty() {
        return Err(DataPlaneRefusal::UnsignedPeer {
            peer_id: signed_endpoint.peer_id.clone(),
            template_id: template_id.to_string(),
        });
    }

    row.replicator_addresses = Some(vec![signed_endpoint.address.clone()]);
    let replicator_addresses = row
        .replicator_addresses
        .unwrap_or_default()
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();

    let collections = if template.scope == Scope::ClientRoute {
        policy::client_route_collections(client_route_direction)
    } else {
        template.collections
    };
    let replicator_collections = collections
        .iter()
        .map(|&c| c.to_string())
        .collect::<BTreeSet<_>>();

    let replicator_filter =
        data_plane_scope_filter(&template.scope, peer_did, self_did, client_route_direction);

    Ok(PairingDesired {
        replicator_addresses,
        replicator_collections,
        replicator_filter,
        template_ids: BTreeSet::from([template.id.to_string()]),
    })
}

fn data_plane_scope_requires_signed_peer_did(scope: &Scope) -> bool {
    matches!(scope, Scope::ClientRoute)
}

fn data_plane_scope_filter(
    scope: &Scope,
    signed_peer_did: &str,
    local_did: &str,
    client_route_direction: policy::PairingDirection,
) -> PairingFilters {
    match scope {
        Scope::PerCollection(rules) => rules
            .iter()
            .map(|rule| {
                let value = match rule.source {
                    DidSource::LocalDid | DidSource::HomeDid => local_did,
                };
                (
                    rule.collection.to_string(),
                    equality_filter(rule.field, value),
                )
            })
            .collect(),
        Scope::ClientRoute => {
            let (requester_did, owner_node_did) = match client_route_direction {
                policy::PairingDirection::ClientToRuntime => (local_did, signed_peer_did),
                policy::PairingDirection::RuntimeToClient => (signed_peer_did, local_did),
            };
            policy::resolve_template_filters(
                resolve_template(templates::CLIENT_TEMPLATE).expect("client template"),
                client_route_direction,
                requester_did,
                owner_node_did,
            )
        }
    }
}

pub fn merge_layered_desired(
    local_did: &str,
    peer_did: &str,
    base: Option<PairingDesired>,
    data_plane: Option<PairingDesired>,
) -> Option<PairingDesired> {
    let base = if peer_did == local_did { None } else { base };
    match (base, data_plane) {
        (None, None) => None,
        (Some(desired), None) | (None, Some(desired)) => Some(desired),
        (Some(mut left), Some(right)) => {
            // The data-plane layer comes from the verified PeerRegistry.
            if !right.replicator_addresses.is_empty() {
                left.replicator_addresses = right.replicator_addresses;
            }
            left.replicator_collections
                .extend(right.replicator_collections);
            left.replicator_filter.extend(right.replicator_filter);
            left.template_ids.extend(right.template_ids);
            Some(left)
        }
    }
}
