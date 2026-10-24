use std::collections::{BTreeMap, BTreeSet};

use super::super::error::{P2pReconcileError, Result};
#[cfg(test)]
use super::super::templates::Delivery;
use super::super::templates::{
    admit_app_collections, equality_filter, resolve_template, DidSource, PairingFilters, Scope,
    APP_COLLECTIONS_TEMPLATE,
};
use super::super::{policy, templates, PairingDesired};
use super::rows::PairingStateRow;
use super::EnrollmentEndpointEntry;

#[cfg(test)]
pub(super) fn desired_from_pairing_row(
    row: PairingStateRow,
    local_did: &str,
) -> Result<Option<PairingDesired>> {
    let replicator_addresses = row
        .replicator_addresses
        .unwrap_or_default()
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();

    let template_id = row
        .template
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or(P2pReconcileError::PairingRowMissingScopeTemplate)?;
    let template = resolve_template(template_id).ok_or_else(|| {
        P2pReconcileError::UnknownPairingScopeTemplate {
            template_id: template_id.to_owned(),
        }
    })?;

    if template.id == APP_COLLECTIONS_TEMPLATE {
        tracing::warn!(target: "gents::agent::p2p_reconcile::engine",
            "PeerPairingDesired names the app-collections template, which is \
             data-plane-only and supplies no collections here; skipping (no wiring)"
        );
        return Ok(None);
    }

    let peer_did = row.node_did.as_deref().map(str::trim).unwrap_or_default();
    if peer_did.is_empty() && scope_requires_peer_did(&template.scope) {
        return Err(P2pReconcileError::PeerDidDependentTemplateBlank {
            template_id: template_id.to_owned(),
        });
    }
    let (requester_did, owner_node_did, direction) = if template.scope == Scope::ClientRoute {
        let route_id = row
            .peer_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(P2pReconcileError::ClientPairingRowMissingRouteKey)?;
        let direction = policy::client_route_direction(route_id)?;
        (local_did, peer_did, direction)
    } else {
        (
            peer_did,
            local_did,
            policy::PairingDirection::RuntimeToClient,
        )
    };
    let replicator_collections = if template.scope == Scope::ClientRoute {
        policy::client_route_collections(direction)
    } else {
        template.collections
    }
    .iter()
    .map(|&collection| collection.to_string())
    .collect::<BTreeSet<_>>();
    let replicator_filter =
        policy::resolve_template_filters(template, direction, requester_did, owner_node_did);

    let subscription_collections = match template.delivery {
        Delivery::Push => BTreeSet::new(),
        Delivery::Replicate => replicator_collections.clone(),
    };

    Ok(Some(PairingDesired {
        collections: subscription_collections,
        replicator_addresses,
        replicator_collections,
        replicator_filter,
        template_ids: BTreeSet::from([template.id.to_string()]),
    }))
}

pub(super) fn data_plane_desired_from_pairing_row(
    mut row: PairingStateRow,
    signed_endpoint: &EnrollmentEndpointEntry,
    self_did: &str,
) -> Result<Option<PairingDesired>> {
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
        tracing::warn!(target: "gents::agent::p2p_reconcile::engine",
            peer_id = %signed_endpoint.peer_id,
            signed_address = %signed_endpoint.address,
            row_addresses = ?row_addresses,
            "DataPlanePairingDesired address did not match signed PeerEndpoint; using signed endpoint address"
        );
    }

    if let Some(row_did) = row
        .node_did
        .as_deref()
        .map(str::trim)
        .filter(|did| !did.is_empty())
    {
        if row_did != self_did {
            return Err(P2pReconcileError::DataPlaneForeignDid {
                peer_id: signed_endpoint.peer_id.clone(),
                row_did: row_did.to_owned(),
                self_did: self_did.to_owned(),
            });
        }
    }

    let template_id = row
        .template
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or(P2pReconcileError::DataPlanePairingRowMissingScopeTemplate)?;
    let template = resolve_template(template_id).ok_or_else(|| {
        P2pReconcileError::UnknownDataPlaneScopeTemplate {
            template_id: template_id.to_owned(),
        }
    })?;
    let client_route_direction = if template.scope == Scope::ClientRoute {
        let route_id = row
            .peer_id
            .as_deref()
            .map(str::trim)
            .filter(|route_id| !route_id.is_empty())
            .ok_or(P2pReconcileError::ClientDataPlanePairingRowMissingRouteKey)?;
        match policy::client_route_direction(route_id) {
            Ok(direction) => direction,
            // Enrollment base routes are keyed by the member peer, not a
            // directional suffix.
            Err(_) if row.source.as_deref() == Some("enrollment") => {
                policy::PairingDirection::RuntimeToClient
            }
            Err(error) => return Err(error),
        }
    } else {
        policy::PairingDirection::RuntimeToClient
    };
    let peer_did = signed_endpoint.node_did.trim();
    if data_plane_scope_requires_signed_peer_did(&template.scope) && peer_did.is_empty() {
        return Err(P2pReconcileError::DataPlaneBlankPeerDid {
            peer_id: signed_endpoint.peer_id.clone(),
            template_id: template_id.to_owned(),
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

    let (replicator_collections, subscription_collections): (BTreeSet<String>, BTreeSet<String>) =
        if template.id == APP_COLLECTIONS_TEMPLATE {
            let requested = row
                .collections
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect::<BTreeSet<_>>();
            let Some(row_cols) = admit_app_collections(requested.clone()) else {
                tracing::warn!(target: "gents::agent::p2p_reconcile::engine",
                    peer_id = %signed_endpoint.peer_id,
                    collections = ?requested,
                    "app-collections DataPlanePairingDesired is empty or overlaps the protocol \
                     catalog; skipping this data-plane layer (control pairing unaffected)"
                );
                return Ok(None);
            };
            (row_cols.clone(), row_cols)
        } else {
            let collections = if template.scope == Scope::ClientRoute {
                policy::client_route_collections(client_route_direction)
            } else {
                template.collections
            };
            let cols = collections
                .iter()
                .map(|&c| c.to_string())
                .collect::<BTreeSet<_>>();
            (cols, BTreeSet::new())
        };

    let filter_collections = replicator_collections
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let replicator_filter = data_plane_scope_filter(
        &template.scope,
        &filter_collections,
        peer_did,
        self_did,
        client_route_direction,
    );

    Ok(Some(PairingDesired {
        collections: subscription_collections,
        replicator_addresses,
        replicator_collections,
        replicator_filter,
        template_ids: BTreeSet::from([template.id.to_string()]),
    }))
}

#[cfg(test)]
fn scope_requires_peer_did(scope: &Scope) -> bool {
    match scope {
        Scope::PeerDid { .. } => true,
        Scope::Unscoped => false,
        Scope::PerCollection(rules) => rules
            .iter()
            .any(|rule| matches!(rule.source, DidSource::PeerDid)),
        Scope::ClientRoute => true,
    }
}

fn data_plane_scope_requires_signed_peer_did(scope: &Scope) -> bool {
    match scope {
        Scope::PeerDid { .. } | Scope::Unscoped => false,
        Scope::PerCollection(rules) => rules
            .iter()
            .any(|rule| matches!(rule.source, DidSource::PeerDid)),
        Scope::ClientRoute => true,
    }
}

fn data_plane_scope_filter(
    scope: &Scope,
    collections: &[&str],
    signed_peer_did: &str,
    local_did: &str,
    client_route_direction: policy::PairingDirection,
) -> PairingFilters {
    match scope {
        Scope::PeerDid { field } => collections
            .iter()
            .map(|&col| (col.to_string(), equality_filter(*field, local_did)))
            .collect(),
        Scope::Unscoped => BTreeMap::new(),
        Scope::PerCollection(rules) => rules
            .iter()
            .map(|rule| {
                let value = match rule.source {
                    DidSource::LocalDid => local_did,
                    DidSource::PeerDid => signed_peer_did,
                    DidSource::HomeDid => local_did,
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
    let data_plane = data_plane.map(|mut desired| {
        if !desired.template_ids.contains(APP_COLLECTIONS_TEMPLATE) {
            desired.collections.clear();
        }
        desired
    });
    match (base, data_plane) {
        (None, None) => None,
        (Some(desired), None) | (None, Some(desired)) => Some(desired),
        (Some(mut left), Some(right)) => {
            left.collections.extend(right.collections);
            // The data-plane layer comes from the verified PeerEndpoint.
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
