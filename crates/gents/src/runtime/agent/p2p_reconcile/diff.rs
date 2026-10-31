use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::templates::{to_replication_filters, PairingFilters};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingDesired {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
    #[serde(default)]
    pub replicator_collections: BTreeSet<String>,
    #[serde(default)]
    pub replicator_filter: PairingFilters,
    #[serde(default)]
    pub template_ids: BTreeSet<String>,
}

impl PairingDesired {
    pub fn has_wiring(&self) -> bool {
        !self.collections.is_empty() || !self.replicator_addresses.is_empty()
    }

    pub fn effective_replicator_collections(&self) -> &BTreeSet<String> {
        if self.replicator_collections.is_empty() {
            &self.collections
        } else {
            &self.replicator_collections
        }
    }

    pub fn uses_agent_target_template(&self) -> bool {
        self.template_ids
            .iter()
            .any(|template| template.starts_with("agent-target-"))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingActual {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
    #[serde(default)]
    pub replicator_collections: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub replicator_filters: BTreeMap<String, defra_p2p_adapter::ReplicationFilters>,
    /// Unknown filters do not cause repeated reinstalls, but cannot certify a
    /// scoped route as matching.
    #[serde(default)]
    pub replicator_filters_unobserved: BTreeSet<String>,
    /// Keep the durable route key alongside transport-reported addresses to
    /// adopt restored peers without accepting stale ticket projections.
    #[serde(default)]
    pub replicator_observed_addresses: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingApplied {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
    #[serde(default)]
    pub replicator_filter: PairingFilters,
}

impl PairingApplied {
    pub fn is_empty(&self) -> bool {
        self.collections.is_empty() && self.replicator_addresses.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOp {
    InstallCollection(String),
    TeardownCollection(String),
    InstallReplicator(String),
    TeardownReplicator(String),
}

pub fn compute_pairing_diff(desired: &PairingDesired, actual: &PairingActual) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    for c in desired.collections.difference(&actual.collections) {
        ops.push(DiffOp::InstallCollection(c.clone()));
    }
    for c in actual.collections.difference(&desired.collections) {
        ops.push(DiffOp::TeardownCollection(c.clone()));
    }
    for r in desired
        .replicator_addresses
        .difference(&actual.replicator_addresses)
    {
        ops.push(DiffOp::InstallReplicator(r.clone()));
    }
    for r in actual
        .replicator_addresses
        .difference(&desired.replicator_addresses)
    {
        ops.push(DiffOp::TeardownReplicator(r.clone()));
    }
    ops
}

pub fn compute_owned_pairing_diff(
    desired: &PairingDesired,
    actual: &PairingActual,
    applied: &PairingApplied,
) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    for c in desired.collections.difference(&actual.collections) {
        ops.push(DiffOp::InstallCollection(c.clone()));
    }
    for c in actual
        .collections
        .intersection(&applied.collections)
        .filter(|c| !desired.collections.contains(*c))
    {
        ops.push(DiffOp::TeardownCollection(c.clone()));
    }
    // A data-plane install can precede a merged control-plane layer, so compare
    // live carried collections to catch this ordering race.
    let filter_changed = desired.replicator_filter != applied.replicator_filter;
    let desired_live_filters = to_replication_filters(&desired.replicator_filter).ok();
    let desired_replicator_collections = desired.effective_replicator_collections();
    for r in applied
        .replicator_addresses
        .difference(&desired.replicator_addresses)
    {
        ops.push(DiffOp::TeardownReplicator(r.clone()));
    }
    for r in actual
        .replicator_addresses
        .intersection(&applied.replicator_addresses)
        .filter(|r| desired.replicator_addresses.contains(*r))
    {
        let collections_changed = actual
            .replicator_collections
            .get(r)
            .is_some_and(|carried| carried != desired_replicator_collections);
        let live_filter_changed = if actual.replicator_filters_unobserved.contains(r) {
            !desired.replicator_filter.is_empty()
        } else {
            actual
                .replicator_filters
                .get(r)
                .map(|filter| {
                    desired_live_filters
                        .as_ref()
                        .is_none_or(|desired| !replication_filters_equivalent(desired, filter))
                })
                .unwrap_or(!desired.replicator_filter.is_empty())
        };
        let endpoint_changed =
            actual
                .replicator_observed_addresses
                .get(r)
                .is_some_and(|observed| {
                    observed.len() != 1
                        || observed
                            .iter()
                            .any(|address| !transport_addresses_equivalent(address, r))
                });
        if filter_changed || live_filter_changed || collections_changed || endpoint_changed {
            ops.push(DiffOp::TeardownReplicator(r.clone()));
            ops.push(DiffOp::InstallReplicator(r.clone()));
        }
    }
    for r in desired
        .replicator_addresses
        .difference(&actual.replicator_addresses)
    {
        ops.push(DiffOp::InstallReplicator(r.clone()));
    }
    ops
}

pub fn owned_pairing_live_matches(
    desired: &PairingDesired,
    actual: &PairingActual,
    applied: &PairingApplied,
) -> bool {
    compute_owned_pairing_diff(desired, actual, applied).is_empty()
        && (desired.replicator_filter.is_empty()
            || desired
                .replicator_addresses
                .iter()
                .all(|address| !actual.replicator_filters_unobserved.contains(address)))
}

fn replication_filters_equivalent(
    desired: &defra_p2p_adapter::ReplicationFilters,
    live: &defra_p2p_adapter::ReplicationFilters,
) -> bool {
    desired.len() == live.len()
        && desired.iter().all(|(collection, desired_filter)| {
            live.get(collection).is_some_and(|live_filter| {
                replication_filter_equivalent(desired_filter, live_filter)
            })
        })
}

fn replication_filter_equivalent(
    left: &defra_p2p_adapter::ReplicationFilter,
    right: &defra_p2p_adapter::ReplicationFilter,
) -> bool {
    if left == right {
        return true;
    }
    matches!(
        (scalar_equality(left), scalar_equality(right)),
        (Some(left), Some(right)) if left == right
    )
}

fn scalar_equality(
    filter: &defra_p2p_adapter::ReplicationFilter,
) -> Option<(&str, &serde_json::Value)> {
    if filter.conditions.is_none() && !filter.field.is_empty() {
        return Some((&filter.field, &filter.value));
    }
    let conditions = filter.conditions.as_ref()?;
    if conditions.len() != 1 {
        return None;
    }
    let (field, operation) = conditions.iter().next()?;
    let operation = operation.as_object()?;
    if operation.len() != 1 {
        return None;
    }
    operation.get("_eq").map(|value| (field.as_str(), value))
}

fn transport_addresses_equivalent(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    if let (Ok((left_peer, left_dial)), Ok((right_peer, right_dial))) = (
        p2p::iroh::parse_public_peer_addr(left),
        p2p::iroh::parse_public_peer_addr(right),
    ) {
        if left_peer == right_peer && (left_dial.is_empty() || right_dial.is_empty()) {
            return true;
        }
    }
    match (
        super::TransportEndpoint::parse(left.to_string()),
        super::TransportEndpoint::parse(right.to_string()),
    ) {
        (Ok(left), Ok(right)) => left.equivalent_to(&right),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(xs: &[&str]) -> BTreeSet<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    fn filter(field: &str, value: &str) -> PairingFilters {
        let mut f = PairingFilters::new();
        f.insert(
            "AgentRequest".to_string(),
            super::super::templates::equality_filter(field, value),
        );
        f
    }

    #[test]
    fn live_scalar_equality_is_equivalent_to_rich_predicate_projection() {
        let pairing_filter = filter("did", "did:key:phone");
        let desired = PairingDesired {
            replicator_addresses: s(&["addr1"]),
            replicator_filter: pairing_filter.clone(),
            ..Default::default()
        };
        let actual = PairingActual {
            replicator_addresses: s(&["addr1"]),
            replicator_filters: BTreeMap::from([(
                "addr1".to_string(),
                BTreeMap::from([(
                    "AgentRequest".to_string(),
                    defra_p2p_adapter::ReplicationFilter::eq(
                        "did",
                        serde_json::json!("did:key:phone"),
                    ),
                )]),
            )]),
            ..Default::default()
        };
        let applied = PairingApplied {
            replicator_addresses: s(&["addr1"]),
            replicator_filter: pairing_filter,
            ..Default::default()
        };

        assert!(compute_owned_pairing_diff(&desired, &actual, &applied).is_empty());
    }

    #[test]
    fn live_scalar_equality_with_wrong_value_reinstalls() {
        let pairing_filter = filter("did", "did:key:phone");
        let desired = PairingDesired {
            replicator_addresses: s(&["addr1"]),
            replicator_filter: pairing_filter.clone(),
            ..Default::default()
        };
        let actual = PairingActual {
            replicator_addresses: s(&["addr1"]),
            replicator_filters: BTreeMap::from([(
                "addr1".to_string(),
                BTreeMap::from([(
                    "AgentRequest".to_string(),
                    defra_p2p_adapter::ReplicationFilter::eq(
                        "did",
                        serde_json::json!("did:key:someone-else"),
                    ),
                )]),
            )]),
            ..Default::default()
        };
        let applied = PairingApplied {
            replicator_addresses: s(&["addr1"]),
            replicator_filter: pairing_filter,
            ..Default::default()
        };

        assert_eq!(
            compute_owned_pairing_diff(&desired, &actual, &applied),
            vec![
                DiffOp::TeardownReplicator("addr1".into()),
                DiffOp::InstallReplicator("addr1".into()),
            ]
        );
    }

    #[test]
    fn identity_only_live_observation_does_not_churn_dialable_route() {
        let peer = "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
        let desired_address = format!("127.0.0.1:56091/p2p/{peer}");
        let desired = PairingDesired {
            replicator_addresses: s(&[&desired_address]),
            ..Default::default()
        };
        let actual = PairingActual {
            replicator_addresses: s(&[&desired_address]),
            replicator_observed_addresses: BTreeMap::from([(
                desired_address.clone(),
                s(&[&format!("iroh://{peer}")]),
            )]),
            ..Default::default()
        };
        let applied = PairingApplied {
            replicator_addresses: s(&[&desired_address]),
            ..Default::default()
        };

        assert!(compute_owned_pairing_diff(&desired, &actual, &applied).is_empty());
    }

    #[test]
    fn stale_dialable_live_observation_reinstalls_same_peer_route() {
        let peer = "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
        let stale_address = format!("127.0.0.1:56090/p2p/{peer}");
        let desired_address = format!("127.0.0.1:56091/p2p/{peer}");
        let desired = PairingDesired {
            replicator_addresses: s(&[&desired_address]),
            ..Default::default()
        };
        let actual = PairingActual {
            replicator_addresses: s(&[&desired_address]),
            replicator_observed_addresses: BTreeMap::from([(
                desired_address.clone(),
                s(&[&stale_address]),
            )]),
            ..Default::default()
        };
        let applied = PairingApplied {
            replicator_addresses: s(&[&desired_address]),
            ..Default::default()
        };

        assert_eq!(
            compute_owned_pairing_diff(&desired, &actual, &applied),
            vec![
                DiffOp::TeardownReplicator(desired_address.clone()),
                DiffOp::InstallReplicator(desired_address),
            ]
        );
    }

    #[test]
    fn changing_scoped_did_reinstalls_replicator() {
        let desired = PairingDesired {
            collections: BTreeSet::new(),
            replicator_addresses: s(&["addr1"]),
            replicator_filter: filter("node_did", "did:key:bob"),
            ..Default::default()
        };
        let actual = PairingActual {
            collections: BTreeSet::new(),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        let applied = PairingApplied {
            collections: BTreeSet::new(),
            replicator_addresses: s(&["addr1"]),
            replicator_filter: filter("node_did", "did:key:alice"),
        };
        assert_eq!(
            compute_owned_pairing_diff(&desired, &actual, &applied),
            vec![
                DiffOp::TeardownReplicator("addr1".into()),
                DiffOp::InstallReplicator("addr1".into()),
            ]
        );
    }

    #[test]
    fn unchanged_filter_does_not_reinstall_replicator() {
        let f = filter("node_did", "did:key:bob");
        let desired = PairingDesired {
            collections: BTreeSet::new(),
            replicator_addresses: s(&["addr1"]),
            replicator_filter: f.clone(),
            ..Default::default()
        };
        let actual = PairingActual {
            collections: BTreeSet::new(),
            replicator_addresses: s(&["addr1"]),
            replicator_filters: BTreeMap::from([(
                "addr1".to_string(),
                to_replication_filters(&f).expect("representable filter"),
            )]),
            ..Default::default()
        };
        let applied = PairingApplied {
            collections: BTreeSet::new(),
            replicator_addresses: s(&["addr1"]),
            replicator_filter: f,
        };
        assert!(compute_owned_pairing_diff(&desired, &actual, &applied).is_empty());
    }

    #[test]
    fn changed_replicator_collections_reinstall_replicator() {
        let desired = PairingDesired {
            collections: s(&["Network"]),
            replicator_addresses: s(&["addr1"]),
            replicator_collections: s(&["Network", "AgentRequest"]),
            ..Default::default()
        };
        let actual = PairingActual {
            collections: s(&["Network"]),
            replicator_addresses: s(&["addr1"]),
            replicator_collections: BTreeMap::from([("addr1".to_string(), s(&["AgentRequest"]))]),
            ..Default::default()
        };
        let applied = PairingApplied {
            collections: s(&["Network"]),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        assert_eq!(
            compute_owned_pairing_diff(&desired, &actual, &applied),
            vec![
                DiffOp::TeardownReplicator("addr1".into()),
                DiffOp::InstallReplicator("addr1".into()),
            ]
        );
    }

    #[test]
    fn matching_replicator_collections_do_not_reinstall() {
        let desired = PairingDesired {
            collections: s(&["Network"]),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        let actual = PairingActual {
            collections: s(&["Network"]),
            replicator_addresses: s(&["addr1"]),
            replicator_collections: BTreeMap::from([("addr1".to_string(), s(&["Network"]))]),
            ..Default::default()
        };
        let applied = PairingApplied {
            collections: s(&["Network"]),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        assert!(compute_owned_pairing_diff(&desired, &actual, &applied).is_empty());
    }

    #[test]
    fn unobservable_replicator_collections_do_not_reinstall() {
        let desired = PairingDesired {
            collections: s(&["Network"]),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        let actual = PairingActual {
            collections: s(&["Network"]),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        let applied = PairingApplied {
            collections: s(&["Network"]),
            replicator_addresses: s(&["addr1"]),
            ..Default::default()
        };
        assert!(compute_owned_pairing_diff(&desired, &actual, &applied).is_empty());
    }

    #[test]
    fn owned_diff_tears_down_only_applied_extras() {
        let desired = PairingDesired::default();
        let actual = PairingActual {
            collections: s(&["manual", "managed"]),
            replicator_addresses: s(&["/ip4/manual/p2p/p", "/ip4/managed/p2p/p"]),
            ..Default::default()
        };
        let applied = PairingApplied {
            collections: s(&["managed"]),
            replicator_addresses: s(&["/ip4/managed/p2p/p"]),
            ..Default::default()
        };
        assert_eq!(
            compute_owned_pairing_diff(&desired, &actual, &applied),
            vec![
                DiffOp::TeardownCollection("managed".into()),
                DiffOp::TeardownReplicator("/ip4/managed/p2p/p".into())
            ]
        );
    }
}
