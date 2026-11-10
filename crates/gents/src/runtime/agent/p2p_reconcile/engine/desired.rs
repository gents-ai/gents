use std::collections::BTreeSet;

use anyhow::{Context, Result};

use super::super::templates::{
    equality_filter, resolve_template, DidSource, PairingFilters, Scope,
};
use super::super::{policy, templates, PairingDesired};
use super::rows::PairingStateRow;
use super::EnrollmentEndpointEntry;

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
            anyhow::bail!(
                "DataPlanePairingDesired for peer {} scopes node_did {} but this node is {}; \
                 refusing to install a data-plane replicator for a foreign DID",
                signed_endpoint.peer_id,
                row_did,
                self_did
            );
        }
    }

    let template_id = row
        .template
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .context("data-plane pairing row is missing its scope template")?;
    let template = resolve_template(template_id)
        .with_context(|| format!("unknown data-plane pairing scope template {template_id:?}"))?;
    let client_route_direction = if template.scope == Scope::ClientRoute {
        let route_id = row
            .peer_id
            .as_deref()
            .map(str::trim)
            .filter(|route_id| !route_id.is_empty())
            .context("client data-plane pairing row is missing its durable route key")?;
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
        anyhow::bail!(
            "DataPlanePairingDesired for peer {} uses template {template_id:?} but the signed \
             PeerRegistry has a blank node_did",
            signed_endpoint.peer_id
        );
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

    Ok(Some(PairingDesired {
        replicator_addresses,
        replicator_collections,
        replicator_filter,
        template_ids: BTreeSet::from([template.id.to_string()]),
    }))
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
