//! Observation and mutation of the live remote P2P topology.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use p2p::iroh::parse_public_peer_addr;

use super::super::{
    DiffOp, PairingActual, PairingDesired, RemoteP2pAdmin, RemoteP2pAdminError, RemoteReplicator,
    TransportEndpoint,
};

/// Remove replicators already aimed at one transport endpoint when no durable
/// applied owner exists for the replacement route. This is the narrow upgrade
/// bridge from legacy unsuffixed mobile pairings: the endpoint match prevents
/// one runtime route from tearing down any other route.
pub async fn teardown_unowned_replicators_at_endpoint(
    admin: &dyn RemoteP2pAdmin,
    address: &str,
    expected_collections: &[String],
) -> Result<usize> {
    let endpoint = TransportEndpoint::parse(address.to_string())?;
    let expected_collections = expected_collections.iter().collect::<BTreeSet<_>>();
    let replicators = admin
        .list_replicators()
        .await
        .context("list replicators for client-route upgrade")?;
    let mut removed = 0;
    for replicator in replicators {
        let matches_endpoint = replicator.address.as_deref().is_some_and(|candidate| {
            candidate == address
                || TransportEndpoint::parse(candidate.to_string())
                    .is_ok_and(|candidate| candidate.peer_id() == endpoint.peer_id())
        }) || replicator.id.as_deref() == Some(endpoint.peer_id());
        if !matches_endpoint {
            continue;
        }
        let mut resolved_collections = BTreeSet::new();
        for collection_id in &replicator.collections {
            let collection = admin
                .resolve_collection_name(collection_id)
                .await
                .with_context(|| format!("resolve unowned replicator collection {collection_id}"))?
                .unwrap_or_else(|| collection_id.clone());
            resolved_collections.insert(collection);
        }
        if resolved_collections.iter().collect::<BTreeSet<_>>() != expected_collections {
            continue;
        }
        let id = replicator.id.as_deref().unwrap_or(endpoint.peer_id());
        admin
            .delete_replicator(id, &replicator.collections)
            .await
            .with_context(|| format!("teardown unowned client replicator {id}"))?;
        removed += 1;
    }
    Ok(removed)
}

/// Authoritatively remove every live replicator at an endpoint owned by a
/// route being deleted. Unlike the legacy-upgrade helper above, ownership has
/// already been established by the caller, so mutable collection/filter drift
/// must not protect the stale route from teardown.
pub async fn teardown_owned_replicators_at_endpoint(
    admin: &dyn RemoteP2pAdmin,
    address: &str,
) -> Result<usize> {
    let endpoint = TransportEndpoint::parse(address.to_string())?;
    let matches_endpoint = |replicator: &RemoteReplicator| {
        replicator.address.as_deref().is_some_and(|candidate| {
            candidate == address
                || TransportEndpoint::parse(candidate.to_string())
                    .is_ok_and(|candidate| candidate.peer_id() == endpoint.peer_id())
        }) || replicator.id.as_deref() == Some(endpoint.peer_id())
    };

    let replicators = admin
        .list_replicators()
        .await
        .context("list replicators for owned route teardown")?;
    let mut removed = 0;
    for replicator in replicators
        .iter()
        .filter(|replicator| matches_endpoint(replicator))
    {
        let id = replicator.id.as_deref().unwrap_or(endpoint.peer_id());
        match admin.delete_replicator(id, &replicator.collections).await {
            Ok(()) | Err(RemoteP2pAdminError::RemoteNotFound(_)) => removed += 1,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("teardown owned client replicator {id}"));
            }
        }
    }

    let remaining = admin
        .list_replicators()
        .await
        .context("verify owned route teardown")?;
    if remaining.iter().any(matches_endpoint) {
        anyhow::bail!(
            "owned client replicator at transport endpoint {} survived teardown",
            endpoint.peer_id()
        );
    }
    Ok(removed)
}

pub(super) struct ActualSnapshot {
    pub(super) state: PairingActual,
    pub(super) replicator_ids_by_addr: BTreeMap<String, String>,
    pub(super) replicator_collections_by_addr: BTreeMap<String, Vec<String>>,
}

pub(super) async fn read_actual(
    admin: &dyn RemoteP2pAdmin,
    applied_addresses: &BTreeSet<String>,
) -> Result<ActualSnapshot> {
    let mut collections = BTreeSet::new();
    for id in admin
        .list_p2p_collections()
        .await
        .context("list remote P2P collections")?
    {
        match admin
            .resolve_collection_name(&id)
            .await
            .with_context(|| format!("resolve collection name for id {id}"))?
        {
            Some(name) => {
                collections.insert(name);
            }
            None => {
                tracing::warn!(
                    target: "gents::agent::p2p_reconcile::engine",
                    collection_id = %id,
                    "remote P2P collection id has no local name; keeping the id in \
                     the actual set"
                );
                collections.insert(id);
            }
        }
    }
    let remote_replicators = admin
        .list_replicators()
        .await
        .context("list remote P2P replicators")?;
    let mut replicator_addresses = BTreeSet::new();
    let mut replicator_ids_by_addr: BTreeMap<String, String> = BTreeMap::new();
    let mut replicator_collections_by_addr: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut replicator_filters_by_addr: BTreeMap<String, defra_p2p_adapter::ReplicationFilters> =
        BTreeMap::new();
    let mut replicator_filters_unobserved = BTreeSet::new();
    let mut replicator_observed_addresses_by_addr: BTreeMap<String, BTreeSet<String>> =
        BTreeMap::new();
    for replicator in remote_replicators {
        let observed_address = replicator.address.clone();
        let address = canonical_replicator_address(&replicator, applied_addresses);
        let Some(address) = address else {
            tracing::warn!(
                target: "gents::agent::p2p_reconcile::engine",
                "remote replicator has neither a peer id nor an address; ignoring it"
            );
            continue;
        };
        replicator_addresses.insert(address.clone());
        if let Some(observed_address) = observed_address {
            replicator_observed_addresses_by_addr
                .entry(address.clone())
                .or_default()
                .insert(observed_address);
        }
        if let Some(id) = replicator.id {
            replicator_ids_by_addr
                .entry(address.clone())
                .and_modify(|existing| {
                    if id.as_str() < existing.as_str() {
                        existing.clone_from(&id);
                    }
                })
                .or_insert(id);
        }
        replicator_collections_by_addr
            .entry(address.clone())
            .or_insert_with(Vec::new)
            .extend(replicator.collections);
        let Some(filters) = replicator.filters else {
            replicator_filters_unobserved.insert(address);
            continue;
        };
        let mut named_filters = defra_p2p_adapter::ReplicationFilters::new();
        for (collection_id, filter) in filters {
            let collection = admin
                .resolve_collection_name(&collection_id)
                .await
                .with_context(|| format!("resolve replicator filter collection {collection_id}"))?
                .unwrap_or(collection_id);
            named_filters.insert(collection, filter);
        }
        replicator_filters_by_addr
            .entry(address)
            .or_default()
            .extend(named_filters);
    }

    let mut replicator_collections = BTreeMap::new();
    for (address, ids) in &replicator_collections_by_addr {
        let mut names = BTreeSet::new();
        for id in ids {
            match admin
                .resolve_collection_name(id)
                .await
                .with_context(|| format!("resolve replicator collection name for id {id}"))?
            {
                Some(name) => {
                    names.insert(name);
                }
                None => {
                    tracing::debug!(
                        target: "gents::agent::p2p_reconcile::engine",
                        collection = %id,
                        address = %address,
                        "replicator collection token has no local name; keeping it raw"
                    );
                    names.insert(id.clone());
                }
            }
        }
        replicator_collections.insert(address.clone(), names);
    }

    Ok(ActualSnapshot {
        state: PairingActual {
            collections,
            replicator_addresses,
            replicator_collections,
            replicator_filters: replicator_filters_by_addr,
            replicator_filters_unobserved,
            replicator_observed_addresses: replicator_observed_addresses_by_addr,
        },
        replicator_ids_by_addr,
        replicator_collections_by_addr,
    })
}

pub(super) fn canonical_replicator_address(
    replicator: &RemoteReplicator,
    applied_addresses: &BTreeSet<String>,
) -> Option<String> {
    if let Some(id) = replicator.id.as_deref() {
        let mut matches = applied_addresses.iter().filter(|address| {
            parse_public_peer_addr(address)
                .map(|(peer_id, _)| peer_id.as_str() == id)
                .unwrap_or(false)
        });
        let only_match = matches.next();
        if only_match.is_some() && matches.next().is_none() {
            return only_match.cloned();
        }
    }
    replicator.address.clone().or_else(|| replicator.id.clone())
}

pub(super) async fn apply_op(
    admin: &dyn RemoteP2pAdmin,
    op: &DiffOp,
    desired: &PairingDesired,
    actual: &ActualSnapshot,
) -> Result<()> {
    match op {
        DiffOp::InstallCollection(collection) => admin
            .add_p2p_collections(std::slice::from_ref(collection))
            .await
            .with_context(|| format!("install P2P collection {collection}")),
        DiffOp::TeardownCollection(collection) => admin
            .delete_p2p_collections(std::slice::from_ref(collection))
            .await
            .with_context(|| format!("teardown P2P collection {collection}")),
        DiffOp::InstallReplicator(address) => {
            let addresses = vec![address.clone()];
            let collections = desired
                .effective_replicator_collections()
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            admin
                .add_replicator(&addresses, &collections, &desired.replicator_filter)
                .await
                .with_context(|| format!("install P2P replicator {address}"))?;
            if desired.uses_subagent_template() {
                tracing::debug!(
                    target: "gents::agent::p2p_reconcile::engine",
                    address = %address,
                    templates = ?desired.template_ids,
                    "subagent pairing replicator installed with initial full replay"
                );
            }
            Ok(())
        }
        DiffOp::TeardownReplicator(address) => {
            let parsed_peer_id = parse_public_peer_addr(address)
                .ok()
                .map(|(peer_id, _)| peer_id.to_string());
            let id = actual
                .replicator_ids_by_addr
                .get(address)
                .or(parsed_peer_id.as_ref())
                .map(String::as_str)
                .unwrap_or(address.as_str());
            let collections = actual
                .replicator_collections_by_addr
                .get(address)
                .cloned()
                .filter(|collections| !collections.is_empty())
                .unwrap_or_else(|| {
                    desired
                        .effective_replicator_collections()
                        .iter()
                        .cloned()
                        .collect()
                });
            match admin.delete_replicator(id, &collections).await {
                Ok(()) | Err(RemoteP2pAdminError::RemoteNotFound(_)) => Ok(()),
                Err(error) => {
                    Err(error).with_context(|| format!("teardown P2P replicator {address}"))
                }
            }
        }
    }
}

pub(super) async fn replay_replicator_after_reconnect(
    admin: &dyn RemoteP2pAdmin,
    address: &str,
    desired: &PairingDesired,
    actual: &ActualSnapshot,
) -> Result<()> {
    let id = actual
        .replicator_ids_by_addr
        .get(address)
        .map(String::as_str)
        .unwrap_or(address);
    let old_collections = actual
        .replicator_collections_by_addr
        .get(address)
        .cloned()
        .filter(|collections| !collections.is_empty())
        .unwrap_or_else(|| {
            desired
                .effective_replicator_collections()
                .iter()
                .cloned()
                .collect()
        });
    admin
        .delete_replicator(id, &old_collections)
        .await
        .with_context(|| format!("remove P2P replicator {address} for reconnect replay"))?;

    let addresses = vec![address.to_string()];
    let collections = desired
        .effective_replicator_collections()
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    admin
        .add_replicator(&addresses, &collections, &desired.replicator_filter)
        .await
        .with_context(|| format!("reinstall P2P replicator {address} for reconnect replay"))?;
    Ok(())
}
